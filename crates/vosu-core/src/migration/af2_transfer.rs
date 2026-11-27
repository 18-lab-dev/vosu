//! Crash-recoverable, copy-only AF.2 cross-kind data and settings transfer
//! (`docs/v2/ecosystem-release-matrix.md` §5).
//!
//! Multi-target rows (`downloader`, `map-analysis`, `practice`, `skin-studio`)
//! write up to three independent destinations:
//! 1. Durable data (`<data>/tool-data/<id>/` -> `<data>/plugins/<plugin_id>/`,
//!    staged in a hidden same-parent directory and published atomically).
//! 2. Plugin settings (`<config>/plugins/settings.json`, including secrets).
//! 3. Web tool settings (`<config>/tools/settings/<web_tool_id>.json`).
//!
//! Each substep persists its durable progress receipt (`data_copied`,
//! `plugin_settings_imported`, `web_settings_copied`) to
//! `<config>/migration-v2-journal.json` immediately after committing, so retry
//! after an interruption skips already-committed substeps instead of mistaking
//! its own partial output for a collision, while still failing closed with
//! `IntentStatus::Collision` on any genuine pre-existing collision on an
//! uncommitted substep.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use vosu_plugin_api::manifest::settings::SettingsSchema;

use crate::plugin::settings::{PluginSettingsStore, SettingsStorage};

use super::MigrationError;
use super::intents::af2_row_for;
use super::journal::{
    Af2TransferSpec, IntentKind, IntentStatus, MigrationJournal, atomic_publish_bytes,
    read_plain_bounded, read_published_bounded, save_journal, sync_parent_dir,
};

/// Size cap on a single 1.x tool settings JSON file (1 MiB).
const MAX_LEGACY_TOOL_SETTINGS_BYTES: u64 = 1024 * 1024;
/// Hard bound on recursive file copies during one AF.2 transfer.
const MAX_AF2_COPY_FILES: usize = 4_096;
/// Hard bound on total bytes copied during one AF.2 durable-data transfer (256 MiB).
const MAX_AF2_COPY_BYTES: u64 = 256 * 1024 * 1024;

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

/// Test-only crash-injection points inside [`execute_af2_transfer`] covering
/// every durable substep receipt boundary.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Af2CrashPoint {
    AfterDataCopied,
    AfterPluginSettingsImported,
    AfterWebSettingsCopied,
}

/// Executes or resumes one closed AF.2 copy-only data and settings transfer row
/// (`docs/v2/ecosystem-release-matrix.md` §5):
/// - Never re-runs once `intent.status == IntentStatus::Completed`.
/// - Preflights ALL uncommitted destinations for collisions BEFORE writing any
///   uncommitted destination; any pre-existing destination data or 2.0 setting
///   records `IntentStatus::Collision { path_hint }` in the journal and leaves
///   both sides completely untouched.
/// - Persists per-substep durable receipts (`data_copied`,
///   `plugin_settings_imported`, `web_settings_copied`) to the journal after
///   each substep so crash replay resumes cleanly without false collisions.
/// - Copies 1.x durable data and allowlisted settings without ever moving,
///   modifying, or deleting 1.x files.
/// - Secrets (`secret_plugin_keys`) go ONLY into the plugin's
///   `PluginSettingsStore`, never into web tool settings or diagnostics.
pub fn execute_af2_transfer(
    config_root: &Path,
    data_root: &Path,
    journal: &mut MigrationJournal,
    intent_id: &str,
    settings_store: &PluginSettingsStore,
    plugin_schema: Option<&SettingsSchema>,
) -> Result<Af2TransferOutcome, MigrationError> {
    execute_af2_transfer_inner(
        config_root,
        data_root,
        journal,
        intent_id,
        settings_store,
        plugin_schema,
        #[cfg(test)]
        None,
    )
}

/// Test-only entry point allowing deterministic crash injection at any AF.2
/// substep boundary in [`Af2CrashPoint`].
#[cfg(test)]
pub fn execute_af2_transfer_with_crash_hook(
    config_root: &Path,
    data_root: &Path,
    journal: &mut MigrationJournal,
    intent_id: &str,
    settings_store: &PluginSettingsStore,
    plugin_schema: Option<&SettingsSchema>,
    crash_at: Option<Af2CrashPoint>,
) -> Result<Af2TransferOutcome, MigrationError> {
    execute_af2_transfer_inner(
        config_root,
        data_root,
        journal,
        intent_id,
        settings_store,
        plugin_schema,
        crash_at,
    )
}

