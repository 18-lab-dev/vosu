//! Plugin-only rules shared by the unified package view and existing host entry point.
use super::{
    ActionDeclaration, Capability, MatchTable, ResourceKind, Target, ValidationError,
    shared::{metadata, unique, validate_replaces},
};
use crate::capability::{CapabilityId, covers, parse_capability};
use serde::Deserialize;

/// `runtime = "in-process" | "process"` (§7.1's request-only field; the
/// host's trust decision, not validated here, lives in the host's registry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    InProcess,
    Process,
}

/// The scalar and table forms share a normalized `PluginManifest` whose
/// SDK-facing `runtime` field remains a `Runtime`.
#[derive(Deserialize)]
#[serde(untagged)]
enum RuntimeRequest {
    Mode(Runtime),
    Options(RuntimeOptions),
}

/// `[runtime]`: the requested execution mode plus platform-specific options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeOptions {
    mode: Runtime,
    linux: Option<LinuxRuntime>,
}

/// `[runtime.linux]`: accepted only for process plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxRuntime {
    pub user_namespaces: bool,
}

// The two views use one list of common fields so validation and deserialization
// cannot drift. Only the wire view distinguishes the scalar and table forms.
macro_rules! plugin_manifest {
    ($( $(#[$attr:meta])* $name:ident: $ty:ty, )*) => {
        /// Plugin-specific typed view of the unified package document.
        #[derive(Debug, Clone, PartialEq, Deserialize)]
        #[serde(from = "PluginManifestWire")]
        pub struct PluginManifest {
            $(pub $name: $ty,)*
            pub runtime: Runtime,
            pub linux_runtime: Option<LinuxRuntime>,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct PluginManifestWire {
            $($(#[$attr])* $name: $ty,)*
            runtime: RuntimeRequest,
        }

        impl From<PluginManifestWire> for PluginManifest {
            fn from(wire: PluginManifestWire) -> Self {
                let PluginManifestWire { runtime, $($name,)* } = wire;
                let (runtime, linux_runtime) = match runtime {
                    RuntimeRequest::Mode(runtime) => (runtime, None),
                    RuntimeRequest::Options(options) => (options.mode, options.linux),
                };
                Self { runtime, linux_runtime, $($name,)* }
            }
        }
    };
}

plugin_manifest! {
    kind: String,
    manifest_version: u32,
    id: String,
    name: String,
    version: String,
    host: String,
    description: Option<String>,
    author: Option<String>,
    homepage: Option<String>,
    icon: Option<String>,
    banner: Option<String>,
    #[serde(default)]
    replaces: Vec<String>,
    abi_version: u32,
    #[serde(default)]
    concurrent_dispatch: bool,
    settings_schema: Option<String>,
    #[serde(default)]
    root_slots: Vec<String>,
    #[serde(default)]
    covers: Vec<String>,
    #[serde(default)]
    targets: std::collections::BTreeMap<String, Target>,
    #[serde(rename = "match")]
    matcher: Option<MatchTable>,
    #[serde(default, rename = "resource_kinds")]
    resource_kinds: Vec<ResourceKind>,
    #[serde(default)]
    actions: Vec<ActionDeclaration>,
    #[serde(default)]
    provides: Vec<Capability>,
    #[serde(default)]
    requires: Vec<Capability>,
    #[serde(default)]
    optional: Vec<Capability>,
}

pub(super) fn reserved_kind(kind: &str) -> bool {
    matches!(kind, "file" | "audio" | "background" | "set" | "skin")
}

pub(super) fn validate(p: &PluginManifest) -> Result<(), ValidationError> {
    metadata(
        p.homepage.as_deref(),
        p.icon.as_deref(),
        p.banner.as_deref(),
    )?;
    validate_replaces(&p.id, &p.replaces, true)?;
    super::requirements::validate_lists("plugin", &p.requires, &p.optional)?;
    unique(p.provides.iter().map(|c| c.id.as_str()), "/provides")?;
    unique(p.root_slots.iter().map(String::as_str), "/root_slots")?;
    unique(p.covers.iter().map(String::as_str), "/covers")?;
    unique(
        p.resource_kinds.iter().map(|r| r.kind.as_str()),
        "/resource_kinds",
    )?;
    for target in p.targets.keys() {
        if !valid_native_target(target) {
            return Err(ValidationError::new(
                "manifest.target",
                format!("/targets/{target}"),
            ));
        }
    }
    let whole = p
        .provides
        .iter()
        .find(|c| c.id.strip_prefix("field:") == Some(p.id.as_str()));
    for (i, cap) in p.provides.iter().enumerate() {
        let pointer = format!("/provides/{i}");
        match parse_capability(&cap.id)
            .map_err(|_| ValidationError::new("manifest.capability", format!("{pointer}/id")))?
        {
            CapabilityId::Field(suffix) => {
                if suffix != p.id && covers(&p.id, suffix) {
                    let whole = whole.ok_or_else(|| {
                        ValidationError::new("manifest.own-schema", format!("{pointer}/id"))
                    })?;
                    if semver::Version::parse(&cap.version).ok()
                        != semver::Version::parse(&whole.version).ok()
                    {
                        return Err(ValidationError::new(
                            "manifest.field-version",
                            format!("{pointer}/version"),
                        ));
                    }
                }
            }
            CapabilityId::Resource(suffix) => {
                let kind = suffix
                    .strip_prefix(&p.id)
                    .and_then(|r| r.strip_prefix('.'))
                    .ok_or_else(|| {
                        ValidationError::new("manifest.resource-owner", format!("{pointer}/id"))
                    })?;
                if reserved_kind(kind)
                    || !super::valid_kind_segment(kind)
                    || !p.resource_kinds.iter().any(|r| r.kind == kind)
                {
                    return Err(ValidationError::new(
                        "manifest.resource-pair",
                        format!("{pointer}/id"),
                    ));
                }
            }
            _ => {}
        }
    }
    for (i, kind) in p.resource_kinds.iter().enumerate() {
        let pointer = format!("/resource_kinds/{i}");
        if kind
            .families
            .iter()
            .any(|f| !matches!(f.as_str(), "image" | "audio" | "video" | "chart-text"))
        {
            return Err(ValidationError::new(
                "manifest.resource-family",
                format!("{pointer}/families"),
            ));
        }
        if reserved_kind(&kind.kind) {
            if kind.tree != matches!(kind.kind.as_str(), "set" | "skin") {
                return Err(ValidationError::new(
                    "manifest.resource-tree",
                    format!("{pointer}/tree"),
                ));
            }
        } else if !p.provides.iter().any(|c| {
            c.id.strip_prefix("resource:")
                .and_then(|s| s.strip_prefix(&p.id))
                .and_then(|s| s.strip_prefix('.'))
                == Some(kind.kind.as_str())
        }) {
            return Err(ValidationError::new(
                "manifest.resource-pair",
                format!("{pointer}/kind"),
            ));
        }
    }
    let game = p.provides.iter().any(|c| c.id.starts_with("game:"));
    if game != p.matcher.is_some() {
        return Err(ValidationError::new("manifest.game-matcher", "/match"));
    }
    if let Some(matcher) = &p.matcher {
        matcher.validate_document()?;
    }
    Ok(())
}

impl super::MatchTable {
    /// Portable packaging checks only, not runtime matching or enabled-set policy.
    pub fn validate_document(&self) -> Result<(), ValidationError> {
        for (name, hints) in [
            ("comm_hints", &self.comm_hints),
            ("linux_name_contains", &self.linux_name_contains),
        ] {
            for (i, hint) in hints.iter().enumerate() {
                if hint.is_empty() || hint.len() > 15 || hint.contains('\0') {
                    return Err(ValidationError::new(
                        "manifest.matcher-hint",
                        format!("/match/{name}/{i}"),
                    ));
                }
            }
        }
        for (name, patterns) in [
            ("process_path_patterns", &self.process_path_patterns),
            ("command_line_patterns", &self.command_line_patterns),
        ] {
            for (i, pattern) in patterns.iter().enumerate() {
                let normalized = normalize_pattern_separators(pattern);
                // Same Regex::new default size/nesting limits and anchors as core.
                // Check both platform name policies; argv is case-sensitive everywhere.
                let policies: &[bool] = if name == "process_path_patterns" {
                    &[false, true]
                } else {
                    &[false]
                };
                for &insensitive in policies {
                    let flag = if insensitive { "(?i)" } else { "" };
                    if regex::Regex::new(&format!("{flag}^(?:{normalized})$")).is_err() {
                        return Err(ValidationError::new(
                            "manifest.matcher-regex",
                            format!("/match/{name}/{i}"),
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Literal escaped backslashes are path separators; regex escapes stay intact.
fn normalize_pattern_separators(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('/'),
            Some(next) => {
                out.push('\\');
                out.push(next);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Lexical target-triple validation, not a claim the host supports that target.
pub fn valid_native_target(target: &str) -> bool {
    target.split('-').count() >= 3
        && target.split('-').all(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}
