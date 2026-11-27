//! Versioned normal-root migration journal (`<config>/migration-v2-journal.json`,
//! A8.2 / A8.3): records raw input fingerprints, validated legacy snapshots,
//! per-step durable receipts, the final `local_conversion_complete` marker, and
//! independent offline/network `pending_intents`.
//!
//! Publication uses `vosu_sys::platform::atomic_replace` (closing the temp
//! handle before Windows replacement and syncing the Unix parent directory) and
//! reads go through `vosu_sys::platform::read_published` with strict size caps
//! both before and after reading. Corrupt, oversized, unknown-version, or
//! invariant-violating journals fail closed without overwriting or resetting.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::catalog::sources::SourceInput;

use super::MigrationError;

/// Journal filename inside the normal-root config directory.
pub const JOURNAL_FILENAME: &str = "migration-v2-journal.json";
/// Current on-disk journal schema version.
pub const JOURNAL_VERSION: u32 = 1;
/// Hard size cap on `migration-v2-journal.json` (2 MiB).
pub const MAX_JOURNAL_BYTES: u64 = 2 * 1024 * 1024;
/// Hard cap on tracked pending intents in one journal.
pub const MAX_PENDING_INTENTS: usize = 256;

/// On-disk normal-root migration journal (format version 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationJournal {
    pub version: u32,
    pub generation: u64,
    pub snapshot: MigrationSnapshot,
    pub plugin_settings: StepState<PluginSettingsReceipt>,
    pub core_config: StepState<CoreConfigReceipt>,
    pub catalog_sources: StepState<CatalogSourcesReceipt>,
    pub user_tools: StepState<UserToolsReceipt>,
    pub local_conversion_complete: bool,
    pub pending_intents: Vec<PendingIntent>,
}

impl MigrationJournal {
    pub(crate) fn new(snapshot: MigrationSnapshot, pending_intents: Vec<PendingIntent>) -> Self {
        Self {
            version: JOURNAL_VERSION,
            generation: 0,
            snapshot,
            plugin_settings: StepState::Pending,
            core_config: StepState::Pending,
            catalog_sources: StepState::Pending,
            user_tools: StepState::Pending,
            local_conversion_complete: false,
            pending_intents,
        }
    }

    pub(crate) fn all_local_steps_completed(&self) -> bool {
        self.plugin_settings.is_completed()
            && self.core_config.is_completed()
            && self.catalog_sources.is_completed()
            && self.user_tools.is_completed()
    }
}

/// Per-step durable receipt state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    content = "receipt",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum StepState<T> {
    Pending,
    Completed(T),
}

impl<T> StepState<T> {
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed(_))
    }

    pub fn receipt(&self) -> Option<&T> {
        match self {
            Self::Pending => None,
            Self::Completed(receipt) => Some(receipt),
        }
    }
}

/// Raw SHA-256 fingerprint of a logical legacy source input captured before any
/// destructive write. Never stores full filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFingerprint {
    pub present: bool,
    pub size_bytes: u64,
    pub sha256: Option<String>,
}

impl SourceFingerprint {
    pub(crate) fn absent() -> Self {
        Self {
            present: false,
            size_bytes: 0,
            sha256: None,
        }
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            present: true,
            size_bytes: bytes.len() as u64,
            sha256: Some(sha256_hex(bytes)),
        }
    }
}

/// Which legacy `.env` file was present and readable at snapshot time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyEnvKind {
    VosuEnv,
    PreRenameEnv,
}

/// Validated canonical `vosu.toml` snapshot captured before any destructive write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreConfigSnapshot {
    /// Canonical stripped `vosu.toml` content to publish, or `None` when no
    /// legacy config/env input required rewriting `vosu.toml`.
    pub canonical_toml: Option<String>,
    /// Sorted moved + retired top-level keys stripped from legacy inputs.
    pub stripped_keys: Vec<String>,
    /// Legacy `.env` files readable at snapshot time, removed only after
    /// canonical `vosu.toml` is durable.
    pub legacy_env_files: Vec<LegacyEnvKind>,
}

/// Official 1.x tool discovered in `tools/state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveredOfficialTool {
    pub id: String,
    pub official_source: String,
}

/// Official 1.x overlay discovered in `tools/overlay-catalog-state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveredOfficialOverlay {
    pub id: String,
    pub official_source: String,
    pub version: String,
}

