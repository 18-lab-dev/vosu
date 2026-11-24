//! Owned snapshot of live provider identities; rebuilt for each validation.
use vosu_plugin_api::manifest::{
    HostSupport, NamespaceRevision, PackageKind, PackageManifest, Registration, SchemaProvenance,
    ValidationContext, ValidationError,
};

#[derive(Clone, Debug)]
pub struct ProviderRegistration {
    pub id: String,
    pub official: bool,
    pub builtin: bool,
    pub replaces: Vec<String>,
    pub revision: Option<semver::Version>,
}

/// A validated package identity copied from the current host/source inventory.
#[derive(Clone, Debug)]
pub struct PackageRegistration {
    pub kind: PackageKind,
    pub id: String,
    pub source: String,
    pub replaces: Vec<String>,
}

impl PackageRegistration {
    pub fn new(package: &PackageManifest, source: &str) -> Self {
        Self {
            kind: package.kind(),
            id: package.fields().id.to_owned(),
            source: source.to_owned(),
            replaces: package.fields().replaces.to_vec(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ValidationSnapshot {
    pub providers: Vec<ProviderRegistration>,
    pub core_revisions: Vec<(String, semver::Version)>,
    pub packages: Vec<PackageRegistration>,
}

impl ValidationSnapshot {
    pub fn validate(
        &self,
        package: &PackageManifest,
        application_version: &semver::Version,
        source: &str,
        official: bool,
    ) -> Result<HostSupport, ValidationError> {
        let registrations: Vec<_> = self
            .providers
            .iter()
            .map(|provider| Registration {
                kind: PackageKind::Plugin,
                id: &provider.id,
                source: if provider.builtin {
                    "bundled"
                } else {
                    "development"
                },
                official: provider.official,
                builtin: provider.builtin,
                replaces: &provider.replaces,
            })
            .chain(self.packages.iter().map(|package| Registration {
                kind: package.kind,
                id: &package.id,
                source: &package.source,
                official: false,
                builtin: false,
                replaces: &package.replaces,
            }))
            .collect();
        let revisions: Vec<_> = self
            .core_revisions
            .iter()
            .map(|(namespace, version)| NamespaceRevision {
                namespace,
                version: version.clone(),
                provenance: SchemaProvenance::TrustedBundled,
            })
            .chain(self.providers.iter().filter_map(|provider| {
                provider.revision.as_ref().map(|version| NamespaceRevision {
                    namespace: &provider.id,
                    version: version.clone(),
                    provenance: if provider.builtin {
                        SchemaProvenance::TrustedBundled
                    } else {
                        SchemaProvenance::ThirdParty
                    },
                })
            }))
            .collect();
        package.validate_context(&ValidationContext {
            application_version,
            source,
            official,
            registrations: &registrations,
            namespace_revisions: &revisions,
        })
    }
}

impl super::PluginHost {
    pub fn validation_snapshot(&self) -> ValidationSnapshot {
        let providers = self
            .instances
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter_map(|slot| match slot {
                super::InstanceSlot::Live(entry) => Some(ProviderRegistration {
                    id: entry.plugin_id.clone(),
                    official: entry.publisher.bundled,
                    builtin: entry.publisher.bundled,
                    replaces: entry.replaces.clone(),
                    revision: entry
                        .capabilities
                        .iter()
                        .find(|cap| cap.id.strip_prefix("field:") == Some(entry.plugin_id.as_str()))
                        .map(|cap| cap.version.clone()),
                }),
                _ => None,
            })
            .collect();
        let registry = self
            .state
            .registry
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let core_revisions = crate::state::schema::CORE_NAMESPACES
            .iter()
            .filter_map(|namespace| {
                registry
                    .get(namespace)
                    .map(|schema| ((*namespace).to_owned(), schema.version.clone()))
            })
            .collect();
        ValidationSnapshot {
            providers,
            core_revisions,
            packages: Vec::new(),
        }
    }
}
