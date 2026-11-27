//! Core's permanent per-plugin settings storage (`docs/v2/plugin-abi.md` §6.9,
//! A7.1, A8.1, A8.3): every plugin's stored overrides live in one channel-aware
//! `<config_root>/plugins/settings.json`, beside the approval store.
//!
//! Values are stored as explicit overrides of each plugin's schema defaults,
//! never as a frozen effective object, so a plugin update's new defaults apply
//! to keys the user never set while explicit `false`/`0`/`""` overrides are
//! preserved verbatim. Publication uses `vosu_sys::platform::atomic_replace`
//! (with temp handles closed before Windows replacement) and reads go through
//! `vosu_sys::platform::read_published` with strict size caps both before and
//! after reading.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use vosu_plugin_api::manifest::settings::SettingsSchema;

use super::import::{ImportOutcome, plan_import};
use super::{SettingsError, SettingsStorage};

const SETTINGS_DIR: &str = "plugins";
const SETTINGS_FILENAME: &str = "settings.json";
const FILE_VERSION: u32 = 1;
/// Upper bound on the whole document, every plugin's overrides together (4 MiB).
pub const MAX_FILE_BYTES: u64 = 4 << 20;

type Plugins = BTreeMap<String, Map<String, Value>>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    version: u32,
    plugins: Plugins,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadFailure {
    Oversized,
    Corrupt,
    UnsupportedVersion(u32),
    Io(io::ErrorKind),
}