/// Complete validated pre-mutation snapshot recorded in the journal so crash
/// replay resumes solely from committed inputs without rereading mutated files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationSnapshot {
    pub fingerprints: BTreeMap<String, SourceFingerprint>,
    pub osu_overrides: Map<String, Value>,
    pub overlay_host_overrides: Map<String, Value>,
    pub core_config: CoreConfigSnapshot,
    pub catalog_sources: Option<Vec<SourceInput>>,
    pub user_tools_to_disable: Vec<String>,
    pub discovered_official_tools: Vec<DiscoveredOfficialTool>,
    pub discovered_official_overlays: Vec<DiscoveredOfficialOverlay>,
}

/// Durable receipt for Step 1 (`plugins/settings.json` import).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSettingsReceipt {
    pub osu_imported_keys: Vec<String>,
    pub osu_preserved_keys: Vec<String>,
    pub overlay_host_imported_keys: Vec<String>,
    pub overlay_host_preserved_keys: Vec<String>,
}

/// Durable receipt for Step 2 (canonical `vosu.toml` + legacy env cleanup).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreConfigReceipt {
    pub wrote_toml: bool,
    pub stripped_keys: Vec<String>,
    pub removed_env_files: Vec<LegacyEnvKind>,
}

/// Durable receipt for Step 3 (`catalog-source-settings.json` publication).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSourcesReceipt {
    pub migrated: bool,
    pub source_count: usize,
    pub trusted_official_count: usize,
}

/// Durable receipt for Step 4 (`tools/state.json` user-tool disablement).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserToolsReceipt {
    pub disabled_user_tools: Vec<String>,
}

/// Kind of post-local-conversion pending intent (A8.2 / AF.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    OfficialToolReplacement,
    OfficialOverlayReplacement,
    Af2CrossKindTransfer,
}

/// Closed AF.2 cross-kind target metadata attached to an `Af2CrossKindTransfer`
/// intent (`docs/v2/ecosystem-release-matrix.md` §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Af2TransferSpec {
    pub target_plugin: Option<String>,
    pub target_web_tool: Option<String>,
    pub copy_durable_data: bool,
}

/// Independent lifecycle state for a single `PendingIntent` (A8.2 / A8.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum IntentStatus {
    Pending,
    Conflict { candidates: Vec<String> },
    Collision { path_hint: String },
    Unmanaged,
    Completed,
    RetryableError { message: String },
}

/// One deduplicated replacement or AF.2 cross-kind transfer intent persisted in
/// the migration journal for explicit refresh/plan/resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingIntent {
    pub intent_id: String,
    pub kind: IntentKind,
    pub legacy_id: String,
    pub official_source: String,
    pub resolved_target_id: Option<String>,
    pub af2_transfer: Option<Af2TransferSpec>,
    pub status: IntentStatus,
}

/// Returns `<config_root>/migration-v2-journal.json`.
pub fn journal_path(config_root: &Path) -> PathBuf {
    config_root.join(JOURNAL_FILENAME)
}

/// Reads and validates `<config_root>/migration-v2-journal.json` through
/// `vosu_sys::platform::read_published`. Returns `Ok(None)` if absent and fails
/// closed on oversized, corrupt, unknown-version, or invariant-violating files.
pub fn load_journal(config_root: &Path) -> Result<Option<MigrationJournal>, MigrationError> {
    let path = journal_path(config_root);
    let Some(bytes) = read_published_bounded(&path, MAX_JOURNAL_BYTES, JOURNAL_FILENAME)? else {
        return Ok(None);
    };
    let journal: MigrationJournal =
        serde_json::from_slice(&bytes).map_err(|_| MigrationError::JournalCorrupt)?;
    if journal.version != JOURNAL_VERSION {
        return Err(MigrationError::JournalUnsupportedVersion(journal.version));
    }
    if journal.generation == 0 || journal.pending_intents.len() > MAX_PENDING_INTENTS {
        return Err(MigrationError::JournalCorrupt);
    }
    if journal.local_conversion_complete && !journal.all_local_steps_completed() {
        return Err(MigrationError::JournalCorrupt);
    }
    Ok(Some(journal))
}

