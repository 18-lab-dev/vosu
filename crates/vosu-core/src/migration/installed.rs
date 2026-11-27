//! Discovery and local conversion of 1.x user tools, official tools, and
//! official overlays (`docs/v2/catalog-v2.md`, A8.2 / A8.3 / AF.2).
//!
//! - Inspects `<config>/tools/state.json`, `<config>/tools/overlay-catalog-state.json`,
//!   and `<data>/tools/<id>/` (identifying 1.x `tool.toml`-only directories
//!   without `manifest.toml`, never parsing `tool.toml`).
//! - Persists `enabled: Some(false)` into `<config>/tools/state.json` via
//!   `atomic_replace` for every 1.x user tool while keeping `<data>/tool-data/<id>/`
//!   and `<config>/tools/settings/<id>.json` completely untouched.
//! - Leaves user overlays untouched and enabled.
//! - Discovers installed official 1.x tools and overlays for pending replacement
//!   and AF.2 cross-kind transfer intents.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::sources::OFFICIAL_SOURCES;

use super::MigrationError;
use super::journal::{
    DiscoveredOfficialOverlay, DiscoveredOfficialTool, SourceFingerprint, UserToolsReceipt,
    atomic_publish_bytes, read_published_bounded,
};

/// Size cap on `tools/state.json` and `tools/overlay-catalog-state.json` (2 MiB).
pub const MAX_TOOL_STATE_BYTES: u64 = 2 * 1024 * 1024;
/// Upper bound on scanned directories under `<data>/tools/`.
const MAX_TOOL_DIRS: usize = 512;

/// Exact 1.x `tools/state.json` per-tool state layout (preserving any extra
/// fields verbatim so migration never silently drops unrelated metadata).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct SavedToolState {
    pub(crate) enabled: Option<bool>,
    #[serde(default)]
    pub(crate) trusted: bool,
    #[serde(default)]
    pub(crate) official: bool,
    #[serde(default)]
    pub(crate) catalog_source: Option<String>,
    #[serde(default)]
    pub(crate) autostart: Option<bool>,
    #[serde(default)]
    pub(crate) replaces: Vec<String>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

/// Exact 1.x `tools/overlay-catalog-state.json` entry layout.
#[derive(Debug, Clone, Deserialize)]
struct SavedOverlayState {
    source: String,
    #[serde(default)]
    version: String,
}

/// Output of scanning 1.x installed tools/overlays before any destructive write.
pub(crate) struct InstalledSnapshotOutput {
    pub(crate) fingerprints: BTreeMap<String, SourceFingerprint>,
    pub(crate) user_tools_to_disable: Vec<String>,
    pub(crate) official_tools: Vec<DiscoveredOfficialTool>,
    pub(crate) official_overlays: Vec<DiscoveredOfficialOverlay>,
}

/// Matches ONLY the three exact fixed official identities (`OFFICIAL_SOURCES`),
/// never URL aliases, tagged forms, case variants, or substrings.
pub(crate) fn canonical_official_source(source: &str) -> Option<&'static str> {
    OFFICIAL_SOURCES
        .iter()
        .copied()
        .find(|official| *official == source)
}

/// Scans `<config>/tools/state.json`, `<config>/tools/overlay-catalog-state.json`,
/// and `<data>/tools/<id>/` before any mutation.
pub(crate) fn snapshot_installed_legacy(
    config_root: &Path,
    data_root: &Path,
) -> Result<InstalledSnapshotOutput, MigrationError> {
    let tool_state_path = config_root.join("tools").join("state.json");
    let overlay_state_path = config_root.join("tools").join("overlay-catalog-state.json");

    let tool_state_bytes =
        read_published_bounded(&tool_state_path, MAX_TOOL_STATE_BYTES, "tools/state.json")?;
    let overlay_state_bytes = read_published_bounded(
        &overlay_state_path,
        MAX_TOOL_STATE_BYTES,
        "tools/overlay-catalog-state.json",
    )?;

    let mut fingerprints = BTreeMap::new();
    fingerprints.insert(
        "tools/state.json".to_owned(),
        tool_state_bytes
            .as_deref()
            .map_or_else(SourceFingerprint::absent, SourceFingerprint::from_bytes),
    );
    fingerprints.insert(
        "tools/overlay-catalog-state.json".to_owned(),
        overlay_state_bytes
            .as_deref()
            .map_or_else(SourceFingerprint::absent, SourceFingerprint::from_bytes),
    );

    let tool_states: BTreeMap<String, SavedToolState> = match tool_state_bytes {
        Some(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| MigrationError::MalformedInput {
                label: "tools/state.json",
            })?
        }
        None => BTreeMap::new(),
    };

    let overlay_states: BTreeMap<String, SavedOverlayState> = match overlay_state_bytes {
        Some(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| MigrationError::MalformedInput {
                label: "tools/overlay-catalog-state.json",
            })?
        }
        None => BTreeMap::new(),
    };

    let tools_dir = data_root.join("tools");
    let (legacy_tool_dirs, v2_tool_dirs) = scan_tool_directories(&tools_dir)?;

    // Determine official 1.x tools from `tools/state.json`.
    let mut official_tool_ids = BTreeSet::new();
    let mut official_tools = Vec::new();
    for (id, state) in &tool_states {
        if v2_tool_dirs.contains(id) {
            continue;
        }
        let official_source = match state.catalog_source.as_deref() {
            Some(source) => canonical_official_source(source),
            None if state.official => Some(if id == "vosulay" {
                "18-lab-dev/vosulay"
            } else {
                "18-lab-dev/vosu-tools"
            }),
            None => None,
        };
        if let Some(source) = official_source {
            official_tool_ids.insert(id.clone());
            official_tools.push(DiscoveredOfficialTool {
                id: id.clone(),
                official_source: source.to_owned(),
            });
        }
    }
    official_tools.sort_by(|left, right| left.id.cmp(&right.id));

    // Every 1.x `tool.toml`-only directory that is NOT an official tool is a
    // 1.x user tool that must persist `enabled: Some(false)`.
    let mut user_tools_to_disable: Vec<String> = legacy_tool_dirs
        .into_iter()
        .filter(|id| !official_tool_ids.contains(id))
        .collect();
    user_tools_to_disable.sort();
    user_tools_to_disable.dedup();

    // Official 1.x overlays from `tools/overlay-catalog-state.json`.
    // Non-official / user overlays are left completely untouched.
    let mut official_overlays = Vec::new();
    for (id, state) in &overlay_states {
        if let Some(source) = canonical_official_source(&state.source) {
            official_overlays.push(DiscoveredOfficialOverlay {
                id: id.clone(),
                official_source: source.to_owned(),
                version: state.version.clone(),
            });
        }
    }
    official_overlays.sort_by(|left, right| left.id.cmp(&right.id));

    Ok(InstalledSnapshotOutput {
        fingerprints,
        user_tools_to_disable,
        official_tools,
        official_overlays,
    })
}

