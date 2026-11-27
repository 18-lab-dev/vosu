//! Offline official replacement intent resolution and AF.2 closed cross-kind
//! copy-only data/settings transfer (`docs/v2/catalog-v2.md`,
//! `docs/v2/ecosystem-release-matrix.md` §5, A8.2 / AF.2).
//!
//! Zero network calls occur in this module. Catalog resolution consumes already
//! validated catalog entries/snapshots and maps each official 1.x tool/overlay
//! strictly by exact `official_source` + same `PackageKind` + explicit `replaces`:
//! - `0` claims -> `IntentStatus::Unmanaged` (1.x install left untouched)
//! - `1` claim  -> `IntentStatus::Pending` with `resolved_target_id = Some(id)`
//! - `>1` claims -> `IntentStatus::Conflict { candidates }` (never first-pick)
//! - lookup error -> `IntentStatus::RetryableError` (never falls back to another source)
//!
//! AF.2 cross-kind transfers are strictly copy-only (never moving or deleting
//! 1.x bytes), fail closed with `IntentStatus::Collision` if any destination
//! data or setting already exists, and never re-run after `IntentStatus::Completed`.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};
use vosu_plugin_api::manifest::PackageKind;
use vosu_plugin_api::manifest::settings::SettingsSchema;

use crate::catalog::sources::SourceSnapshot;
use crate::plugin::settings::{PluginSettingsStore, SettingsStorage};

use super::MigrationError;
use super::journal::{
    Af2TransferSpec, DiscoveredOfficialOverlay, DiscoveredOfficialTool, IntentKind, IntentStatus,
    MAX_PENDING_INTENTS, MigrationJournal, PendingIntent, atomic_publish_bytes, read_plain_bounded,
    read_published_bounded, save_journal,
};

/// Size cap on a single 1.x tool settings JSON file (1 MiB).
const MAX_LEGACY_TOOL_SETTINGS_BYTES: u64 = 1024 * 1024;
/// Hard bound on recursive file copies during one AF.2 transfer.
const MAX_AF2_COPY_FILES: usize = 4_096;
/// Hard bound on total bytes copied during one AF.2 durable-data transfer (256 MiB).
const MAX_AF2_COPY_BYTES: u64 = 256 * 1024 * 1024;

/// One row of AF.2's closed one-time cross-kind transfer map
/// (`docs/v2/ecosystem-release-matrix.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Af2TransferRow {
    pub legacy_tool_id: &'static str,
    pub official_source: &'static str,
    pub target_plugin: Option<&'static str>,
    pub copy_durable_data: bool,
    pub plugin_settings_keys: &'static [&'static str],
    pub secret_plugin_keys: &'static [&'static str],
    pub target_web_tool: Option<&'static str>,
    pub web_tool_settings_keys: &'static [&'static str],
}

