//! Whole-source SDK context construction. No candidate selection or namespace reimplementation.
use super::{SourceIdentity, SourceRecord};
use semver::Version;
use vosu_plugin_api::{
    catalog::Catalog,
    manifest::{NamespaceRevision, Registration, ValidationContext, ValidationError},
};

pub struct SourceCatalog<'a> {
    pub source: &'a SourceRecord,
    pub catalog: &'a Catalog,
}
/// Inputs must be the complete successfully parsed enabled source set, plus actual
/// installed/bundled registrations. Failed sources are excluded by the coordinator.
/// Each catalog is validated against every other catalog, never a first-wins map.
pub fn validate_catalogs<'a>(
    catalogs: &'a [SourceCatalog<'a>],
    installed_and_bundled: &'a [Registration<'a>],
    namespace_revisions: &'a [NamespaceRevision<'a>],
    application_version: &'a Version,
) -> Vec<(&'a SourceIdentity, Result<(), ValidationError>)> {
    catalogs
        .iter()
        .filter(|item| item.source.enabled)
        .map(|item| {
            let mut registrations = installed_and_bundled.to_vec();
            for other in catalogs.iter().filter(|other| {
                other.source.enabled && other.source.identity != item.source.identity
            }) {
                registrations.extend(other.catalog.entries().map(|(kind, entry)| Registration {
                    kind,
                    id: &entry.id,
                    source: other.source.identity.as_str(),
                    official: other.source.identity.is_official(),
                    builtin: false,
                    replaces: &entry.replaces,
                }));
            }
            let official_ids = if item.source.identity.is_official() {
                item.catalog
                    .entries()
                    .map(|(_, entry)| entry.id.as_str())
                    .collect()
            } else {
                Vec::new()
            };
            let context = ValidationContext {
                application_version,
                source: item.source.identity.as_str(),
                official: item.source.identity.is_official(),
                registrations: &registrations,
                namespace_revisions,
            };
            (
                &item.source.identity,
                item.catalog.validate_context(&context, &official_ids),
            )
        })
        .collect()
}
