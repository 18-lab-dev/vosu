//! Normal-root 1.x -> 2.0 migration journal and offline local conversion engine
//! (`docs/v2/catalog-v2.md` "First-start migration from 1.x", A8.1, A8.2, A8.3,
//! AF.2).
//!
//! ## Destructive write ordering (A8.3)
//! 1. Refuse / no-op on [`BuildChannel::Prerelease`] without touching any file.
//! 2. Load `<config>/migration-v2-journal.json` fail-closed. If
//!    `local_conversion_complete == true`, return immediately without writing.
//! 3. If no journal exists, parse and validate ALL legacy inputs upfront
//!    (`vosu.toml` > `vosu.env` > pre-rename `.env`, `catalog-sources.json`,
//!    `tools/state.json`, `tools/overlay-catalog-state.json`, `<data>/tools/*`,
//!    and `PluginSettingsStore` health), then persist the snapshotted journal
//!    BEFORE any destructive write.
//! 4. Execute only incomplete steps strictly from the journal's snapshot:
//!    - `plugin_settings`: `PluginSettingsStore::import_missing_overrides`
//!      (existing valid per-plugin overrides win per key) BEFORE touching
//!      `vosu.toml` or deleting legacy env files.
//!    - `core_config`: publish canonical stripped `vosu.toml` via
//!      `atomic_replace` BEFORE deleting readable legacy env files.
//!    - `catalog_sources`: publish verbatim 1.x sources to
//!      `catalog-source-settings.json` with exact-official-only trust.
//!    - `user_tools`: persist `enabled: Some(false)` for 1.x user tools in
//!      `tools/state.json` without touching durable data, settings, or overlays.
//! 5. Publish `local_conversion_complete: true` ONLY after all four steps are
//!    durable.

mod af2_transfer;
pub mod config;
pub mod installed;
pub mod intents;
pub mod journal;
pub mod sources;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_crash;
#[cfg(test)]
mod tests_intents;

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_sys::platform::BuildChannel;

use crate::plugin::settings::{PluginSettingsStore, SettingsStorage};

pub use config::{
    MAX_CONFIG_BYTES, OSU_MOVED_KEYS, OSU_PLUGIN_ID, OVERLAY_HOST_LEGACY_KEY,
    OVERLAY_HOST_PLUGIN_ID, OVERLAY_HOST_SETTING_KEY, RETIRED_KEYS,
};
pub use intents::{
    AF2_TRANSFER_MAP, Af2TransferOutcome, Af2TransferRow, CatalogSourceLookup,
    IntentResolutionSummary, ValidatedCatalogEntry, af2_row_for, execute_af2_transfer,
    resolve_intents,
};
#[cfg(test)]
pub use intents::{Af2CrashPoint, execute_af2_transfer_with_crash_hook};
pub use journal::{
    Af2TransferSpec, CatalogSourcesReceipt, CoreConfigReceipt, CoreConfigSnapshot,
    DiscoveredOfficialOverlay, DiscoveredOfficialTool, IntentKind, IntentStatus, JOURNAL_FILENAME,
    JOURNAL_VERSION, LegacyEnvKind, MAX_JOURNAL_BYTES, MAX_PENDING_INTENTS, MigrationJournal,
    MigrationSnapshot, PendingIntent, PluginSettingsReceipt, SourceFingerprint, StepState,
    UserToolsReceipt, journal_path, load_journal, save_journal,
};

/// Real plugin `settings.schema.json` documents required to validate and import
/// migrated overrides (A8.1).
#[derive(Debug, Clone)]
pub struct MigrationSchemas {
    pub osu: SettingsSchema,
    pub overlay_host: SettingsSchema,
}

/// Result of [`run_local_migration`].
#[derive(Debug, Clone, PartialEq)]
pub enum MigrationOutcome {
    /// Prerelease channel: normal-root migration is skipped with zero writes.
    SkippedPrerelease,
    /// Journal already recorded `local_conversion_complete: true`; fast no-op.
    AlreadyComplete(MigrationJournal),
    /// Local conversion ran (or resumed from a crash) and is now durable.
    Completed(MigrationJournal),
}

