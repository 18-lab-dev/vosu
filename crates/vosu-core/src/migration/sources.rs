//! Verbatim 1.x catalog sources migration (`docs/v2/catalog-v2.md`, A8.2 / A8.3).
//!
//! Reads the 1.x `catalog-sources.json` list (`<config>/tools/catalog-sources.json`
//! or fallback `<config>/catalog-sources.json`), validates every entry before any
//! destructive write, and publishes verbatim source strings, `enabled` flags, and
//! exact order to `<config>/catalog-source-settings.json` with:
//! - `trusted: true` ONLY for exact fixed official identities (`OFFICIAL_SOURCES`)
//! - `trusted: false` for every other migrated source (never URL aliases or substrings)
//! - `private_network: false` for all migrated sources
//! - zero invented or imported credentials.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::catalog::sources::{MAX_SOURCES, SourceIdentity, SourceInput, publish_migrated_records};

use super::MigrationError;
use super::journal::{CatalogSourcesReceipt, SourceFingerprint, read_published_bounded};

/// Maximum legacy `catalog-sources.json` size (512 KiB, matching `SourceStore`).
pub const MAX_LEGACY_SOURCES_BYTES: u64 = 512 * 1024;

/// Output of snapshotting 1.x `catalog-sources.json` before any mutation.
pub(crate) struct SourcesSnapshotOutput {
    pub(crate) fingerprints: BTreeMap<String, SourceFingerprint>,
    pub(crate) inputs: Option<Vec<SourceInput>>,
}

/// Reads and validates the 1.x catalog sources list without writing to disk.
///
/// Checks `<config>/tools/catalog-sources.json` (the 1.x `ToolRegistry` path)
/// first, then falls back to `<config>/catalog-sources.json` when absent.
/// Fails closed on malformed JSON, unknown fields, duplicate sources, unsafe
/// source identities, or exceeding `MAX_SOURCES`.
pub(crate) fn snapshot_legacy_sources(
    config_root: &Path,
) -> Result<SourcesSnapshotOutput, MigrationError> {
    let tools_path = config_root.join("tools").join("catalog-sources.json");
    let root_path = config_root.join("catalog-sources.json");

    let tools_bytes = read_published_bounded(
        &tools_path,
        MAX_LEGACY_SOURCES_BYTES,
        "tools/catalog-sources.json",
    )?;
    let root_bytes = if tools_bytes.is_none() {
        read_published_bounded(&root_path, MAX_LEGACY_SOURCES_BYTES, "catalog-sources.json")?
    } else {
        None
    };

    let mut fingerprints = BTreeMap::new();
    let (label, bytes) = if let Some(bytes) = tools_bytes {
        fingerprints.insert(
            "tools/catalog-sources.json".to_owned(),
            SourceFingerprint::from_bytes(&bytes),
        );
        ("tools/catalog-sources.json", Some(bytes))
    } else if let Some(bytes) = root_bytes {
        fingerprints.insert(
            "catalog-sources.json".to_owned(),
            SourceFingerprint::from_bytes(&bytes),
        );
        ("catalog-sources.json", Some(bytes))
    } else {
        fingerprints.insert(
            "tools/catalog-sources.json".to_owned(),
            SourceFingerprint::absent(),
        );
        ("tools/catalog-sources.json", None)
    };

    let Some(bytes) = bytes else {
        return Ok(SourcesSnapshotOutput {
            fingerprints,
            inputs: None,
        });
    };

    let inputs: Vec<SourceInput> =
        serde_json::from_slice(&bytes).map_err(|_| MigrationError::MalformedInput { label })?;
    validate_source_inputs(&inputs, label)?;

    Ok(SourcesSnapshotOutput {
        fingerprints,
        inputs: Some(inputs),
    })
}

/// Validates a slice of 1.x `SourceInput` records against `SourceStore` rules.
fn validate_source_inputs(
    inputs: &[SourceInput],
    label: &'static str,
) -> Result<(), MigrationError> {
    if inputs.len() > MAX_SOURCES {
        return Err(MigrationError::MalformedInput { label });
    }
    let mut seen = HashSet::with_capacity(inputs.len());
    for entry in inputs {
        SourceIdentity::parse(entry.source.clone())
            .map_err(|_| MigrationError::MalformedInput { label })?;
        if !seen.insert(entry.source.as_str()) {
            return Err(MigrationError::MalformedInput { label });
        }
    }
    Ok(())
}

/// Publishes snapshotted 1.x sources to `<config>/catalog-source-settings.json`,
/// or records an untouched no-op receipt when `inputs` is `None`.
pub(crate) fn apply_migrated_sources(
    config_root: &Path,
    inputs: Option<&[SourceInput]>,
) -> Result<CatalogSourcesReceipt, MigrationError> {
    let Some(inputs) = inputs else {
        return Ok(CatalogSourcesReceipt {
            migrated: false,
            source_count: 0,
            trusted_official_count: 0,
        });
    };

    let records = publish_migrated_records(config_root, inputs)
        .map_err(|_| MigrationError::SourcesPublishFailed)?;
    let trusted_official_count = records.iter().filter(|record| record.trusted).count();
    Ok(CatalogSourcesReceipt {
        migrated: true,
        source_count: records.len(),
        trusted_official_count,
    })
}