/// Complete closed AF.2 cross-kind transfer table (9 official 1.x tools).
pub const AF2_TRANSFER_MAP: [Af2TransferRow; 9] = [
    Af2TransferRow {
        legacy_tool_id: "lazer-library",
        official_source: "18-lab-dev/vosu-tools",
        target_plugin: Some("vosu.tools.lazer-library"),
        copy_durable_data: true,
        plugin_settings_keys: &["lazer_data_dir"],
        secret_plugin_keys: &[],
        target_web_tool: None,
        web_tool_settings_keys: &[],
    },
    Af2TransferRow {
        legacy_tool_id: "map-analysis",
        official_source: "18-lab-dev/vosu-tools",
        target_plugin: Some("vosu.tools.map-analysis"),
        copy_durable_data: true,
        plugin_settings_keys: &["local_window", "local_hop", "strain_window", "strain_hop"],
        secret_plugin_keys: &[],
        target_web_tool: Some("map-analysis-v2"),
        web_tool_settings_keys: &[
            "follow_game",
            "show_etterna",
            "show_prism",
            "show_quaver",
            "show_interlude",
            "show_osu",
            "show_reform",
            "show_dan7k",
            "show_danln",
            "timeline_system",
        ],
    },
    Af2TransferRow {
        legacy_tool_id: "server-pp",
        official_source: "18-lab-dev/vosu-tools",
        target_plugin: Some("vosu.tools.server-pp"),
        copy_durable_data: true,
        plugin_settings_keys: &[
            "calculators.folder",
            "calculators.files",
            "calculators.enabled",
            "calculators.names",
            "calculators.fuel",
            "calculators.memory_mib",
        ],
        secret_plugin_keys: &[],
        target_web_tool: Some("server-pp-v2"),
        web_tool_settings_keys: &[],
    },
    Af2TransferRow {
        legacy_tool_id: "practice",
        official_source: "18-lab-dev/vosu-tools",
        target_plugin: Some("vosu.tools.practice"),
        copy_durable_data: true,
        plugin_settings_keys: &[
            "lead_in_ms",
            "lead_out_ms",
            "gap_ms",
            "fade_ms",
            "pitch_with_rate",
            "export_dir",
            "open_exports",
        ],
        secret_plugin_keys: &[],
        target_web_tool: Some("practice-v2"),
        web_tool_settings_keys: &["map_analysis_timeline"],
    },
    Af2TransferRow {
        legacy_tool_id: "replay-renderer",
        official_source: "18-lab-dev/vosu-tools",
        target_plugin: Some("vosu.tools.replay-renderer"),
        copy_durable_data: true,
        plugin_settings_keys: &[
            "audio.master",
            "audio.master_mute",
            "audio.music",
            "audio.hitsounds",
            "audio.mute",
            "audio.fallback",
            "audio.limiter",
            "osu_dir",
            "songs_dir",
            "skin",
            "mania.scroll_speed",
            "resolution",
            "fps",
            "outro_ms",
            "fade_ms",
            "output_dir",
            "encoder",
            "export_for",
            "discord_mb",
            "parallel_jobs",
            "render_gpu",
            "ffmpeg_path",
        ],
        secret_plugin_keys: &[],
        target_web_tool: Some("replay-renderer-v2"),
        web_tool_settings_keys: &[],
    },
    Af2TransferRow {
        legacy_tool_id: "skin-studio",
        official_source: "18-lab-dev/vosu-tools",
        target_plugin: Some("vosu.tools.skin-studio"),
        copy_durable_data: true,
        plugin_settings_keys: &["stable_skins_dir", "lazer_skins", "credits"],
        secret_plugin_keys: &[],
        target_web_tool: Some("skin-studio-v2"),
        web_tool_settings_keys: &["start_ruleset"],
    },
    Af2TransferRow {
        legacy_tool_id: "downloader",
        official_source: "18-lab-dev/vosu-tools",
        target_plugin: Some("vosu.tools.downloader"),
        copy_durable_data: true,
        plugin_settings_keys: &[
            "concurrency",
            "max_file_mb",
            "no_video",
            "songs_dir",
            "sources",
            "client_id",
            "client_secret",
        ],
        secret_plugin_keys: &["client_secret"],
        target_web_tool: Some("downloader-v2"),
        web_tool_settings_keys: &["hide_owned", "follow_current_map"],
    },
    Af2TransferRow {
        legacy_tool_id: "map-preview",
        official_source: "18-lab-dev/vosu-tools",
        target_plugin: None,
        copy_durable_data: false,
        plugin_settings_keys: &[],
        secret_plugin_keys: &[],
        target_web_tool: Some("map-preview-v2"),
        web_tool_settings_keys: &[
            "skin",
            "mania_scroll_speed",
            "note_speed",
            "show_bar",
            "show_progress",
            "controls_open",
        ],
    },
    Af2TransferRow {
        legacy_tool_id: "vosulay",
        official_source: "18-lab-dev/vosulay",
        target_plugin: Some("vosu.vosulay"),
        copy_durable_data: true,
        plugin_settings_keys: &[],
        secret_plugin_keys: &[],
        target_web_tool: Some("vosulay-editor"),
        web_tool_settings_keys: &[],
    },
];

