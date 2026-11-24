//! Read-only installed-provider lookup. Only Live instance entries authorize;
//! creation, loss and destruction immediately cease to provide capabilities.
use super::{InstanceSlot, PluginHost};
use semver::Version;
use vosu_plugin_api::{CapabilityId, covers, parse_capability};

pub mod validation;

pub(super) struct ProvidedCapability {
    pub id: String,
    pub version: Version,
}

impl PluginHost {
    pub fn installed_capability_version(&self, capability: &str) -> Option<Version> {
        let kind = parse_capability(capability).ok()?;
        if matches!(kind, CapabilityId::Action(_)) {
            return None;
        }
        if let CapabilityId::Field(field) = kind {
            if covers("vosu.osu.internal", field) {
                return None;
            }
            // Core namespaces are always installed. Their schema version, not
            // the application's version, is the public capability version.
            for ns in crate::state::schema::CORE_NAMESPACES {
                if covers(ns, field) {
                    return self
                        .state
                        .registry
                        .read()
                        .unwrap_or_else(|p| p.into_inner())
                        .get(ns)
                        .map(|schema| schema.version.clone());
                }
            }
        }
        // Only the instances lock is held; no plugin callback or schema lock.
        self.instances
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter_map(|slot| match slot {
                InstanceSlot::Live(entry) => Some(entry),
                _ => None,
            })
            .flat_map(|entry| &entry.capabilities)
            .filter(|provided| {
                provided.id == capability
                    || matches!(kind, CapabilityId::Field(_)) && covers(&provided.id, capability)
            })
            .map(|provided| &provided.version)
            .max()
            .cloned()
    }
}
