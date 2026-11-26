//! Prepare one pinned add completely before any publication: strict download,
//! same-handle size/hash, staged ZIP preflight, confined assets, SDK asset-backed
//! manifest/schema/catalog mirror equality and target library/UI checks.
use super::{
    InstallError, InstallLayout,
    download::{self, PackageTransport},
    installed::{InstalledFor, InstalledRecord},
    preflight,
};
use crate::catalog::plans::{PinnedAdd, Reason};
use crate::plugin::approval::{Digests, Sha256Digest};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;
use vosu_net::streaming::StagedTree;
use vosu_plugin_api::{
    catalog::{CatalogEntry, compare_package},
    manifest::{
        PackageKind, PackageManifest,
        action_schema::{self, CompiledActionSchema},
        files::MAX_MANIFEST_BYTES,
        schema::{self, CompiledSchema, SchemaProvenance},
        settings::{MAX_SETTINGS_SCHEMA_BYTES, SettingsSchema},
    },
};

/// A fully verified, unpublished package tree owned by this operation.
#[derive(Debug)]
pub(super) struct Prepared {
    pub staged: StagedTree,
    pub record: InstalledRecord,
    pub reason: Reason,
    pub replaces: Vec<String>,
}

pub(super) struct PrepareContext<'a> {
    pub layout: &'a InstallLayout,
    pub transport: &'a dyn PackageTransport,
    pub common_defs: &'a Arc<Value>,
    pub cancel: &'a CancellationToken,
}

pub(super) async fn prepare(
    context: &PrepareContext<'_>,
    add: &PinnedAdd,
) -> Result<Prepared, InstallError> {
    let entry = Arc::clone(&add.entry);
    let package = entry
        .packages
        .get(&add.package_target)
        .ok_or_else(|| InstallError::Unsupported(entry.id.clone()))?
        .clone();
    // The plan's pinned cap bounds compressed AND expanded bytes (P0/A6b.8).
    if package.size > add.package_limit_bytes {
        return Err(InstallError::TooLarge(entry.id.clone()));
    }
    let parent = context.layout.root(add.kind).to_path_buf();
    // Validated destination name before any bytes are fetched.
    context.layout.destination(add.kind, &entry.id)?;
    let (download_directory, file) = download::fetch(
        context.transport,
        &context.layout.downloads,
        &package.url,
        package.size,
        context.cancel,
    )
    .await?;
    let cancel = context.cancel.clone();
    let common_defs = Arc::clone(context.common_defs);
    let kind = add.kind;
    let target = add.package_target.clone();
    let official = add.source.is_official();
    let limit = add.package_limit_bytes;
    let verified_entry = Arc::clone(&entry);
    let (staged, process_digests) = tokio::task::spawn_blocking(move || {
        let mut file = file;
        download::verify(&mut file, package.size, &package.sha256, &cancel)?;
        let staged = preflight::stage(file, &parent, limit, &cancel)?;
        drop(download_directory);
        let provenance = if official {
            SchemaProvenance::TrustedOfficial
        } else {
            SchemaProvenance::Untrusted
        };
        let digests = validate(
            staged.path(),
            kind,
            &verified_entry,
            &target,
            provenance,
            &common_defs,
        )?;
        Ok::<_, InstallError>((staged, digests))
    })
    .await
    .map_err(|_| InstallError::Worker)??;
    let sha256 = package_hash(&entry, &add.package_target)?;
    if add
        .artifact
        .as_ref()
        .is_some_and(|artifact| super::installed::hex(&artifact.sha256) != sha256)
        || add.artifact.is_some() != (kind == PackageKind::Plugin)
    {
        return Err(InstallError::HashMismatch);
    }
    Ok(Prepared {
        staged,
        record: InstalledRecord {
            kind: kind.into(),
            id: entry.id.clone(),
            version: entry.version.clone(),
            source: add.source.clone(),
            target: add.package_target.clone(),
            sha256,
            artifact: add.artifact.clone(),
            process_digests,
            approval: None,
            installed_for: InstalledFor::merge(None, add.reason),
        },
        reason: add.reason,
        replaces: entry.replaces.clone(),
    })
}