fn execute_af2_transfer_inner(
    config_root: &Path,
    data_root: &Path,
    journal: &mut MigrationJournal,
    intent_id: &str,
    settings_store: &PluginSettingsStore,
    plugin_schema: Option<&SettingsSchema>,
    #[cfg(test)] crash_at: Option<Af2CrashPoint>,
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

    // Ensure `af2_transfer` progress metadata exists on the intent.
    if journal.pending_intents[index].af2_transfer.is_none() {
        journal.pending_intents[index].af2_transfer = Some(Af2TransferSpec {
            target_plugin: row.target_plugin.map(str::to_owned),
            target_web_tool: row.target_web_tool.map(str::to_owned),
            copy_durable_data: row.copy_durable_data,
            data_copied: false,
            plugin_settings_imported: false,
            web_settings_copied: false,
        });
    }
    let progress = journal.pending_intents[index]
        .af2_transfer
        .clone()
        .expect("initialized above");

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

    // 3. Preflight ALL uncommitted destinations for collisions & schema errors
    //    BEFORE mutating any uncommitted destination.
    let src_data_dir = data_root.join("tool-data").join(&legacy_id);
    let dst_data_dir = row
        .target_plugin
        .map(|plugin_id| data_root.join("plugins").join(plugin_id));
    let mut files_to_copy = Vec::new();
    if !progress.data_copied
        && row.copy_durable_data
        && let (Some(plugin_id), Some(dst_dir)) = (row.target_plugin, dst_data_dir.as_ref())
        && path_exists(&src_data_dir, "tool-data")?
    {
        if destination_has_data(dst_dir, "plugins")? {
            return record_collision(config_root, journal, index, format!("plugins/{plugin_id}"));
        }
        let mut total_bytes = 0u64;
        collect_copy_plan(
            &src_data_dir,
            &src_data_dir,
            &mut files_to_copy,
            &mut total_bytes,
        )?;
    }

    if !progress.plugin_settings_imported
        && let Some(plugin_id) = row.target_plugin
        && !plugin_overrides.is_empty()
    {
        let existing = settings_store
            .load(plugin_id)
            .map_err(|_| MigrationError::PluginSettingsStoreFailed)?;
        if plugin_overrides
            .keys()
            .any(|key| existing.contains_key(key))
        {
            return record_collision(
                config_root,
                journal,
                index,
                format!("plugins/settings.json:{plugin_id}"),
            );
        }
        if let Some(schema) = plugin_schema {
            schema
                .merge(&plugin_overrides)
                .map_err(|_| MigrationError::PluginSchemaRejected { plugin_id })?;
        }
    }

    let mut existing_web_tool_settings = Map::new();
    if !progress.web_settings_copied
        && let Some(web_tool_id) = row.target_web_tool
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
                return record_collision(
                    config_root,
                    journal,
                    index,
                    format!("tools/settings/{web_tool_id}.json"),
                );
            }
        }
    }

    // 4a. Substep 1: copy durable data into a same-parent hidden staging dir,
    //     atomically rename into `<data>/plugins/<plugin_id>`, then record `data_copied`.
    let mut copied_data_files = 0usize;
    if !progress.data_copied {
        if let (Some(plugin_id), Some(dst_dir)) = (row.target_plugin, dst_data_dir.as_ref())
            && !files_to_copy.is_empty()
        {
            copied_data_files = stage_and_publish_data_dir(
                data_root,
                plugin_id,
                &src_data_dir,
                dst_dir,
                &files_to_copy,
            )?;
        }
        if let Some(spec) = journal.pending_intents[index].af2_transfer.as_mut() {
            spec.data_copied = true;
        }
        save_journal(config_root, journal)?;

        #[cfg(test)]
        if crash_at == Some(Af2CrashPoint::AfterDataCopied) {
            return Err(MigrationError::InjectedAf2Crash(
                Af2CrashPoint::AfterDataCopied,
            ));
        }
    }

    // 4b. Substep 2: import allowlisted plugin settings, then record `plugin_settings_imported`.
    let mut imported_plugin_keys = Vec::new();
    if !progress.plugin_settings_imported {
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
        if let Some(spec) = journal.pending_intents[index].af2_transfer.as_mut() {
            spec.plugin_settings_imported = true;
        }
        save_journal(config_root, journal)?;

        #[cfg(test)]
        if crash_at == Some(Af2CrashPoint::AfterPluginSettingsImported) {
            return Err(MigrationError::InjectedAf2Crash(
                Af2CrashPoint::AfterPluginSettingsImported,
            ));
        }
    }

    // 4c. Substep 3: copy allowlisted web-tool presentation settings, then record `web_settings_copied`.
    let mut imported_web_tool_keys = Vec::new();
    if !progress.web_settings_copied {
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
            let mut bytes =
                serde_json::to_vec_pretty(&existing_web_tool_settings).map_err(|_| {
                    MigrationError::MalformedInput {
                        label: "tools/settings",
                    }
                })?;
            bytes.push(b'\n');
            atomic_publish_bytes(&dst_settings_path, &bytes, "tools/settings")?;
        }
        if let Some(spec) = journal.pending_intents[index].af2_transfer.as_mut() {
            spec.web_settings_copied = true;
        }
        save_journal(config_root, journal)?;

        #[cfg(test)]
        if crash_at == Some(Af2CrashPoint::AfterWebSettingsCopied) {
            return Err(MigrationError::InjectedAf2Crash(
                Af2CrashPoint::AfterWebSettingsCopied,
            ));
        }
    }

    // 5. All substeps are durable; mark intent Completed.
    journal.pending_intents[index].status = IntentStatus::Completed;
    save_journal(config_root, journal)?;

    Ok(Af2TransferOutcome::Completed {
        copied_data_files,
        imported_plugin_keys,
        imported_web_tool_keys,
    })
}