/// Increments `journal.generation` and publishes `<config_root>/migration-v2-journal.json`
/// durably via `vosu_sys::platform::atomic_replace`.
pub fn save_journal(
    config_root: &Path,
    journal: &mut MigrationJournal,
) -> Result<(), MigrationError> {
    if journal.version != JOURNAL_VERSION {
        return Err(MigrationError::JournalUnsupportedVersion(journal.version));
    }
    if journal.pending_intents.len() > MAX_PENDING_INTENTS {
        return Err(MigrationError::JournalOversized);
    }
    if journal.local_conversion_complete && !journal.all_local_steps_completed() {
        return Err(MigrationError::JournalCorrupt);
    }

    let next_generation = journal
        .generation
        .checked_add(1)
        .ok_or(MigrationError::JournalCorrupt)?;
    let mut candidate = journal.clone();
    candidate.generation = next_generation;

    let mut bytes =
        serde_json::to_vec_pretty(&candidate).map_err(|_| MigrationError::JournalSerialize)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(MigrationError::JournalOversized);
    }

    let path = journal_path(config_root);
    atomic_publish_bytes(&path, &bytes, JOURNAL_FILENAME)?;
    journal.generation = next_generation;
    Ok(())
}

/// Computes lowercase hex SHA-256 of `bytes`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// Rejects replacing a symlink at `path` so migration never follows a symlink
/// outside the normal root.
pub(crate) fn reject_target_symlink(
    path: &Path,
    label: &'static str,
) -> Result<(), MigrationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(MigrationError::SymlinkTarget { label })
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MigrationError::Io {
            label,
            kind: error.kind(),
        }),
    }
}

/// Reads an `atomic_replace`-published file with strict pre- and post-read byte
/// caps via `vosu_sys::platform::read_published`.
pub(crate) fn read_published_bounded(
    path: &Path,
    max_bytes: u64,
    label: &'static str,
) -> Result<Option<Vec<u8>>, MigrationError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > max_bytes => {
            return Err(MigrationError::InputOversized { label, max_bytes });
        }
        Ok(_) => {}
        // Delegate `NotFound` to `read_published` so Windows absorbs any
        // transient cross-directory `atomic_replace` missing-name window.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(MigrationError::Io {
                label,
                kind: error.kind(),
            });
        }
    }
    let bytes = match vosu_sys::platform::read_published(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(MigrationError::Io {
                label,
                kind: error.kind(),
            });
        }
    };
    if bytes.len() as u64 > max_bytes {
        return Err(MigrationError::InputOversized { label, max_bytes });
    }
    Ok(Some(bytes))
}

/// Reads a plain non-`atomic_replace` file (such as legacy `.env`) with strict
/// pre- and post-read byte caps.
pub(crate) fn read_plain_bounded(
    path: &Path,
    max_bytes: u64,
    label: &'static str,
) -> Result<Option<Vec<u8>>, MigrationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(MigrationError::SymlinkTarget { label });
        }
        Ok(metadata) if metadata.len() > max_bytes => {
            return Err(MigrationError::InputOversized { label, max_bytes });
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(MigrationError::Io {
                label,
                kind: error.kind(),
            });
        }
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(MigrationError::Io {
                label,
                kind: error.kind(),
            });
        }
    };
    if bytes.len() as u64 > max_bytes {
        return Err(MigrationError::InputOversized { label, max_bytes });
    }
    Ok(Some(bytes))
}

/// Atomically publishes `bytes` to `path` via `NamedTempFile::new_in`,
/// `write_all`, `flush`, `sync_all`, `into_temp_path()` (closing the handle
/// before Windows replacement), `vosu_sys::platform::atomic_replace`, and Unix
/// parent-directory `sync_all`.
pub(crate) fn atomic_publish_bytes(
    path: &Path,
    bytes: &[u8],
    label: &'static str,
) -> Result<(), MigrationError> {
    reject_target_symlink(path, label)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(MigrationError::Io {
            label,
            kind: io::ErrorKind::InvalidInput,
        })?;
    fs::create_dir_all(parent).map_err(|error| MigrationError::Io {
        label,
        kind: error.kind(),
    })?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| MigrationError::Io {
            label,
            kind: error.kind(),
        })?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| MigrationError::Io {
            label,
            kind: error.kind(),
        })?;
    // Close the replacement file handle before Windows `atomic_replace`.
    let temp = temporary.into_temp_path();
    reject_target_symlink(path, label)?;
    vosu_sys::platform::atomic_replace(&temp, path).map_err(|error| MigrationError::Io {
        label,
        kind: error.kind(),
    })?;
    sync_parent_dir(parent, label)?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn sync_parent_dir(parent: &Path, label: &'static str) -> Result<(), MigrationError> {
    fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| MigrationError::Io {
            label,
            kind: error.kind(),
        })
}

#[cfg(not(unix))]
pub(crate) fn sync_parent_dir(_parent: &Path, _label: &'static str) -> Result<(), MigrationError> {
    Ok(())
}