fn package_hash(entry: &CatalogEntry, target: &str) -> Result<String, InstallError> {
    let hash = download::decode_sha256(&entry.packages[target].sha256)
        .ok_or(InstallError::HashMismatch)?;
    Ok(super::installed::hex(&hash))
}

fn mismatch(error: vosu_plugin_api::manifest::ValidationError) -> InstallError {
    InstallError::Manifest(error.to_string())
}

/// SDK validation of the actual staged package against the pinned entry.
pub(super) fn validate(
    root: &Path,
    kind: PackageKind,
    entry: &CatalogEntry,
    target: &str,
    provenance: SchemaProvenance,
    common_defs: &Value,
) -> Result<Option<Digests>, InstallError> {
    let text = preflight::read_asset(root, "manifest.toml", MAX_MANIFEST_BYTES)?;
    let manifest = PackageManifest::parse(&text).map_err(mismatch)?;
    let fields = manifest.fields();
    // Kind/id/version first, so a wrong package is named before deeper checks.
    if manifest.kind() != kind {
        return Err(InstallError::Manifest("catalog.mirror at /kind".into()));
    }
    if fields.id != entry.id {
        return Err(InstallError::Manifest("catalog.mirror at /id".into()));
    }
    if fields.version != entry.version {
        return Err(InstallError::Manifest("catalog.mirror at /version".into()));
    }
    let mut compiled: Vec<CompiledSchema> = Vec::new();
    for provide in fields.provides {
        let Some(path) = &provide.schema else {
            continue;
        };
        let namespace = provide
            .id
            .strip_prefix("field:")
            .ok_or_else(|| InstallError::Manifest("catalog.mirror at /provides".into()))?;
        let version = semver::Version::parse(&provide.version)
            .map_err(|_| InstallError::InvalidAsset(path.clone()))?;
        let schema_text = preflight::read_asset(root, path, schema::MAX_BYTES)?;
        compiled.push(
            schema::compile(namespace, &version, provenance, &schema_text, common_defs)
                .map_err(|_| InstallError::InvalidAsset(path.clone()))?,
        );
    }
    compare_package(kind, entry, &manifest, &compiled).map_err(mismatch)?;
    for art in [fields.icon, fields.banner].into_iter().flatten() {
        preflight::confined(root, art)?;
    }
    let mut process_digests = None;
    match &manifest {
        PackageManifest::Plugin(plugin) => {
            let library = plugin
                .targets
                .get(target)
                .ok_or(InstallError::MissingTarget)?;
            let path = preflight::confined(root, &library.library)?;
            if plugin.runtime == vosu_plugin_api::manifest::Runtime::Process {
                process_digests = Some(
                    Digests::files(Sha256Digest(Sha256::digest(text.as_bytes()).into()), &path)
                        .map_err(|_| InstallError::InvalidAsset(library.library.clone()))?,
                );
            }
            if let Some(path) = &plugin.settings_schema {
                let settings = preflight::read_asset(root, path, MAX_SETTINGS_SCHEMA_BYTES)?;
                SettingsSchema::parse(&settings)
                    .map_err(|_| InstallError::InvalidAsset(path.clone()))?;
            }
            for action in &plugin.actions {
                for path in [&action.args_schema, &action.result_schema] {
                    let text = preflight::read_asset(root, path, action_schema::MAX_SCHEMA_BYTES)?;
                    CompiledActionSchema::compile(text)
                        .map_err(|_| InstallError::InvalidAsset(path.clone()))?;
                }
            }
        }
        PackageManifest::Tool(tool) => {
            preflight::confined(root, &format!("{}/{}", tool.ui.root, tool.ui.entry))?;
        }
        PackageManifest::Overlay(_) => {
            preflight::confined(root, "index.html")?;
        }
    }
    Ok(process_digests)
}
