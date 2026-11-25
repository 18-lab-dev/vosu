use crate::state::schema::{CORE_NAMESPACES, SchemaRegistry};
use vosu_plugin_api::manifest::{Capability, PackageManifest, Requirement};

/// Construct from the actual core schema registry and compiled bundled manifests,
/// never from catalogs. These providers have no installable node or reference.
#[derive(Debug, Default)]
pub struct BuiltinProviders {
    capabilities: Vec<(String, String)>,
}
impl BuiltinProviders {
    pub fn from_registry(registry: &SchemaRegistry, bundled: &[PackageManifest]) -> Self {
        let mut capabilities = Vec::new();
        for namespace in CORE_NAMESPACES {
            if let Some(schema) = registry.get(namespace) {
                let version = schema.version.to_string();
                capabilities.push((format!("field:{namespace}"), version.clone()));
                let resources: &[&str] = match namespace {
                    "vosu.chart" => &["chart.file", "chart.audio", "chart.background", "chart.set"],
                    "vosu.skin" => &["skin"],
                    _ => &[],
                };
                for resource in resources {
                    capabilities.push((format!("resource:{resource}"), version.clone()));
                }
            }
        }
        for manifest in bundled {
            for Capability { id, version, .. } in manifest.fields().provides {
                capabilities.push((id.clone(), version.clone()));
            }
        }
        Self { capabilities }
    }
    pub(super) fn satisfies(&self, requirement: &Requirement) -> bool {
        self.capabilities
            .iter()
            .any(|(id, version)| requirement.satisfied_by(id, version))
    }
}
