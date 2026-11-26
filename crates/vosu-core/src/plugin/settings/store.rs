//! Core's per-plugin settings storage (`docs/v2/plugin-abi.md` §6.9 "M7
//! storage and editing", A7.1): every plugin's stored overrides in one
//! `<config_root>/plugins/settings.json`, beside the approval store. Values
//! are stored as overrides of the schema's defaults, never as a frozen
//! effective object, so a plugin update's new defaults apply to keys the
//! user never set. Bridge B2 (`vosu.osu`) and the `vosu.overlayhost` seed
//! keep their own storage until M8; the root composition routes only every
//! other plugin id here.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{SettingsError, SettingsStorage};

const SETTINGS_FILE: &str = "plugins/settings.json";
const FILE_VERSION: u32 = 1;
/// Upper bound on the whole document, every plugin's overrides together.
const MAX_FILE_BYTES: u64 = 4 << 20;

type Plugins = BTreeMap<String, Map<String, Value>>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    version: u32,
    plugins: Plugins,
}

/// File-backed [`SettingsStorage`] for every plugin id the root routes to
/// it. A document that fails to read is never rewritten: every load and
/// store then fails with [`SettingsError::Storage`], and a plugin's
/// `init.settings` falls back to its schema defaults.
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
    /// Reads `<config_root>/plugins/settings.json`; a missing file is an
    /// empty store.
    pub fn open(config_root: &Path) -> Self {
        let path = config_root.join(SETTINGS_FILE);
        let plugins = read(&path).map_err(|error| {
            let message = format!("{} is unreadable: {error}", path.display());
            tracing::warn!("{message}; plugin settings use their defaults and cannot be saved");
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
        write(&self.path, &next).map_err(|error| {
            SettingsError::Storage(format!("plugin settings were not saved: {error}"))
        })?;
        *plugins = Ok(next);
        Ok(())
    }
}

fn read(path: &Path) -> io::Result<Plugins> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Plugins::new()),
        Err(error) => return Err(error),
    };
    if metadata.len() > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "larger than the plugin settings limit",
        ));
    }
    // Published through tempfile `persist` by `write` below, the approval
    // store's precedent: outside `read_published`'s scope.
    let bytes = std::fs::read(path)?;
    let file: SettingsFile = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if file.version != FILE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported version {}", file.version),
        ));
    }
    Ok(file.plugins)
}

fn write(path: &Path, plugins: &Plugins) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("plugin settings need a parent directory"))?;
    let bytes = serde_json::to_vec_pretty(&SettingsFile {
        version: FILE_VERSION,
        plugins: plugins.clone(),
    })?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::other("larger than the plugin settings limit"));
    }
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