/// Test-only durable crash-boundary injection points covering every write
/// boundary required by A8.3.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    AfterSnapshot,
    AfterPluginSettings,
    AfterCoreConfigWriteBeforeEnvDelete,
    AfterCoreConfig,
    AfterCatalogSources,
    AfterUserToolsWriteBeforeReceipt,
    BeforeLocalConversionComplete,
}

/// Redacted, actionable migration error. Never exposes raw setting values,
/// secret values, raw file contents, or full local filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MigrationError {
    #[error(
        "migration journal is malformed or corrupt; repair or remove migration-v2-journal.json before retrying"
    )]
    JournalCorrupt,
    #[error(
        "migration journal exceeds the 2 MiB size limit; repair or remove migration-v2-journal.json before retrying"
    )]
    JournalOversized,
    #[error(
        "migration journal uses unsupported version {0}; upgrade vosu or repair migration-v2-journal.json"
    )]
    JournalUnsupportedVersion(u32),
    #[error("migration journal could not be serialized")]
    JournalSerialize,
    #[error(
        "legacy input {label} is malformed; fix it before starting vosu 2.0 (no files were modified)"
    )]
    MalformedInput { label: &'static str },
    #[error("legacy input {label} exceeds the {max_bytes}-byte safety limit")]
    InputOversized { label: &'static str, max_bytes: u64 },
    #[error(
        "legacy setting {key} has an invalid value; fix it before starting vosu 2.0 (no files were modified)"
    )]
    InvalidSetting { key: &'static str },
    #[error(
        "migrated settings for plugin {plugin_id} failed schema validation (no files were modified)"
    )]
    PluginSchemaRejected { plugin_id: &'static str },
    #[error(
        "plugin settings store is unavailable or corrupt; repair plugins/settings.json before retrying"
    )]
    PluginSettingsStoreFailed,
    #[error("migrated catalog sources could not be published")]
    SourcesPublishFailed,
    #[error("refusing to operate on symlinked migration path {label}")]
    SymlinkTarget { label: &'static str },
    #[error("filesystem error while accessing {label} ({kind:?})")]
    Io {
        label: &'static str,
        kind: io::ErrorKind,
    },
    #[error("migration intent not found in journal")]
    IntentNotFound,
    #[error("migration intent is not an AF.2 cross-kind transfer")]
    InvalidIntentKind,
    #[error("migration intent does not match the closed AF.2 transfer map")]
    UnknownAf2Row,
    #[cfg(test)]
    #[error("injected test crash at {0:?}")]
    InjectedCrash(CrashPoint),
    #[cfg(test)]
    #[error("injected AF.2 test crash at {0:?}")]
    InjectedAf2Crash(Af2CrashPoint),
}

/// Executes or resumes normal-root 1.x -> 2.0 offline local migration (A8.1,
/// A8.2, A8.3, AF.2).
///
/// - No-ops immediately on [`BuildChannel::Prerelease`].
/// - No-ops immediately when `<config>/migration-v2-journal.json` already
///   records `local_conversion_complete: true`.
/// - Performs zero network calls.
/// - Holds no locks across filesystem operations.
pub fn run_local_migration(
    config_root: &Path,
    data_root: &Path,
    channel: BuildChannel,
    schemas: &MigrationSchemas,
    settings_store: &PluginSettingsStore,
) -> Result<MigrationOutcome, MigrationError> {
    run_local_migration_inner(
        config_root,
        data_root,
        channel,
        schemas,
        settings_store,
        #[cfg(test)]
        None,
    )
}