/// Looks up one closed AF.2 row by exact `(legacy_tool_id, official_source)`.
pub fn af2_row_for(legacy_tool_id: &str, official_source: &str) -> Option<&'static Af2TransferRow> {
    AF2_TRANSFER_MAP
        .iter()
        .find(|row| row.legacy_tool_id == legacy_tool_id && row.official_source == official_source)
}

/// Builds bounded, sorted, deduplicated initial `PendingIntent`s from
/// discovered official 1.x tools and overlays.
pub(crate) fn build_pending_intents(
    official_tools: &[DiscoveredOfficialTool],
    official_overlays: &[DiscoveredOfficialOverlay],
) -> Result<Vec<PendingIntent>, MigrationError> {
    let mut intents = Vec::new();
    let mut seen_ids = BTreeSet::new();

    for tool in official_tools {
        let af2 = af2_row_for(&tool.id, &tool.official_source);
        // `lazer-library` has no 2.0 web UI; it retires through AF.2 alone.
        let has_web_successor = af2.is_none_or(|row| row.target_web_tool.is_some());
        if has_web_successor {
            let intent_id = format!("tool:{}:{}", tool.official_source, tool.id);
            if seen_ids.insert(intent_id.clone()) {
                intents.push(PendingIntent {
                    intent_id,
                    kind: IntentKind::OfficialToolReplacement,
                    legacy_id: tool.id.clone(),
                    official_source: tool.official_source.clone(),
                    resolved_target_id: None,
                    af2_transfer: None,
                    status: IntentStatus::Pending,
                });
            }
        }
        if let Some(row) = af2 {
            let target_label = row
                .target_plugin
                .or(row.target_web_tool)
                .unwrap_or(row.legacy_tool_id);
            let intent_id = format!("af2:{}:{target_label}", row.legacy_tool_id);
            if seen_ids.insert(intent_id.clone()) {
                intents.push(PendingIntent {
                    intent_id,
                    kind: IntentKind::Af2CrossKindTransfer,
                    legacy_id: tool.id.clone(),
                    official_source: tool.official_source.clone(),
                    resolved_target_id: row
                        .target_plugin
                        .or(row.target_web_tool)
                        .map(str::to_owned),
                    af2_transfer: Some(Af2TransferSpec {
                        target_plugin: row.target_plugin.map(str::to_owned),
                        target_web_tool: row.target_web_tool.map(str::to_owned),
                        copy_durable_data: row.copy_durable_data,
                    }),
                    status: IntentStatus::Pending,
                });
            }
        }
    }

    for overlay in official_overlays {
        let intent_id = format!("overlay:{}:{}", overlay.official_source, overlay.id);
        if seen_ids.insert(intent_id.clone()) {
            intents.push(PendingIntent {
                intent_id,
                kind: IntentKind::OfficialOverlayReplacement,
                legacy_id: overlay.id.clone(),
                official_source: overlay.official_source.clone(),
                resolved_target_id: None,
                af2_transfer: None,
                status: IntentStatus::Pending,
            });
        }
    }

    if intents.len() > MAX_PENDING_INTENTS {
        return Err(MigrationError::JournalOversized);
    }
    intents.sort_by(|left, right| left.intent_id.cmp(&right.intent_id));
    Ok(intents)
}

/// One already-validated catalog entry projected for offline intent resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCatalogEntry {
    pub id: String,
    pub kind: PackageKind,
    pub replaces: Vec<String>,
}

/// Validated per-source catalog view passed into [`resolve_intents`].
/// Zero network I/O is performed by resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSourceLookup {
    pub source: String,
    pub entries: Result<Vec<ValidatedCatalogEntry>, String>,
}

