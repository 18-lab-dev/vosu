//! Owned snapshot of live provider identities, cached by topology generation.
use std::sync::{Arc, atomic::Ordering};

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
    /// Monotonic generation bumped on every live instance transition and every
    /// schema-registry write.
    pub fn validation_generation(&self) -> u64 {
        self.topology_generation.load(Ordering::Acquire)
    }

    /// Returns a cached live validation snapshot for the current topology.
    /// Lock order: `validation_cache` is a leaf lock, never held across
    /// `instances` or `state.registry`.
    pub fn validation_snapshot_shared(&self) -> Arc<ValidationSnapshot> {
        let before = self.validation_generation();
        {
            let cache = self
                .validation_cache
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some((generation, snapshot)) = cache.as_ref()
                && *generation == before
            {
                return Arc::clone(snapshot);
            }
        }
        let snapshot = Arc::new(self.build_validation_snapshot());
        let after = self.validation_generation();
        if after == before {
            let mut cache = self
                .validation_cache
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if cache
                .as_ref()
                .is_none_or(|(generation, _)| *generation < after)
            {
                *cache = Some((after, Arc::clone(&snapshot)));
            }
        }
        snapshot
    }

    pub fn validation_snapshot(&self) -> ValidationSnapshot {
        (*self.validation_snapshot_shared()).clone()
    }

    fn build_validation_snapshot(&self) -> ValidationSnapshot {
        let providers = self
            .instances
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter_map(|slot| match slot {
                super::InstanceSlot::Live(entry) => Some(ProviderRegistration {
                    id: entry.plugin_id.clone(),
                    official: entry.publisher.official,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::PluginInstanceId;
    use crate::plugin::{BundledPlugin, load_bundled};
    use vosu_plugin_testkit::publisher;

    const MANIFEST: &str = r#"
kind = 'plugin'
manifest_version = 1
id = 'test.validation.cache'
name = 'Validation cache test'
version = '1.0.0'
host = '>=2.0.0-alpha.1, <3.0.0'
abi_version = 1
runtime = 'in-process'
"#;

    #[test]
    fn snapshot_cache_reuses_arc_until_live_instance_changes() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _enter = rt.enter();
        let (dir, host) = super::super::super::tests::test_host(rt.handle().clone());

        let gen0 = host.validation_generation();
        let snap0 = host.validation_snapshot_shared();
        let snap0_again = host.validation_snapshot_shared();
        assert!(Arc::ptr_eq(&snap0, &snap0_again));
        assert!(snap0.providers.is_empty());

        let _events = publisher::register("test.validation.cache");
        let loaded = load_bundled(
            &semver::Version::parse("2.0.0-alpha.1").unwrap(),
            &snap0,
            &[BundledPlugin {
                manifest: MANIFEST,
                settings_schema: None,
                namespace_schemas: &[],
                action_schemas: &[],
                vtable: publisher::vosu_plugin_vtable,
            }],
        )
        .unwrap();
        let id = PluginInstanceId::new(98_701);
        host.create_instance(id, &loaded[0], dir.path().to_path_buf(), None)
            .unwrap();

        let gen1 = host.validation_generation();
        assert!(gen1 > gen0);
        let snap1 = host.validation_snapshot_shared();
        let snap1_again = host.validation_snapshot_shared();
        assert!(Arc::ptr_eq(&snap1, &snap1_again));
        assert!(!Arc::ptr_eq(&snap0, &snap1));
        assert_eq!(snap1.providers.len(), 1);
        assert_eq!(snap1.providers[0].id, "test.validation.cache");

        host.teardown_instance(id).unwrap();
        let gen2 = host.validation_generation();
        assert!(gen2 > gen1);
        let snap2 = host.validation_snapshot_shared();
        assert!(snap2.providers.is_empty());
        host.destroy_all(std::time::Duration::from_secs(5));
    }
}