/// Test-only entry point allowing deterministic crash injection at any durable
/// boundary in [`CrashPoint`].
#[cfg(test)]
pub fn run_local_migration_with_crash_hook(
    config_root: &Path,
    data_root: &Path,
    channel: BuildChannel,
    schemas: &MigrationSchemas,
    settings_store: &PluginSettingsStore,
    crash_at: Option<CrashPoint>,
) -> Result<MigrationOutcome, MigrationError> {
    run_local_migration_inner(
        config_root,
        data_root,
        channel,
        schemas,
        settings_store,
        crash_at,
    )
}

fn run_local_migration_inner(
    config_root: &Path,
    data_root: &Path,
    channel: BuildChannel,
    schemas: &MigrationSchemas,
    settings_store: &PluginSettingsStore,
    #[cfg(test)] crash_at: Option<CrashPoint>,
) -> Result<MigrationOutcome, MigrationError> {
    if channel == BuildChannel::Prerelease {
        return Ok(MigrationOutcome::SkippedPrerelease);
    }

    if let Some(existing) = load_journal(config_root)? {
        if existing.local_conversion_complete {
            return Ok(MigrationOutcome::AlreadyComplete(existing));
        }
        return finish_from_journal(
            config_root,
            existing,
            schemas,
            settings_store,
            #[cfg(test)]
            crash_at,
        );
    }

    // Fail closed if the destination plugin settings store is already corrupt
    // before writing a journal or touching any legacy file.
    settings_store
        .load(OSU_PLUGIN_ID)
        .map_err(|_| MigrationError::PluginSettingsStoreFailed)?;
    settings_store
        .load(OVERLAY_HOST_PLUGIN_ID)
        .map_err(|_| MigrationError::PluginSettingsStoreFailed)?;

    // Parse, validate, and snapshot ALL local inputs before any destructive write.
    let cfg_snap = config::snapshot_legacy_config(config_root, schemas)?;
    let src_snap = sources::snapshot_legacy_sources(config_root)?;
    let inst_snap = installed::snapshot_installed_legacy(config_root, data_root)?;

    let mut fingerprints = BTreeMap::new();
    fingerprints.extend(cfg_snap.fingerprints);
    fingerprints.extend(src_snap.fingerprints);
    fingerprints.extend(inst_snap.fingerprints);

    let pending_intents =
        intents::build_pending_intents(&inst_snap.official_tools, &inst_snap.official_overlays)?;

    let snapshot = MigrationSnapshot {
        fingerprints,
        osu_overrides: cfg_snap.osu_overrides,
        overlay_host_overrides: cfg_snap.overlay_host_overrides,
        core_config: cfg_snap.core_config,
        catalog_sources: src_snap.inputs,
        user_tools_to_disable: inst_snap.user_tools_to_disable,
        discovered_official_tools: inst_snap.official_tools,
        discovered_official_overlays: inst_snap.official_overlays,
    };

    let mut journal = MigrationJournal::new(snapshot, pending_intents);
    save_journal(config_root, &mut journal)?;

    #[cfg(test)]
    if crash_at == Some(CrashPoint::AfterSnapshot) {
        return Err(MigrationError::InjectedCrash(CrashPoint::AfterSnapshot));
    }

    finish_from_journal(
        config_root,
        journal,
        schemas,
        settings_store,
        #[cfg(test)]
        crash_at,
    )
}

