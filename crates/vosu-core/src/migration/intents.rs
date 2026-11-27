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

use vosu_plugin_api::manifest::PackageKind;

use crate::catalog::sources::SourceSnapshot;

use super::MigrationError;
use super::journal::{
    Af2TransferSpec, DiscoveredOfficialOverlay, DiscoveredOfficialTool, IntentKind, IntentStatus,
    MAX_PENDING_INTENTS, MigrationJournal, PendingIntent,
};

#[cfg(test)]
pub use super::af2_transfer::{Af2CrashPoint, execute_af2_transfer_with_crash_hook};
pub use super::af2_transfer::{Af2TransferOutcome, execute_af2_transfer};

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
                        data_copied: false,
                        plugin_settings_imported: false,
                        web_settings_copied: false,
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