/// Scans direct child directories under `<data>/tools/`.
/// Returns `(legacy_1x_tool_ids, v2_tool_ids)`:
/// - A 1.x tool directory has `tool.toml` and NO `manifest.toml` (never parses `tool.toml`).
/// - A 2.0 tool directory has `manifest.toml`.
fn scan_tool_directories(
    tools_dir: &Path,
) -> Result<(BTreeSet<String>, BTreeSet<String>), MigrationError> {
    let entries = match fs::read_dir(tools_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((BTreeSet::new(), BTreeSet::new()));
        }
        Err(error) => {
            return Err(MigrationError::Io {
                label: "data/tools",
                kind: error.kind(),
            });
        }
    };

    let mut legacy = BTreeSet::new();
    let mut v2 = BTreeSet::new();
    let mut count = 0usize;
    for entry in entries {
        count += 1;
        if count > MAX_TOOL_DIRS {
            return Err(MigrationError::MalformedInput {
                label: "data/tools",
            });
        }
        let entry = entry.map_err(|error| MigrationError::Io {
            label: "data/tools",
            kind: error.kind(),
        })?;
        let file_type = entry.file_type().map_err(|error| MigrationError::Io {
            label: "data/tools",
            kind: error.kind(),
        })?;
        if !file_type.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // Ignore hidden staging/backup directories (matching `ToolRegistry::rescan`).
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.join("manifest.toml").is_file() {
            v2.insert(name);
        } else if path.join("tool.toml").is_file() {
            legacy.insert(name);
        }
    }

    Ok((legacy, v2))
}

/// Persists `enabled: Some(false)` into `<config>/tools/state.json` via
/// `atomic_replace` for each snapshotted 1.x user tool without touching
/// `<data>/tool-data/<id>/` or `<config>/tools/settings/<id>.json`.
pub(crate) fn disable_user_tools(
    config_root: &Path,
    user_tools_to_disable: &[String],
) -> Result<UserToolsReceipt, MigrationError> {
    if user_tools_to_disable.is_empty() {
        return Ok(UserToolsReceipt {
            disabled_user_tools: Vec::new(),
        });
    }

    let state_path = config_root.join("tools").join("state.json");
    let existing = read_published_bounded(&state_path, MAX_TOOL_STATE_BYTES, "tools/state.json")?;
    let existed = existing.is_some();
    let mut states: BTreeMap<String, SavedToolState> = match existing {
        Some(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| MigrationError::MalformedInput {
                label: "tools/state.json",
            })?
        }
        None => BTreeMap::new(),
    };

    let mut changed = false;
    for id in user_tools_to_disable {
        let entry = states.entry(id.clone()).or_default();
        if entry.enabled != Some(false) {
            entry.enabled = Some(false);
            changed = true;
        }
    }

    if changed || !existed {
        let mut bytes =
            serde_json::to_vec_pretty(&states).map_err(|_| MigrationError::MalformedInput {
                label: "tools/state.json",
            })?;
        bytes.push(b'\n');
        atomic_publish_bytes(&state_path, &bytes, "tools/state.json")?;
    }

    Ok(UserToolsReceipt {
        disabled_user_tools: user_tools_to_disable.to_vec(),
    })
}