impl CatalogSourceLookup {
    /// Projects core's already-validated [`SourceSnapshot`] slice into offline
    /// lookup records for [`resolve_intents`].
    pub fn from_source_snapshots(snapshots: &[SourceSnapshot]) -> Vec<Self> {
        snapshots
            .iter()
            .map(|snapshot| {
                let entries = match snapshot.catalog() {
                    Ok(catalog) => Ok(catalog
                        .entries()
                        .map(|(kind, entry)| ValidatedCatalogEntry {
                            id: entry.id.clone(),
                            kind,
                            replaces: entry.replaces.clone(),
                        })
                        .collect()),
                    Err(failure) => Err(failure.to_string()),
                };
                Self {
                    source: snapshot.source().identity.as_str().to_owned(),
                    entries,
                }
            })
            .collect()
    }
}

/// Summary of intent states after one offline [`resolve_intents`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentResolutionSummary {
    pub pending_resolved: usize,
    pub unmanaged: usize,
    pub conflicts: usize,
    pub retryable_errors: usize,
    pub unchanged_terminal: usize,
}

/// Resolves `journal.pending_intents` strictly offline against validated
/// per-source catalog lookups (A8.2).
///
/// - Never performs network I/O, never marks anything installed, and never
///   falls back to a different catalog source on error or absence.
/// - Preserves terminal `Completed` and `Collision` intents untouched.
pub fn resolve_intents(
    journal: &mut MigrationJournal,
    lookups: &[CatalogSourceLookup],
) -> IntentResolutionSummary {
    let mut summary = IntentResolutionSummary::default();

    for intent in &mut journal.pending_intents {
        if matches!(
            intent.status,
            IntentStatus::Completed | IntentStatus::Collision { .. }
        ) {
            summary.unchanged_terminal += 1;
            continue;
        }

        let source_lookup = lookups
            .iter()
            .find(|lookup| lookup.source == intent.official_source);

        let entries = match source_lookup {
            None => {
                intent.resolved_target_id = None;
                intent.status = IntentStatus::RetryableError {
                    message: "official catalog source unavailable".to_owned(),
                };
                summary.retryable_errors += 1;
                continue;
            }
            Some(lookup) => match &lookup.entries {
                Ok(entries) => entries,
                Err(_) => {
                    intent.resolved_target_id = None;
                    intent.status = IntentStatus::RetryableError {
                        message: "official catalog lookup failed".to_owned(),
                    };
                    summary.retryable_errors += 1;
                    continue;
                }
            },
        };

        match intent.kind {
            IntentKind::OfficialToolReplacement | IntentKind::OfficialOverlayReplacement => {
                let expected_kind = match intent.kind {
                    IntentKind::OfficialToolReplacement => PackageKind::Tool,
                    IntentKind::OfficialOverlayReplacement => PackageKind::Overlay,
                    IntentKind::Af2CrossKindTransfer => unreachable!(),
                };
                let mut matches: Vec<String> = entries
                    .iter()
                    .filter(|entry| {
                        entry.kind == expected_kind && entry.replaces.contains(&intent.legacy_id)
                    })
                    .map(|entry| entry.id.clone())
                    .collect();
                matches.sort();
                matches.dedup();

                match matches.len() {
                    0 => {
                        intent.resolved_target_id = None;
                        intent.status = IntentStatus::Unmanaged;
                        summary.unmanaged += 1;
                    }
                    1 => {
                        intent.resolved_target_id = Some(matches.remove(0));
                        intent.status = IntentStatus::Pending;
                        summary.pending_resolved += 1;
                    }
                    _ => {
                        intent.resolved_target_id = None;
                        intent.status = IntentStatus::Conflict {
                            candidates: matches,
                        };
                        summary.conflicts += 1;
                    }
                }
            }
            IntentKind::Af2CrossKindTransfer => {
                let Some(row) = af2_row_for(&intent.legacy_id, &intent.official_source) else {
                    intent.resolved_target_id = None;
                    intent.status = IntentStatus::Unmanaged;
                    summary.unmanaged += 1;
                    continue;
                };
                let matched_target = if let Some(plugin_id) = row.target_plugin {
                    entries
                        .iter()
                        .any(|entry| entry.kind == PackageKind::Plugin && entry.id == plugin_id)
                        .then_some(plugin_id)
                } else if let Some(web_tool_id) = row.target_web_tool {
                    entries
                        .iter()
                        .any(|entry| entry.kind == PackageKind::Tool && entry.id == web_tool_id)
                        .then_some(web_tool_id)
                } else {
                    None
                };
                match matched_target {
                    Some(target) => {
                        intent.resolved_target_id = Some(target.to_owned());
                        intent.status = IntentStatus::Pending;
                        summary.pending_resolved += 1;
                    }
                    None => {
                        intent.resolved_target_id = None;
                        intent.status = IntentStatus::Unmanaged;
                        summary.unmanaged += 1;
                    }
                }
            }
        }
    }

    summary
}

