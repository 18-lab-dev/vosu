//! Reuse the package validator rather than implement a second capability grammar.
use super::{Catalog, CatalogEntry};
use crate::manifest::{
    Capability, HostSupport, OverlayManifest, PackageKind, PackageManifest, PluginManifest,
    Registration, SharedManifest, ToolManifest, ToolUi, ValidationContext, ValidationError,
    valid_native_target,
};
use semver::Version;
use std::collections::{BTreeMap, BTreeSet};

fn error(rule: &'static str, pointer: impl Into<String>) -> ValidationError {
    ValidationError::new(rule, pointer)
}
fn prefix(mut e: ValidationError, path: &str) -> ValidationError {
    e.pointer.insert_str(0, path);
    e
}
fn https(value: &str, pointer: &str) -> Result<(), ValidationError> {
    let url = url::Url::parse(value).map_err(|_| error("catalog.https", pointer))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || value.split_once("://").is_some_and(|(_, rest)| {
            rest.split(['/', '?', '#'])
                .next()
                .is_some_and(|authority| authority.contains('@'))
        })
    {
        return Err(error("catalog.https", pointer));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntrySupport {
    pub host: HostSupport,
    pub target: bool,
}
impl EntrySupport {
    pub fn supported(self) -> bool {
        self.host == HostSupport::Supported && self.target
    }
}

impl Catalog {
    /// Parse strict wire shapes and intrinsic semantics; does not attest registration or assets.
    pub fn parse(text: &str) -> Result<Self, ValidationError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|_| error("catalog.shape", ""))?;
        // Check key presence as well as decoded values: null cannot disguise a forbidden field.
        for (array, plugin, overlay) in [
            ("plugins", true, false),
            ("tools", false, false),
            ("overlays", false, true),
        ] {
            if let Some(entries) = value.get(array).and_then(serde_json::Value::as_array) {
                for (i, entry) in entries.iter().enumerate() {
                    for key in [
                        "runtime",
                        "matcher",
                        "actions",
                        "resource_kinds",
                        "covers",
                        "resolution",
                        "preview",
                    ] {
                        if entry.get(key).is_some()
                            && !match key {
                                "resolution" | "preview" => overlay,
                                _ => plugin,
                            }
                        {
                            return Err(error("catalog.kind-field", format!("/{array}/{i}/{key}")));
                        }
                        if entry.get(key).is_some_and(serde_json::Value::is_null)
                            && key != "preview"
                        {
                            return Err(error("catalog.shape", format!("/{array}/{i}/{key}")));
                        }
                    }
                    for list in ["provides", "requires", "optional"] {
                        if let Some(caps) = entry.get(list).and_then(serde_json::Value::as_array) {
                            for (j, cap) in caps.iter().enumerate() {
                                let whole = plugin
                                    && list == "provides"
                                    && cap
                                        .get("id")
                                        .and_then(serde_json::Value::as_str)
                                        .and_then(|id| id.strip_prefix("field:"))
                                        == entry.get("id").and_then(serde_json::Value::as_str);
                                for key in ["schema", "schema_version", "lanes"] {
                                    if cap.get(key).is_some() && (key == "schema" || !whole) {
                                        return Err(error(
                                            "catalog.schema-field",
                                            format!("/{array}/{i}/{list}/{j}/{key}"),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let catalog: Self =
            serde_json::from_value(value).map_err(|_| error("catalog.shape", ""))?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn entries(&self) -> impl Iterator<Item = (PackageKind, &CatalogEntry)> {
        self.plugins
            .iter()
            .map(|e| (PackageKind::Plugin, e))
            .chain(self.tools.iter().map(|e| (PackageKind::Tool, e)))
            .chain(self.overlays.iter().map(|e| (PackageKind::Overlay, e)))
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.catalog_version != 2 {
            return Err(error("catalog.version", "/catalog_version"));
        }
        if self.name.trim().is_empty() {
            return Err(error("catalog.name", "/name"));
        }
        for (array, kind, entries) in [
            ("plugins", PackageKind::Plugin, &self.plugins),
            ("tools", PackageKind::Tool, &self.tools),
            ("overlays", PackageKind::Overlay, &self.overlays),
        ] {
            let mut ids = BTreeSet::new();
            let mut claims = BTreeMap::new();
            for (i, entry) in entries.iter().enumerate() {
                let path = format!("/{array}/{i}");
                if !ids.insert(&entry.id) {
                    return Err(error("catalog.duplicate-id", format!("{path}/id")));
                }
                entry.validate(kind).map_err(|e| prefix(e, &path))?;
                for (j, former) in entry.replaces.iter().enumerate() {
                    if claims.insert(former, &entry.id).is_some()
                        || entries.iter().any(|e| e.id == *former)
                    {
                        return Err(error("catalog.replaces", format!("{path}/replaces/{j}")));
                    }
                }
            }
        }
        Ok(())
    }

    /// Caller supplies the complete enabled-source/bundled/installed set and exact official IDs.
    /// This catalog is added atomically to that set, allowing sibling references but not collisions.
    pub fn validate_context(
        &self,
        context: &ValidationContext<'_>,
        official_ids: &[&str],
    ) -> Result<(), ValidationError> {
        self.validate()?;
        let mut registrations = context.registrations.to_vec();
        registrations.extend(self.entries().map(|(kind, entry)| Registration {
            kind,
            id: &entry.id,
            source: context.source,
            official: official_ids.contains(&entry.id.as_str()),
            builtin: false,
            replaces: &entry.replaces,
        }));
        for (array, kind, entries) in [
            ("plugins", PackageKind::Plugin, &self.plugins),
            ("tools", PackageKind::Tool, &self.tools),
            ("overlays", PackageKind::Overlay, &self.overlays),
        ] {
            for (i, entry) in entries.iter().enumerate() {
                let local = ValidationContext {
                    application_version: context.application_version,
                    source: context.source,
                    official: official_ids.contains(&entry.id.as_str()),
                    registrations: &registrations,
                    namespace_revisions: context.namespace_revisions,
                };
                entry
                    .package_view(kind)?
                    .validate_context(&local)
                    .map_err(|e| prefix(e, &format!("/{array}/{i}")))?;
            }
        }
        Ok(())
    }
}
fn array_name(kind: PackageKind) -> &'static str {
    match kind {
        PackageKind::Plugin => "plugins",
        PackageKind::Tool => "tools",
        PackageKind::Overlay => "overlays",
    }
}

impl CatalogEntry {
    pub fn support(
        &self,
        kind: PackageKind,
        application: &Version,
        target: &str,
    ) -> Result<EntrySupport, ValidationError> {
        self.validate(kind)?;
        let host =
            semver::VersionReq::parse(&self.host).map_err(|_| error("manifest.host", "/host"))?;
        Ok(EntrySupport {
            host: if host.matches(application) {
                HostSupport::Supported
            } else {
                HostSupport::Unsupported
            },
            target: self.packages.contains_key(if kind == PackageKind::Plugin {
                target
            } else {
                "any"
            }),
        })
    }

    pub fn validate(&self, kind: PackageKind) -> Result<(), ValidationError> {
        if kind != PackageKind::Plugin
            && (self.runtime.is_some()
                || self.matcher.is_some()
                || self.actions.is_some()
                || self.resource_kinds.is_some()
                || self.covers.is_some())
        {
            return Err(error("catalog.kind-field", ""));
        }
        if kind != PackageKind::Overlay && (self.resolution.is_some() || self.preview.is_some()) {
            return Err(error("catalog.kind-field", "/resolution"));
        }
        for (key, value) in [
            ("icon_url", &self.icon_url),
            ("banner_url", &self.banner_url),
            ("preview", &self.preview),
        ] {
            if let Some(value) = value {
                https(value, &format!("/{key}"))?;
            }
        }
        if kind != PackageKind::Plugin
            && (self.packages.len() != 1 || !self.packages.contains_key("any"))
        {
            return Err(error("catalog.targets", "/packages"));
        }
        for (target, package) in &self.packages {
            let path = format!("/packages/{target}");
            if kind == PackageKind::Plugin && !valid_native_target(target) {
                return Err(error("catalog.target", path));
            }
            https(&package.url, &format!("{path}/url"))?;
            if package.sha256.len() != 64 || !package.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(error("catalog.sha256", format!("{path}/sha256")));
            }
            if package.size == 0 {
                return Err(error("catalog.size", format!("{path}/size")));
            }
        }
        for (list, caps) in [("requires", &self.requires), ("optional", &self.optional)] {
            for (i, cap) in caps.iter().enumerate() {
                if cap.schema.is_some() {
                    return Err(error("catalog.schema-field", format!("/{list}/{i}/schema")));
                }
            }
        }
        for (i, cap) in self.provides.iter().enumerate() {
            let path = format!("/provides/{i}");
            if cap.id.strip_prefix("field:") == Some(self.id.as_str())
                && kind == PackageKind::Plugin
            {
                let revision = cap.schema_version.as_ref().ok_or_else(|| {
                    error("catalog.schema-version", format!("{path}/schema_version"))
                })?;
                if Version::parse(revision).ok() != Version::parse(&cap.version).ok()
                    || Version::parse(revision).is_err()
                {
                    return Err(error(
                        "catalog.schema-version",
                        format!("{path}/schema_version"),
                    ));
                }
                let lanes = cap
                    .lanes
                    .as_ref()
                    .ok_or_else(|| error("catalog.lanes", format!("{path}/lanes")))?;
                crate::manifest::schema::validate_records(lanes)
                    .map_err(|_| error("catalog.lanes", format!("{path}/lanes")))?;
            } else if cap.schema_version.is_some() || cap.lanes.is_some() {
                return Err(error("catalog.schema-field", path));
            }
        }
        self.package_view(kind)?.validate()
    }

    // Synthetic package-local placeholders only adapt the shared validator. They never attest
    // package assets or leak into the wire model/comparison. Catalog has no ABI/UI/schema paths.
    pub(super) fn package_view(
        &self,
        kind: PackageKind,
    ) -> Result<PackageManifest, ValidationError> {
        let shared = SharedManifest {
            manifest_version: 1,
            kind: array_name(kind).trim_end_matches('s').to_owned(),
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            host: self.host.clone(),
            description: self.description.clone(),
            author: self.author.clone(),
            homepage: self.homepage.clone(),
            icon: None,
            banner: None,
            replaces: self.replaces.clone(),
            provides: self
                .provides
                .iter()
                .map(|c| Capability {
                    id: c.id.clone(),
                    version: c.version.clone(),
                    schema: (kind == PackageKind::Plugin
                        && c.id.strip_prefix("field:") == Some(self.id.as_str()))
                    .then(|| "namespace.schema.json".to_owned()),
                })
                .collect(),
            requires: self.requires.clone(),
            optional: self.optional.clone(),
        };
        Ok(match kind {
            PackageKind::Tool => PackageManifest::Tool(ToolManifest {
                shared,
                ui: ToolUi {
                    root: "ui".to_owned(),
                    entry: "index.html".to_owned(),
                },
            }),
            PackageKind::Overlay => PackageManifest::Overlay(OverlayManifest {
                shared,
                resolution: self.resolution,
            }),
            PackageKind::Plugin => PackageManifest::Plugin(PluginManifest {
                manifest_version: 1,
                kind: shared.kind,
                id: shared.id,
                name: shared.name,
                version: shared.version,
                host: shared.host,
                description: shared.description,
                author: shared.author,
                homepage: shared.homepage,
                icon: None,
                banner: None,
                replaces: shared.replaces,
                abi_version: 1,
                runtime: self
                    .runtime
                    .ok_or_else(|| error("catalog.runtime", "/runtime"))?,
                concurrent_dispatch: false,
                settings_schema: None,
                root_slots: Vec::new(),
                targets: BTreeMap::new(),
                covers: self.covers.clone().unwrap_or_default(),
                matcher: self.matcher.clone(),
                resource_kinds: self.resource_kinds.clone().unwrap_or_default(),
                actions: self.actions.clone().unwrap_or_default(),
                provides: shared.provides,
                requires: shared.requires,
                optional: shared.optional,
            }),
        })
    }
}
