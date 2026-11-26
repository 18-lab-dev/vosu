//! Live installed-provider metadata, never catalog or installer resolution.
use std::sync::Arc;

use crate::actions::{ActionDescriptor, ActionRegistry};
use semver::Version;

/// Non-action capability resolution against loaded providers (including core).
/// Implementations apply field ancestor coverage, never catalog resolution.
/// For game capabilities, any returned version represents presence only.
pub trait InstalledCapabilities: Send + Sync {
    fn version(&self, capability: &str) -> Option<Version>;
    fn validation_snapshot(&self) -> crate::plugin::ValidationSnapshot {
        crate::plugin::ValidationSnapshot::default()
    }
    /// The plugin id of the loaded provider that resolves `capability`, or
    /// `None` for core namespaces and unresolved capabilities.
    fn provider(&self, _capability: &str) -> Option<String> {
        None
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

    /// The resolved provider's plugin id for the `ToolView` (`null` when
    /// unresolved or core-provided).
    pub(super) fn provider(
        &self,
        registry: &dyn ActionRegistry,
        installed: &dyn InstalledCapabilities,
    ) -> Option<String> {
        if !self.resolved(registry, installed) {
            return None;
        }
        match self.id.strip_prefix("action:") {
            Some(action) => registry
                .get(action)
                .map(|descriptor| descriptor.provider_id.clone()),
            None => installed.provider(&self.id),
        }
    }
}
