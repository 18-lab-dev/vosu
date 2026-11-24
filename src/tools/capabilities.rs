//! Live installed-provider metadata, never catalog or installer resolution.
use std::sync::Arc;

use semver::{Version, VersionReq};
use serde::Deserialize;
use vosu_core::actions::{ActionDescriptor, ActionRegistry};
use vosu_plugin_api::{CapabilityId, parse_capability};

/// Non-action capability resolution against loaded providers (including core).
/// Implementations apply field ancestor coverage, never catalog resolution.
/// For game capabilities, any returned version represents presence only.
pub trait InstalledCapabilities: Send + Sync {
    fn version(&self, capability: &str) -> Option<Version>;
}

pub(super) struct EmptyRegistry;
impl ActionRegistry for EmptyRegistry {
    fn get(&self, _action: &str) -> Option<Arc<ActionDescriptor>> {
        None
    }
}
impl InstalledCapabilities for EmptyRegistry {
    fn version(&self, _capability: &str) -> Option<Version> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct Requirement {
    pub id: String,
    pub version: VersionReq,
}
impl<'de> Deserialize<'de> for Requirement {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            id: String,
            version: String,
        }
        let raw = Raw::deserialize(deserializer)?;
        if raw.id.starts_with("game:") && raw.version != "*" {
            return Err(serde::de::Error::custom(
                "game capabilities require literal '*'",
            ));
        }
        Ok(Self {
            id: raw.id,
            version: raw.version.parse().map_err(serde::de::Error::custom)?,
        })
    }
}
impl Requirement {
    pub(super) fn validate(&self) -> Result<(), String> {
        let capability = parse_capability(&self.id).map_err(|e| e.to_string())?;
        if let CapabilityId::Field(field) = capability
            && (field == "vosu.osu.internal" || field.starts_with("vosu.osu.internal."))
        {
            return Err("tool capabilities cannot access internal state".into());
        }
        if matches!(capability, CapabilityId::Game(_)) && self.version != VersionReq::STAR {
            return Err("game capabilities require version '*'".into());
        }
        Ok(())
    }

    pub(super) fn resolved(
        &self,
        registry: &dyn ActionRegistry,
        installed: &dyn InstalledCapabilities,
    ) -> bool {
        if let Some(action) = self.id.strip_prefix("action:") {
            registry.get(action).is_some_and(|descriptor| {
                descriptor.id == action && self.version.matches(&descriptor.version)
            })
        } else {
            installed.version(&self.id).is_some_and(|version| {
                self.id.starts_with("game:") || self.version.matches(&version)
            })
        }
    }
}

pub(super) fn version_requirement<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<VersionReq, D::Error> {
    String::deserialize(deserializer)?
        .parse()
        .map_err(serde::de::Error::custom)
}
