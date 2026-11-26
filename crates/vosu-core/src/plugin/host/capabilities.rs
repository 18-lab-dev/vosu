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

    /// The plugin id of the live instance whose provided capability resolves
    /// `capability` at [`Self::installed_capability_version`]'s version.
    /// `None` for core namespaces, actions and unresolved capabilities.
    pub fn installed_capability_provider(&self, capability: &str) -> Option<String> {
        let kind = parse_capability(capability).ok()?;
        if matches!(kind, CapabilityId::Action(_)) {
            return None;
        }
        if let CapabilityId::Field(field) = kind
            && (covers("vosu.osu.internal", field)
                || crate::state::schema::CORE_NAMESPACES
                    .iter()
                    .any(|ns| covers(ns, field)))
        {
            return None;
        }
        self.instances
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter_map(|slot| match slot {
                InstanceSlot::Live(entry) => Some(entry),
                _ => None,
            })
            .flat_map(|entry| {
                entry
                    .capabilities
                    .iter()
                    .map(move |provided| (&entry.plugin_id, provided))
            })
            .filter(|(_, provided)| {
                provided.id == capability
                    || matches!(kind, CapabilityId::Field(_)) && covers(&provided.id, capability)
            })
            .max_by(|(_, a), (_, b)| a.version.cmp(&b.version))
            .map(|(plugin, _)| plugin.clone())
    }
}