fn record_collision(
    config_root: &Path,
    journal: &mut MigrationJournal,
    index: usize,
    path_hint: String,
) -> Result<Af2TransferOutcome, MigrationError> {
    journal.pending_intents[index].status = IntentStatus::Collision {
        path_hint: path_hint.clone(),
    };
    save_journal(config_root, journal)?;
    Ok(Af2TransferOutcome::Collision { path_hint })
}

/// Copies `files_to_copy` from `src_data_dir` into a hidden same-parent staging
/// directory `<data>/plugins/.af2-stage-<plugin_id>` (cleaning up any leftover
/// staging directory from an earlier interrupted attempt), then atomically
/// renames the complete directory into `dst_dir`.
fn stage_and_publish_data_dir(
    data_root: &Path,
    plugin_id: &str,
    src_data_dir: &Path,
    dst_dir: &Path,
    files_to_copy: &[PathBuf],
) -> Result<usize, MigrationError> {
    let plugins_parent = data_root.join("plugins");
    fs::create_dir_all(&plugins_parent).map_err(|error| MigrationError::Io {
        label: "plugins",
        kind: error.kind(),
    })?;

    let stage_dir = plugins_parent.join(format!(".af2-stage-{plugin_id}"));
    match fs::remove_dir_all(&stage_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(MigrationError::Io {
                label: "plugins",
                kind: error.kind(),
            });
        }
    }
    fs::create_dir_all(&stage_dir).map_err(|error| MigrationError::Io {
        label: "plugins",
        kind: error.kind(),
    })?;

    let mut copied = 0usize;
    for relative in files_to_copy {
        let src_file = src_data_dir.join(relative);
        let stage_file = stage_dir.join(relative);
        let Some(bytes) = read_plain_bounded(&src_file, MAX_AF2_COPY_BYTES, "tool-data")? else {
            continue;
        };
        atomic_publish_bytes(&stage_file, &bytes, "plugins")?;
        copied += 1;
    }

    // If an empty destination directory already existed (confirmed empty in preflight),
    // remove it before renaming so Windows and Unix directory rename behave identically.
    match fs::symlink_metadata(dst_dir) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
            fs::remove_dir(dst_dir).map_err(|error| MigrationError::Io {
                label: "plugins",
                kind: error.kind(),
            })?;
        }
        Ok(_) => {
            return Err(MigrationError::Io {
                label: "plugins",
                kind: io::ErrorKind::AlreadyExists,
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(MigrationError::Io {
                label: "plugins",
                kind: error.kind(),
            });
        }
    }

    fs::rename(&stage_dir, dst_dir).map_err(|error| MigrationError::Io {
        label: "plugins",
        kind: error.kind(),
    })?;
    sync_parent_dir(&plugins_parent, "plugins")?;
    Ok(copied)
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
    out: &mut Vec<PathBuf>,
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