/// Result of executing one AF.2 copy-only cross-kind data/settings transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Af2TransferOutcome {
    AlreadyCompleted,
    Collision {
        path_hint: String,
    },
    Completed {
        copied_data_files: usize,
        imported_plugin_keys: Vec<String>,
        imported_web_tool_keys: Vec<String>,
    },
}

/// Executes one closed AF.2 copy-only data and settings transfer row
/// (`docs/v2/ecosystem-release-matrix.md` §5):
/// - Never re-runs once `intent.status == IntentStatus::Completed`.
/// - Checks ALL durable-data and settings destinations for collisions BEFORE
///   writing any file; any existing destination data or 2.0 setting records
///   `IntentStatus::Collision { path_hint }` in the journal and leaves both
///   sides completely untouched.
/// - Copies 1.x durable data and allowlisted settings without ever moving,
///   modifying, or deleting 1.x files.
/// - Secrets (`secret_plugin_keys`) go ONLY into the plugin's `PluginSettingsStore`,
///   never into web tool settings or diagnostics.
pub fn execute_af2_transfer(
    config_root: &Path,
    data_root: &Path,
    journal: &mut MigrationJournal,
    intent_id: &str,
    settings_store: &PluginSettingsStore,
    plugin_schema: Option<&SettingsSchema>,
) -> Result<Af2TransferOutcome, MigrationError> {
    let index = journal
        .pending_intents
        .iter()
        .position(|intent| intent.intent_id == intent_id)
        .ok_or(MigrationError::IntentNotFound)?;

    if journal.pending_intents[index].kind != IntentKind::Af2CrossKindTransfer {
        return Err(MigrationError::InvalidIntentKind);
    }
    if journal.pending_intents[index].status == IntentStatus::Completed {
        return Ok(Af2TransferOutcome::AlreadyCompleted);
    }

    let legacy_id = journal.pending_intents[index].legacy_id.clone();
    let official_source = journal.pending_intents[index].official_source.clone();
    let row = af2_row_for(&legacy_id, &official_source).ok_or(MigrationError::UnknownAf2Row)?;

    // 1. Read optional 1.x tool settings JSON (strictly bounded, fail-closed on corruption).
    let legacy_settings_path = config_root
        .join("tools")
        .join("settings")
        .join(format!("{legacy_id}.json"));
    let legacy_settings: Map<String, Value> = match read_published_bounded(
        &legacy_settings_path,
        MAX_LEGACY_TOOL_SETTINGS_BYTES,
        "tools/settings",
    )? {
        Some(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| MigrationError::MalformedInput {
                label: "tools/settings",
            })?
        }
        None => Map::new(),
    };

    // 2. Partition strictly allowlisted keys (unlisted keys are never migrated;
    //    secret keys belong exclusively to `plugin_settings_keys`).
    let mut plugin_overrides = Map::new();
    for key in row.plugin_settings_keys {
        if let Some(value) = legacy_settings.get(*key) {
            plugin_overrides.insert((*key).to_owned(), value.clone());
        }
    }
    let mut web_tool_overrides = Map::new();
    for key in row.web_tool_settings_keys {
        if row.secret_plugin_keys.contains(key) {
            continue;
        }
        if let Some(value) = legacy_settings.get(*key) {
            web_tool_overrides.insert((*key).to_owned(), value.clone());
        }
    }

    // 3. Preflight ALL collision & schema checks BEFORE writing any byte.
    let src_data_dir = data_root.join("tool-data").join(&legacy_id);
    let dst_data_dir = row
        .target_plugin
        .map(|plugin_id| data_root.join("plugins").join(plugin_id));
    let mut files_to_copy = Vec::new();
    if row.copy_durable_data
        && let (Some(plugin_id), Some(dst_dir)) = (row.target_plugin, dst_data_dir.as_ref())
        && path_exists(&src_data_dir, "tool-data")?
    {
        if destination_has_data(dst_dir, "plugins")? {
            let path_hint = format!("plugins/{plugin_id}");
            journal.pending_intents[index].status = IntentStatus::Collision {
                path_hint: path_hint.clone(),
            };
            save_journal(config_root, journal)?;
            return Ok(Af2TransferOutcome::Collision { path_hint });
        }
        let mut total_bytes = 0u64;
        collect_copy_plan(
            &src_data_dir,
            &src_data_dir,
            &mut files_to_copy,
            &mut total_bytes,
        )?;
    }

    if let Some(plugin_id) = row.target_plugin
        && !plugin_overrides.is_empty()
    {
        let existing = settings_store
            .load(plugin_id)
            .map_err(|_| MigrationError::PluginSettingsStoreFailed)?;
        if plugin_overrides
            .keys()
            .any(|key| existing.contains_key(key))
        {
            let path_hint = format!("plugins/settings.json:{plugin_id}");
            journal.pending_intents[index].status = IntentStatus::Collision {
                path_hint: path_hint.clone(),
            };
            save_journal(config_root, journal)?;
            return Ok(Af2TransferOutcome::Collision { path_hint });
        }
        if let Some(schema) = plugin_schema {
            schema
                .merge(&plugin_overrides)
                .map_err(|_| MigrationError::PluginSchemaRejected { plugin_id })?;
        }
    }

    let mut existing_web_tool_settings = Map::new();
    if let Some(web_tool_id) = row.target_web_tool
        && !web_tool_overrides.is_empty()
    {
        let dst_settings_path = config_root
            .join("tools")
            .join("settings")
            .join(format!("{web_tool_id}.json"));
        if let Some(bytes) = read_published_bounded(
            &dst_settings_path,
            MAX_LEGACY_TOOL_SETTINGS_BYTES,
            "tools/settings",
        )? {
            existing_web_tool_settings =
                serde_json::from_slice(&bytes).map_err(|_| MigrationError::MalformedInput {
                    label: "tools/settings",
                })?;
            if web_tool_overrides
                .keys()
                .any(|key| existing_web_tool_settings.contains_key(key))
            {
                let path_hint = format!("tools/settings/{web_tool_id}.json");
                journal.pending_intents[index].status = IntentStatus::Collision {
                    path_hint: path_hint.clone(),
                };
                save_journal(config_root, journal)?;
                return Ok(Af2TransferOutcome::Collision { path_hint });
            }
        }
    }

    // 4. Perform copy-only durable writes (1.x source files are never modified or removed).
    let mut copied_data_files = 0usize;
    if let Some(dst_dir) = dst_data_dir.as_ref()
        && !files_to_copy.is_empty()
    {
        for relative in &files_to_copy {
            let src_file = src_data_dir.join(relative);
            let dst_file = dst_dir.join(relative);
            let Some(bytes) = read_plain_bounded(&src_file, MAX_AF2_COPY_BYTES, "tool-data")?
            else {
                continue;
            };
            atomic_publish_bytes(&dst_file, &bytes, "plugins")?;
            copied_data_files += 1;
        }
    }

    let mut imported_plugin_keys = Vec::new();
    if let Some(plugin_id) = row.target_plugin
        && !plugin_overrides.is_empty()
    {
        if let Some(schema) = plugin_schema {
            let outcome = settings_store
                .import_missing_overrides(plugin_id, schema, &plugin_overrides)
                .map_err(|_| MigrationError::PluginSettingsStoreFailed)?;
            imported_plugin_keys = outcome.imported_keys;
        } else {
            let mut merged = settings_store
                .load(plugin_id)
                .map_err(|_| MigrationError::PluginSettingsStoreFailed)?;
            for (key, value) in &plugin_overrides {
                merged.insert(key.clone(), value.clone());
                imported_plugin_keys.push(key.clone());
            }
            imported_plugin_keys.sort();
            settings_store
                .store(plugin_id, &merged)
                .map_err(|_| MigrationError::PluginSettingsStoreFailed)?;
        }
    }

    let mut imported_web_tool_keys = Vec::new();
    if let Some(web_tool_id) = row.target_web_tool
        && !web_tool_overrides.is_empty()
    {
        let dst_settings_path = config_root
            .join("tools")
            .join("settings")
            .join(format!("{web_tool_id}.json"));
        for (key, value) in &web_tool_overrides {
            existing_web_tool_settings.insert(key.clone(), value.clone());
            imported_web_tool_keys.push(key.clone());
        }
        imported_web_tool_keys.sort();
        let mut bytes = serde_json::to_vec_pretty(&existing_web_tool_settings).map_err(|_| {
            MigrationError::MalformedInput {
                label: "tools/settings",
            }
        })?;
        bytes.push(b'\n');
        atomic_publish_bytes(&dst_settings_path, &bytes, "tools/settings")?;
    }

    journal.pending_intents[index].status = IntentStatus::Completed;
    save_journal(config_root, journal)?;

    Ok(Af2TransferOutcome::Completed {
        copied_data_files,
        imported_plugin_keys,
        imported_web_tool_keys,
    })
}