impl ReadFailure {
    /// Actionable, redacted diagnostic: never includes raw file contents,
    /// unknown field names, setting values, or full local filesystem paths.
    fn redacted_message(self) -> String {
        match self {
            Self::Oversized => {
                "plugins/settings.json exceeds the 4 MiB plugin settings size limit; repair or remove it before saving settings"
                    .to_owned()
            }
            Self::Corrupt => {
                "plugins/settings.json is malformed or corrupt; repair or remove it before saving settings"
                    .to_owned()
            }
            Self::UnsupportedVersion(version) => format!(
                "plugins/settings.json uses unsupported version {version}; repair or remove it before saving settings"
            ),
            Self::Io(kind) => format!(
                "plugins/settings.json could not be read ({kind}); check permissions and repair or remove it before saving settings"
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteFailure {
    MissingParent,
    Oversized,
    Serialize,
    Io(io::ErrorKind),
}

impl WriteFailure {
    fn redacted_message(self) -> String {
        match self {
            Self::MissingParent => {
                "plugin settings were not saved: settings path has no parent directory".to_owned()
            }
            Self::Oversized => {
                "plugin settings were not saved: document exceeds the 4 MiB plugin settings limit"
                    .to_owned()
            }
            Self::Serialize => {
                "plugin settings were not saved: failed to serialize settings document".to_owned()
            }
            Self::Io(kind) => format!("plugin settings were not saved: filesystem error ({kind})"),
        }
    }
}

/// File-backed [`SettingsStorage`] for every settings-bearing plugin under one
/// channel-aware `config_root`.
///
/// A document that fails to read (corrupt, unsupported version, oversized, or
/// unreadable) enters a fail-closed `Err(String)` state and is never
/// overwritten or reset: every load, store, and import then fails with a
/// redacted actionable [`SettingsError::Storage`].
///
/// ## Lock discipline
/// `plugins` is this store's sole internal mutex. It is never held across
/// plugin callbacks or `.await` points, and acquires no other host lock.
pub struct PluginSettingsStore {
    path: PathBuf,
    plugins: Mutex<Result<Plugins, String>>,
}

impl std::fmt::Debug for PluginSettingsStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginSettingsStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl PluginSettingsStore {
    /// Opens `<config_root>/plugins/settings.json`; a missing file opens as an
    /// empty store. Pass the channel-resolved config root (`Paths::config()`)
    /// so release and prerelease roots remain strictly isolated.
    pub fn open(config_root: &Path) -> Self {
        let path = config_root.join(SETTINGS_DIR).join(SETTINGS_FILENAME);
        let plugins = read_with_limit(&path, MAX_FILE_BYTES).map_err(|failure| {
            let message = failure.redacted_message();
            tracing::warn!(
                path = %path.display(),
                reason = %message,
                "plugin settings use their defaults and cannot be saved"
            );
            message
        });
        Self {
            path,
            plugins: Mutex::new(plugins),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> MutexGuard<'_, Result<Plugins, String>> {
        self.plugins.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Idempotently imports explicitly-present `legacy_overrides` for
    /// `plugin_id` directly against this store (A8.1 / A8.3).
    ///
    /// - Fails closed without writing if the store is unreadable/corrupt.
    /// - Validates every candidate legacy value against `schema` before any
    ///   write; malformed input fails closed without creating or modifying the
    ///   store file.
    /// - Existing valid per-plugin overrides always win per key; absent keys
    ///   are imported verbatim (preserving explicit `false`, `0`, `0.0`, `""`).
    /// - Durable `atomic_replace` precedes updating in-memory state and
    ///   returning [`ImportOutcome`].
    pub fn import_missing_overrides(
        &self,
        plugin_id: &str,
        schema: &SettingsSchema,
        legacy_overrides: &Map<String, Value>,
    ) -> Result<ImportOutcome, SettingsError> {
        let mut plugins = self.lock();
        let current = plugins
            .as_ref()
            .map_err(|error| SettingsError::Storage(error.clone()))?;
        let existing = current.get(plugin_id).cloned().unwrap_or_default();
        let plan = plan_import(schema, existing, legacy_overrides)?;
        if !plan.imported_keys.is_empty() {
            let mut next = current.clone();
            next.insert(plugin_id.to_owned(), plan.next_overrides);
            write_with_limit(&self.path, &next, MAX_FILE_BYTES)
                .map_err(|failure| SettingsError::Storage(failure.redacted_message()))?;
            *plugins = Ok(next);
        }
        Ok(ImportOutcome {
            imported_keys: plan.imported_keys,
            preserved_existing_keys: plan.preserved_existing_keys,
            effective: plan.effective,
        })
    }
}

impl SettingsStorage for PluginSettingsStore {
    fn load(&self, plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
        let plugins = self.lock();
        let plugins = plugins
            .as_ref()
            .map_err(|error| SettingsError::Storage(error.clone()))?;
        Ok(plugins.get(plugin_id).cloned().unwrap_or_default())
    }

    /// Replaces the file atomically in its own directory before the
    /// in-memory copy changes, so a failed write leaves both unchanged.
    fn store(&self, plugin_id: &str, values: &Map<String, Value>) -> Result<(), SettingsError> {
        let mut plugins = self.lock();
        let current = plugins
            .as_ref()
            .map_err(|error| SettingsError::Storage(error.clone()))?;
        let mut next = current.clone();
        if values.is_empty() {
            next.remove(plugin_id);
        } else {
            next.insert(plugin_id.to_owned(), values.clone());
        }
        write_with_limit(&self.path, &next, MAX_FILE_BYTES)
            .map_err(|failure| SettingsError::Storage(failure.redacted_message()))?;
        *plugins = Ok(next);
        Ok(())
    }
}

fn parse_bytes_with_limit(bytes: &[u8], max_bytes: u64) -> Result<Plugins, ReadFailure> {
    if bytes.len() as u64 > max_bytes {
        return Err(ReadFailure::Oversized);
    }
    let file: SettingsFile = serde_json::from_slice(bytes).map_err(|_| ReadFailure::Corrupt)?;
    if file.version != FILE_VERSION {
        return Err(ReadFailure::UnsupportedVersion(file.version));
    }
    Ok(file.plugins)
}

fn read_with_limit(path: &Path, max_bytes: u64) -> Result<Plugins, ReadFailure> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.len() > max_bytes => return Err(ReadFailure::Oversized),
        Ok(_) => {}
        // Still delegate `NotFound` to `read_published` so Windows absorbs any
        // transient cross-directory `atomic_replace` missing-name window.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(ReadFailure::Io(error.kind())),
    }
    let bytes = match vosu_sys::platform::read_published(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Plugins::new()),
        Err(error) => return Err(ReadFailure::Io(error.kind())),
    };
    parse_bytes_with_limit(&bytes, max_bytes)
}

fn write_with_limit(path: &Path, plugins: &Plugins, max_bytes: u64) -> Result<(), WriteFailure> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(WriteFailure::MissingParent)?;
    let mut bytes = serde_json::to_vec_pretty(&SettingsFile {
        version: FILE_VERSION,
        plugins: plugins.clone(),
    })
    .map_err(|_| WriteFailure::Serialize)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > max_bytes {
        return Err(WriteFailure::Oversized);
    }
    std::fs::create_dir_all(parent).map_err(|error| WriteFailure::Io(error.kind()))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| WriteFailure::Io(error.kind()))?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| WriteFailure::Io(error.kind()))?;
    // Close the replacement file handle before `atomic_replace` (required on
    // Windows; `TempPath`'s `Drop` removes the temp file if replace fails).
    let temp = temporary.into_temp_path();
    vosu_sys::platform::atomic_replace(&temp, path)
        .map_err(|error| WriteFailure::Io(error.kind()))?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| WriteFailure::Io(error.kind()))?;
    Ok(())
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