fn finish_from_journal(
    config_root: &Path,
    mut journal: MigrationJournal,
    schemas: &MigrationSchemas,
    settings_store: &PluginSettingsStore,
    #[cfg(test)] crash_at: Option<CrashPoint>,
) -> Result<MigrationOutcome, MigrationError> {
    // Step 1: Import missing plugin setting overrides BEFORE touching `vosu.toml`
    // or deleting any legacy env file.
    if !journal.plugin_settings.is_completed() {
        let osu_outcome = settings_store
            .import_missing_overrides(OSU_PLUGIN_ID, &schemas.osu, &journal.snapshot.osu_overrides)
            .map_err(|_| MigrationError::PluginSettingsStoreFailed)?;
        let overlay_outcome = settings_store
            .import_missing_overrides(
                OVERLAY_HOST_PLUGIN_ID,
                &schemas.overlay_host,
                &journal.snapshot.overlay_host_overrides,
            )
            .map_err(|_| MigrationError::PluginSettingsStoreFailed)?;
        journal.plugin_settings = StepState::Completed(PluginSettingsReceipt {
            osu_imported_keys: osu_outcome.imported_keys,
            osu_preserved_keys: osu_outcome.preserved_existing_keys,
            overlay_host_imported_keys: overlay_outcome.imported_keys,
            overlay_host_preserved_keys: overlay_outcome.preserved_existing_keys,
        });
        save_journal(config_root, &mut journal)?;

        #[cfg(test)]
        if crash_at == Some(CrashPoint::AfterPluginSettings) {
            return Err(MigrationError::InjectedCrash(
                CrashPoint::AfterPluginSettings,
            ));
        }
    }

    // Step 2: Publish canonical stripped `vosu.toml` BEFORE deleting readable
    // legacy env files.
    if !journal.core_config.is_completed() {
        let wrote_toml =
            if let Some(canonical_toml) = journal.snapshot.core_config.canonical_toml.as_deref() {
                config::publish_canonical_toml(config_root, canonical_toml)?;
                true
            } else {
                false
            };

        #[cfg(test)]
        if crash_at == Some(CrashPoint::AfterCoreConfigWriteBeforeEnvDelete) {
            return Err(MigrationError::InjectedCrash(
                CrashPoint::AfterCoreConfigWriteBeforeEnvDelete,
            ));
        }

        let removed_env_files = config::remove_legacy_env_files(
            config_root,
            &journal.snapshot.core_config.legacy_env_files,
        )?;
        journal.core_config = StepState::Completed(CoreConfigReceipt {
            wrote_toml,
            stripped_keys: journal.snapshot.core_config.stripped_keys.clone(),
            removed_env_files,
        });
        save_journal(config_root, &mut journal)?;

        #[cfg(test)]
        if crash_at == Some(CrashPoint::AfterCoreConfig) {
            return Err(MigrationError::InjectedCrash(CrashPoint::AfterCoreConfig));
        }
    }

    // Step 3: Publish verbatim 1.x catalog sources into `catalog-source-settings.json`.
    if !journal.catalog_sources.is_completed() {
        let receipt = sources::apply_migrated_sources(
            config_root,
            journal.snapshot.catalog_sources.as_deref(),
        )?;
        journal.catalog_sources = StepState::Completed(receipt);
        save_journal(config_root, &mut journal)?;

        #[cfg(test)]
        if crash_at == Some(CrashPoint::AfterCatalogSources) {
            return Err(MigrationError::InjectedCrash(
                CrashPoint::AfterCatalogSources,
            ));
        }
    }

    // Step 4: Persist `enabled: Some(false)` for 1.x user tools in `tools/state.json`.
    if !journal.user_tools.is_completed() {
        let receipt =
            installed::disable_user_tools(config_root, &journal.snapshot.user_tools_to_disable)?;

        #[cfg(test)]
        if crash_at == Some(CrashPoint::AfterUserToolsWriteBeforeReceipt) {
            return Err(MigrationError::InjectedCrash(
                CrashPoint::AfterUserToolsWriteBeforeReceipt,
            ));
        }

        journal.user_tools = StepState::Completed(receipt);
        save_journal(config_root, &mut journal)?;
    }

    // Final Step: Publish `local_conversion_complete` ONLY after all four steps
    // are durably committed.
    #[cfg(test)]
    if crash_at == Some(CrashPoint::BeforeLocalConversionComplete) {
        return Err(MigrationError::InjectedCrash(
            CrashPoint::BeforeLocalConversionComplete,
        ));
    }

    journal.local_conversion_complete = true;
    save_journal(config_root, &mut journal)?;

    Ok(MigrationOutcome::Completed(journal))
}
