//! Complete mirror checks for publisher/install boundaries, before any mutation.
use super::CatalogEntry;
use crate::manifest::{PackageKind, PackageManifest, ValidationError, schema::CompiledSchema};

fn mismatch(pointer: impl Into<String>) -> ValidationError {
    ValidationError::new("catalog.mirror", pointer)
}

/// Compare every mirrored package field, including ordered requirements and extracted lanes.
/// `schemas` MUST be compiler results from the actual package schema bytes, not metadata
/// reconstructed from this catalog. Every manifest schema declaration needs exactly one result;
/// omission is an error, not partial verification. Package-local art/UI/library/schema paths
/// are intentionally not compared to remote catalog URLs or invented catalog fields.
pub fn compare_package(
    kind: PackageKind,
    entry: &CatalogEntry,
    manifest: &PackageManifest,
    schemas: &[CompiledSchema],
) -> Result<(), ValidationError> {
    entry.validate(kind)?;
    manifest.validate()?;
    if manifest.kind() != kind {
        return Err(mismatch("/kind"));
    }
    let fields = manifest.fields();
    macro_rules! equal {
        ($name:ident, $actual:expr) => {
            if entry.$name != $actual {
                return Err(mismatch(concat!("/", stringify!($name))));
            }
        };
    }
    equal!(id, fields.id);
    equal!(name, fields.name);
    equal!(version, fields.version);
    equal!(host, fields.host);
    if entry.description.as_deref() != fields.description {
        return Err(mismatch("/description"));
    }
    if entry.author.as_deref() != fields.author {
        return Err(mismatch("/author"));
    }
    if entry.homepage.as_deref() != fields.homepage {
        return Err(mismatch("/homepage"));
    }
    equal!(replaces, fields.replaces);
    equal!(requires, fields.requires);
    equal!(optional, fields.optional);
    if entry.provides.len() != fields.provides.len() {
        return Err(mismatch("/provides"));
    }
    let mut schema_count = 0;
    for (i, (catalog, package)) in entry.provides.iter().zip(fields.provides).enumerate() {
        let path = format!("/provides/{i}");
        if catalog.id != package.id || catalog.version != package.version {
            return Err(mismatch(path));
        }
        if package.schema.is_some() {
            schema_count += 1;
            let namespace = package
                .id
                .strip_prefix("field:")
                .ok_or_else(|| mismatch(&path))?;
            let mut matching = schemas.iter().filter(|s| s.namespace() == namespace);
            let schema = matching
                .next()
                .ok_or_else(|| mismatch(format!("{path}/lanes")))?;
            if matching.next().is_some()
                || catalog
                    .schema_version
                    .as_deref()
                    .and_then(|v| semver::Version::parse(v).ok())
                    .as_ref()
                    != Some(schema.version())
            {
                return Err(mismatch(format!("{path}/schema_version")));
            }
            if catalog.lanes.as_deref() != Some(schema.lanes()) {
                return Err(mismatch(format!("{path}/lanes")));
            }
        }
    }
    if schemas.len() != schema_count {
        return Err(mismatch("/provides"));
    }
    match manifest {
        PackageManifest::Plugin(plugin) => {
            if entry.runtime != Some(plugin.runtime) {
                return Err(mismatch("/runtime"));
            }
            if entry.linux_user_namespaces.unwrap_or_default() != plugin.linux_user_namespaces() {
                return Err(mismatch("/linux_user_namespaces"));
            }
            if entry.matcher != plugin.matcher {
                return Err(mismatch("/matcher"));
            }
            if entry.actions.as_deref().unwrap_or_default() != plugin.actions {
                return Err(mismatch("/actions"));
            }
            if entry.resource_kinds.as_deref().unwrap_or_default() != plugin.resource_kinds {
                return Err(mismatch("/resource_kinds"));
            }
            if entry.covers.as_deref().unwrap_or_default() != plugin.covers {
                return Err(mismatch("/covers"));
            }
        }
        PackageManifest::Overlay(overlay) => equal!(resolution, overlay.resolution),
        PackageManifest::Tool(_) => {}
    }
    Ok(())
}
