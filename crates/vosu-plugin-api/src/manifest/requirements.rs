//! Pure requirement predicates shared with M6b; no availability/trust/installer policy.
use super::{Capability, ValidationError, shared::unique};
use crate::capability::{CapabilityId, covers, parse_capability};

pub(super) fn validate_lists(
    kind: &str,
    requires: &[Capability],
    optional: &[Capability],
) -> Result<(), ValidationError> {
    for (name, list) in [("requires", requires), ("optional", optional)] {
        unique(list.iter().map(|c| c.id.as_str()), &format!("/{name}"))?;
        for (i, cap) in list.iter().enumerate() {
            let pointer = format!("/{name}/{i}");
            let parsed = parse_capability(&cap.id).map_err(|_| {
                ValidationError::new("manifest.capability", format!("{pointer}/id"))
            })?;
            if cap.schema.is_some() {
                return Err(ValidationError::new(
                    "manifest.schema-placement",
                    format!("{pointer}/schema"),
                ));
            }
            if (kind == "overlay"
                && !matches!(parsed, CapabilityId::Field(_) | CapabilityId::Resource(_)))
                || (kind != "plugin" && covers("field:vosu.osu.internal", &cap.id))
            {
                return Err(ValidationError::new(
                    "manifest.permission",
                    format!("{pointer}/id"),
                ));
            }
            let valid = if matches!(parsed, CapabilityId::Game(_)) {
                cap.version == "*"
            } else {
                semver::VersionReq::parse(&cap.version).is_ok()
            };
            if !valid {
                return Err(ValidationError::new(
                    "manifest.requirement",
                    format!("{pointer}/version"),
                ));
            }
        }
    }
    for (i, cap) in optional.iter().enumerate() {
        if requires.iter().any(|r| r.id == cap.id) {
            return Err(ValidationError::new(
                "manifest.intersection",
                format!("/optional/{i}/id"),
            ));
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
        let parsed =
            parse_capability(id).map_err(|_| ValidationError::new("manifest.capability", "/id"))?;
        if matches!(parsed, CapabilityId::Game(_)) && version != "*" {
            return Err(ValidationError::new("manifest.requirement", "/version"));
        }
        let version_req = semver::VersionReq::parse(version)
            .map_err(|_| ValidationError::new("manifest.requirement", "/version"))?;
        Ok(Self {
            id: id.to_owned(),
            raw_version: version.to_owned(),
            version: version_req,
        })
    }

    /// Coverage and capability version only; package release is deliberately absent.
    pub fn satisfied_by(&self, provided_id: &str, provided_version: &str) -> bool {
        if self.id.starts_with("game:") {
            return provided_version == "*"
                && parse_capability(provided_id).is_ok()
                && covers(provided_id, &self.id);
        }
        semver::Version::parse(provided_version)
            .is_ok_and(|version| self.satisfied_by_version(provided_id, &version))
    }

    /// Matches a runtime's already parsed interface revision without formatting
    /// or reparsing it. Games use the literal-presence predicate above.
    pub fn satisfied_by_version(&self, provided_id: &str, version: &semver::Version) -> bool {
        !self.id.starts_with("game:")
            && parse_capability(provided_id).is_ok()
            && covers(provided_id, &self.id)
            && self.version.matches(version)
    }
}

/// All requirements must match this one candidate's declared capabilities.
/// Optional absence is caller-owned and must not become an installation request.
pub fn candidate_satisfies(requirements: &[Requirement], provides: &[Capability]) -> bool {
    requirements
        .iter()
        .all(|r| provides.iter().any(|p| r.satisfied_by(&p.id, &p.version)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_runtime_requirement_uses_interface_revision_and_coverage() {
        let requirement = Requirement::parse("field:acme.game.score", "^1.2.0").unwrap();
        assert!(
            requirement.satisfied_by_version("field:acme.game", &semver::Version::new(1, 9, 0))
        );
        assert!(
            !requirement.satisfied_by_version("field:acme.game", &semver::Version::new(2, 0, 0))
        );
        assert!(
            !requirement.satisfied_by_version("field:acme.gam", &semver::Version::new(1, 9, 0))
        );
        assert!(
            !requirement.satisfied_by_version("action:acme.game", &semver::Version::new(1, 9, 0))
        );
        assert!(
            !Requirement::parse("game:osu", "*")
                .unwrap()
                .satisfied_by_version("game:osu", &semver::Version::new(1, 0, 0))
        );
    }
}
