//! Explicit registration/source context. No filesystem, implicit officialness or SDK host version.
use std::collections::BTreeSet;
use semver::Version;
use crate::capability::{CORE_NAMESPACES, CapabilityId, parse_capability};
use super::{PackageManifest, ValidationError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind { Plugin, Tool, Overlay }

/// Provenance supplied by the host, never obtained from a document's title/id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaProvenance { ThirdParty, Official, TrustedBundled }

#[derive(Debug, Clone)]
pub struct Registration<'a> {
    pub kind: PackageKind,
    pub id: &'a str,
    pub source: &'a str,
    pub official: bool,
    pub builtin: bool,
    pub replaces: &'a [String],
}

#[derive(Debug, Clone)]
pub struct NamespaceRevision<'a> {
    pub namespace: &'a str,
    pub version: Version,
    pub provenance: SchemaProvenance,
}

pub struct ValidationContext<'a> {
    pub application_version: &'a Version,
    pub source: &'a str,
    /// Exact current identity's status, established by the host/source registry.
    pub official: bool,
    pub registrations: &'a [Registration<'a>],
    pub namespace_revisions: &'a [NamespaceRevision<'a>],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSupport { Supported, Unsupported }

/// Full namespace index, shared by package/catalog validation and the resolver.
#[derive(Debug, Clone)]
pub struct NamespaceIndex<'a> { namespaces: BTreeSet<&'a str> }
impl<'a> NamespaceIndex<'a> {
    pub fn new(registrations: &'a [Registration<'a>]) -> Result<Self, ValidationError> {
        let mut result = Self { namespaces: CORE_NAMESPACES.into_iter().collect() };
        for (i, registration) in registrations.iter().enumerate() {
            if registration.kind == PackageKind::Plugin {
                result.insert(registration.id, registration.official)
                    .map_err(|_| ValidationError::new("manifest.namespace", format!("/registrations/{i}/id")))?;
            }
        }
        Ok(result)
    }

    pub fn insert(&mut self, id: &'a str, official: bool) -> Result<(), ValidationError> {
        if !super::valid_plugin_id(id) || (id.starts_with("vosu.") && !official)
            || self.namespaces.iter().any(|n| *n != id && (ancestor(n, id) || ancestor(id, n))) {
            return Err(ValidationError::new("manifest.namespace", "/id"));
        }
        self.namespaces.insert(id);
        Ok(())
    }

    /// Segment-delimited longest prefix, independent of registration order.
    pub fn resolve<'b>(&self, suffix: &'b str) -> Option<(&'a str, &'b str)> {
        self.namespaces.iter().filter_map(|n| {
            if suffix == *n { Some((*n, "")) }
            else { suffix.strip_prefix(*n).and_then(|s| s.strip_prefix('.')).map(|rest| (*n, rest)) }
        }).max_by_key(|(n, _)| n.len())
    }
}

fn ancestor(parent: &str, child: &str) -> bool {
    child.strip_prefix(parent).is_some_and(|r| r.starts_with('.'))
}

impl PackageManifest {
    /// Context validation never silently asserts host compatibility or schema-byte verification.
    pub fn validate_context(&self, context: &ValidationContext<'_>) -> Result<HostSupport, ValidationError> {
        self.validate()?;
        let fields = self.fields();
        let mut index = NamespaceIndex::new(context.registrations)?;
        if self.kind() == PackageKind::Plugin { index.insert(fields.id, context.official)?; }
        validate_replacements(self, context)?;
        for (list, caps) in [("provides", fields.provides), ("requires", fields.requires), ("optional", fields.optional)] {
            for (i, cap) in caps.iter().enumerate() {
                let pointer = format!("/{list}/{i}");
                let parsed = parse_capability(&cap.id).map_err(|_| ValidationError::new("manifest.capability", format!("{pointer}/id")))?;
                let suffix = match parsed {
                    CapabilityId::Game(_) => continue,
                    CapabilityId::Resource("chart.file" | "chart.audio" | "chart.background" | "chart.set" | "skin") => continue,
                    CapabilityId::Field(s) | CapabilityId::Resource(s) | CapabilityId::Action(s) => s,
                };
                let (namespace, rest) = index.resolve(suffix).ok_or_else(|| ValidationError::new("manifest.unknown-namespace", format!("{pointer}/id")))?;
                if CORE_NAMESPACES.contains(&namespace) && !matches!(parsed, CapabilityId::Field(_)) {
                    return Err(ValidationError::new("manifest.capability-owner", format!("{pointer}/id")));
                }
                let valid_path = match parsed {
                    CapabilityId::Field(_) => rest.is_empty() || rest.split('.').all(super::valid_camel_path_segment),
                    CapabilityId::Action(_) => !rest.is_empty() && rest.split('.').all(super::valid_kind_segment),
                    CapabilityId::Resource(_) => super::valid_kind_segment(rest) && !super::plugin::reserved_kind(rest),
                    CapabilityId::Game(_) => unreachable!(),
                };
                if !valid_path { return Err(ValidationError::new("manifest.capability-boundary", format!("{pointer}/id"))); }
                if list == "provides" && matches!(parsed, CapabilityId::Field(_)) && namespace != fields.id {
                    let revision = context.namespace_revisions.iter().find(|r| r.namespace == namespace && r.provenance != SchemaProvenance::ThirdParty)
                        .ok_or_else(|| ValidationError::new("manifest.authoritative-version", format!("{pointer}/version")))?;
                    if rest.is_empty() || Version::parse(&cap.version).ok().as_ref() != Some(&revision.version) {
                        return Err(ValidationError::new("manifest.authoritative-version", format!("{pointer}/version")));
                    }
                }
            }
        }
        let host = semver::VersionReq::parse(fields.host).map_err(|_| ValidationError::new("manifest.host", "/host"))?;
        Ok(if host.matches(context.application_version) { HostSupport::Supported } else { HostSupport::Unsupported })
    }
}

fn validate_replacements(package: &PackageManifest, context: &ValidationContext<'_>) -> Result<(), ValidationError> {
    // Validate claims across the complete source/kind set, not just the current package.
    let fields = package.fields();
    let mut claims = std::collections::BTreeMap::new();
    for registration in context.registrations {
        super::shared::validate_replaces(registration.id, registration.replaces, registration.kind == PackageKind::Plugin)?;
        for former in registration.replaces {
            let key = (registration.source, registration.kind as u8, former.as_str());
            if claims.insert(key, registration.id).is_some_and(|owner| owner != registration.id) {
                return Err(ValidationError::new("manifest.replaces-conflict", "/replaces"));
            }
            if context.registrations.iter().any(|r| r.kind == registration.kind && r.id == former && (r.builtin || r.source == registration.source)) {
                return Err(ValidationError::new("manifest.replaces-current", "/replaces"));
            }
        }
    }
    for (i, former) in fields.replaces.iter().enumerate() {
        if context.registrations.iter().any(|r| r.kind == package.kind() && r.id == former && (r.builtin || r.source == context.source))
            || claims.get(&(context.source, package.kind() as u8, former.as_str())).is_some_and(|id| *id != fields.id) {
            return Err(ValidationError::new("manifest.replaces-conflict", format!("/replaces/{i}")));
        }
    }
    Ok(())
}

