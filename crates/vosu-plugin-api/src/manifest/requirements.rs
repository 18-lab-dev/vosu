//! Pure requirement predicates shared with M6b; no availability/trust/installer policy.
use crate::capability::{CapabilityId, covers, parse_capability};
use super::{Capability, ValidationError, shared::unique};

pub(super) fn validate_lists(kind: &str, requires: &[Capability], optional: &[Capability]) -> Result<(), ValidationError> {
    for (name, list) in [("requires", requires), ("optional", optional)] {
        unique(list.iter().map(|c| c.id.as_str()), &format!("/{name}"))?;
        for (i, cap) in list.iter().enumerate() {
            let pointer = format!("/{name}/{i}");
            let parsed = parse_capability(&cap.id).map_err(|_| ValidationError::new("manifest.capability", format!("{pointer}/id")))?;
            if cap.schema.is_some() { return Err(ValidationError::new("manifest.schema-placement", format!("{pointer}/schema"))); }
            if (kind == "overlay" && !matches!(parsed, CapabilityId::Field(_) | CapabilityId::Resource(_)))
                || (kind != "plugin" && covers("field:vosu.osu.internal", &cap.id)) {
                return Err(ValidationError::new("manifest.permission", format!("{pointer}/id")));
            }
            let valid = if matches!(parsed, CapabilityId::Game(_)) { cap.version == "*" }
                else { semver::VersionReq::parse(&cap.version).is_ok() };
            if !valid { return Err(ValidationError::new("manifest.requirement", format!("{pointer}/version"))); }
        }
    }
    for (i, cap) in optional.iter().enumerate() {
        if requires.iter().any(|r| r.id == cap.id) {
            return Err(ValidationError::new("manifest.intersection", format!("/optional/{i}/id")));
        }
    }
    Ok(())
}

/// One validated requirement, preserving the raw literal for games and mirrors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub id: String,
    pub raw_version: String,
    version: semver::VersionReq,
}
impl Requirement {
    pub fn parse(id: &str, version: &str) -> Result<Self, ValidationError> {
        let parsed = parse_capability(id).map_err(|_| ValidationError::new("manifest.capability", "/id"))?;
        if matches!(parsed, CapabilityId::Game(_)) && version != "*" {
            return Err(ValidationError::new("manifest.requirement", "/version"));
        }
        let version_req = semver::VersionReq::parse(version).map_err(|_| ValidationError::new("manifest.requirement", "/version"))?;
        Ok(Self { id: id.to_owned(), raw_version: version.to_owned(), version: version_req })
    }

    /// Coverage and capability version only; package release is deliberately absent.
    pub fn satisfied_by(&self, provided_id: &str, provided_version: &str) -> bool {
        if parse_capability(provided_id).is_err() || !covers(provided_id, &self.id) { return false; }
        if self.id.starts_with("game:") { return provided_version == "*"; }
        semver::Version::parse(provided_version).is_ok_and(|v| self.version.matches(&v))
    }
}

/// All requirements must match this one candidate's declared capabilities.
/// Optional absence is caller-owned and must not become an installation request.
pub fn candidate_satisfies(requirements: &[Requirement], provides: &[Capability]) -> bool {
    requirements.iter().all(|r| provides.iter().any(|p| r.satisfied_by(&p.id, &p.version)))
}
