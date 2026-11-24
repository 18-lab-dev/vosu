//! Live installed-provider metadata, never catalog or installer resolution.
use std::sync::Arc;

use semver::Version;
use vosu_core::actions::{ActionDescriptor, ActionRegistry};

/// Non-action capability resolution against loaded providers (including core).
/// Implementations apply field ancestor coverage, never catalog resolution.
/// For game capabilities, any returned version represents presence only.
pub trait InstalledCapabilities: Send + Sync {
    fn version(&self, capability: &str) -> Option<Version>;
    fn validation_snapshot(&self) -> vosu_core::plugin::ValidationSnapshot {
        vosu_core::plugin::ValidationSnapshot::default()
    }
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

/// Host-owned live lookup around the SDK's pure requirement predicate.
#[derive(Clone, Debug)]
pub struct Requirement(vosu_plugin_api::manifest::Requirement);

impl std::ops::Deref for Requirement {
    type Target = vosu_plugin_api::manifest::Requirement;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Requirement {
    pub(super) fn from_capability(
        capability: &vosu_plugin_api::manifest::Capability,
    ) -> Result<Self, String> {
        vosu_plugin_api::manifest::Requirement::parse(&capability.id, &capability.version)
            .map(Self)
            .map_err(|error| error.to_string())
    }

    pub(super) fn resolved(
        &self,
        registry: &dyn ActionRegistry,
        installed: &dyn InstalledCapabilities,
    ) -> bool {
        if let Some(action) = self.id.strip_prefix("action:") {
            registry.get(action).is_some_and(|descriptor| {
                descriptor.id == action && self.satisfied_by_version(&self.id, &descriptor.version)
            })
        } else {
            installed.version(&self.id).is_some_and(|version| {
                if self.id.starts_with("game:") {
                    self.satisfied_by(&self.id, "*")
                } else {
                    self.satisfied_by_version(&self.id, &version)
                }
            })
        }
    }
}