fn path_exists(path: &Path, label: &'static str) -> Result<bool, MigrationError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(MigrationError::SymlinkTarget { label }),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(MigrationError::Io {
            label,
            kind: error.kind(),
        }),
    }
}

fn destination_has_data(dir: &Path, label: &'static str) -> Result<bool, MigrationError> {
    match fs::symlink_metadata(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(MigrationError::Io {
            label,
            kind: error.kind(),
        }),
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => Ok(true),
        Ok(_) => {
            let mut entries = fs::read_dir(dir).map_err(|error| MigrationError::Io {
                label,
                kind: error.kind(),
            })?;
            Ok(entries
                .next()
                .transpose()
                .map_err(|error| MigrationError::Io {
                    label,
                    kind: error.kind(),
                })?
                .is_some())
        }
    }
}

fn collect_copy_plan(
    root: &Path,
    current: &Path,
    out: &mut Vec<std::path::PathBuf>,
    total_bytes: &mut u64,
) -> Result<(), MigrationError> {
    let mut entries: Vec<_> = fs::read_dir(current)
        .map_err(|error| MigrationError::Io {
            label: "tool-data",
            kind: error.kind(),
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| MigrationError::Io {
            label: "tool-data",
            kind: error.kind(),
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let meta = fs::symlink_metadata(&path).map_err(|error| MigrationError::Io {
            label: "tool-data",
            kind: error.kind(),
        })?;
        if meta.file_type().is_symlink() {
            return Err(MigrationError::SymlinkTarget { label: "tool-data" });
        }
        if meta.is_dir() {
            collect_copy_plan(root, &path, out, total_bytes)?;
        } else if meta.is_file() {
            if out.len() >= MAX_AF2_COPY_FILES {
                return Err(MigrationError::InputOversized {
                    label: "tool-data",
                    max_bytes: MAX_AF2_COPY_BYTES,
                });
            }
            *total_bytes = total_bytes.saturating_add(meta.len());
            if *total_bytes > MAX_AF2_COPY_BYTES {
                return Err(MigrationError::InputOversized {
                    label: "tool-data",
                    max_bytes: MAX_AF2_COPY_BYTES,
                });
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| MigrationError::MalformedInput { label: "tool-data" })?
                .to_path_buf();
            out.push(relative);
        }
    }
    Ok(())
}
