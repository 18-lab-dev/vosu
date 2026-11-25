//! Stable diagnostics for plugin-view callers and unified package callers.
use super::{ManifestError, ValidationError};
impl ManifestError {
    pub(super) fn for_manifest(&self, manifest: &super::PluginManifest) -> ValidationError {
        let mut diagnostic = self.diagnostic();
        let (id, member) = match self {
            Self::Capability(id)
            | Self::CapabilityForeignNamespace(id)
            | Self::CapabilitySelfDependency(id)
            | Self::CapabilityDuplicate(id) => (id, "id"),
            Self::CapabilityVersion(id) | Self::CapabilityGameVersion(id) => (id, "version"),
            Self::CapabilityMissingSchema(id) | Self::CapabilityUnexpectedSchema(id) => {
                (id, "schema")
            }
            _ => return diagnostic,
        };
        for (name, list) in [
            ("provides", &manifest.provides),
            ("requires", &manifest.requires),
            ("optional", &manifest.optional),
        ] {
            if (matches!(self, Self::CapabilitySelfDependency(_)) && name == "provides")
                || (matches!(self, Self::CapabilityDuplicate(_)) && name != "optional")
            {
                continue;
            }
            if let Some(i) = list.iter().position(|c| c.id == *id) {
                diagnostic.pointer = format!("/{name}/{i}/{member}");
                break;
            }
        }
        diagnostic
    }
    pub fn diagnostic(&self) -> ValidationError {
        let (rule, pointer) = match self {
            Self::Rule(error) => return error.clone(),
            Self::Toml(_) => ("manifest.shape", ""),
            Self::Kind => ("manifest.kind", "/kind"),
            Self::ManifestVersion => ("manifest.format", "/manifest_version"),
            Self::Id => ("manifest.id", "/id"),
            Self::Name => ("manifest.name", "/name"),
            Self::Version(_) => ("manifest.version", "/version"),
            Self::Host(_) => ("manifest.host", "/host"),
            Self::AbiVersion => ("manifest.abi", "/abi_version"),
            Self::SettingsSchema => ("manifest.path", "/settings_schema"),
            Self::RootSlot(_) => ("manifest.root-slot", "/root_slots"),
            Self::Covers(_) => ("manifest.coverage", "/covers"),
            Self::Target(_) => ("manifest.path", "/targets"),
            Self::ResourceKind(_) => ("manifest.resource-kind", "/resource_kinds"),
            Self::Capability(_) => ("manifest.capability", "/provides"),
            Self::CapabilityVersion(_) => ("manifest.capability-version", "/provides"),
            Self::CapabilityGameVersion(_) => ("manifest.game-version", "/provides"),
            Self::CapabilityMissingSchema(_) => ("manifest.own-schema", "/provides"),
            Self::CapabilityForeignNamespace(_) => ("manifest.provide-owner", "/provides"),
            Self::CapabilityDuplicate(_) => ("manifest.intersection", "/optional"),
            Self::CapabilitySchemaPath(_) => ("manifest.path", "/provides"),
            Self::CapabilityUnexpectedSchema(_) => ("manifest.schema-placement", "/provides"),
            Self::CapabilitySelfDependency(_) => ("manifest.self-dependency", "/requires"),
            Self::Action(_) => ("manifest.action", "/actions"),
        };
        ValidationError::new(rule, pointer)
    }
}
