//! Channel guard, crash-boundary replay at EVERY durable boundary, post-migration
//! user-edit preservation, and double-run byte-identical idempotence tests (A8.1, A8.3).

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};
use vosu_sys::platform::BuildChannel;

use crate::config::PRE_RENAME_ENV_FILENAME;
use crate::plugin::settings::{PluginSettingsStore, SettingsStorage};

use super::tests::{real_schemas, write_legacy_user_tool};
use super::*;

fn seed_full_1x_root(config: &Path, data: &Path) {
    fs::create_dir_all(config.join("tools")).unwrap();
    fs::create_dir_all(data.join("tools")).unwrap();

    fs::write(
        config.join(PRE_RENAME_ENV_FILENAME),
        "CALCULATE_PP=false\nSTATIC_FOLDER_PATH=\"./legacy-overlays\"\n",
    )
    .unwrap();
    fs::write(
        config.join("vosu.env"),
        "POLL_RATE=222\nENABLE_KEY_OVERLAY=true\n",
    )
    .unwrap();
    fs::write(
        config.join("vosu.toml"),
        "SERVER_PORT = 24088\nAPI_V1_SUBSCRIPTION_DEBOUNCE = 4\nV2_SUBSCRIPTION_DEBOUNCE = 20\n",
    )
    .unwrap();
    fs::write(
        config.join("tools").join("catalog-sources.json"),
        serde_json::to_vec_pretty(&json!([
            {"source": "18-lab-dev/vosu-tools", "enabled": true},
            {"source": "community/extras", "enabled": true}
        ]))
        .unwrap(),
    )
    .unwrap();
    write_legacy_user_tool(data, config, "legacy-user-tool");
    fs::write(
        config.join("tools").join("state.json"),
        serde_json::to_vec_pretty(&json!({
            "legacy-user-tool": {"enabled": true, "trusted": true},
            "lazer-library": {"enabled": true, "trusted": true, "official": true, "catalog_source": "18-lab-dev/vosu-tools"}
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn prerelease_channel_is_a_strict_no_op_and_never_creates_journal_or_mutates_files() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    seed_full_1x_root(&config, &data);

    let toml_before = fs::read(config.join("vosu.toml")).unwrap();
    let env_before = fs::read(config.join("vosu.env")).unwrap();
    let state_before = fs::read(config.join("tools/state.json")).unwrap();

    let schemas = real_schemas();
    let store = PluginSettingsStore::open(&config);
    let outcome =
        run_local_migration(&config, &data, BuildChannel::Prerelease, &schemas, &store).unwrap();
    assert_eq!(outcome, MigrationOutcome::SkippedPrerelease);
    assert!(!journal_path(&config).exists());
    assert!(!store.path().exists());
    assert!(!config.join("catalog-source-settings.json").exists());
    assert_eq!(fs::read(config.join("vosu.toml")).unwrap(), toml_before);
    assert_eq!(fs::read(config.join("vosu.env")).unwrap(), env_before);
    assert!(config.join(PRE_RENAME_ENV_FILENAME).exists());
    assert_eq!(
        fs::read(config.join("tools/state.json")).unwrap(),
        state_before
    );
}

#[test]
fn crash_replay_at_every_durable_boundary_resumes_from_snapshot_without_clobbering_edits() {
    let schemas = real_schemas();
    let crash_points = [
        CrashPoint::AfterSnapshot,
        CrashPoint::AfterPluginSettings,
        CrashPoint::AfterCoreConfigWriteBeforeEnvDelete,
        CrashPoint::AfterCoreConfig,
        CrashPoint::AfterCatalogSources,
        CrashPoint::AfterUserToolsWriteBeforeReceipt,
        CrashPoint::BeforeLocalConversionComplete,
    ];

    for crash_point in crash_points {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        seed_full_1x_root(&config, &data);

        let store_before = PluginSettingsStore::open(&config);
        let err = run_local_migration_with_crash_hook(
            &config,
            &data,
            BuildChannel::Release,
            &schemas,
            &store_before,
            Some(crash_point),
        )
        .unwrap_err();
        assert_eq!(err, MigrationError::InjectedCrash(crash_point));

        let mid_journal = load_journal(&config).unwrap().expect("journal persisted");
        assert!(
            !mid_journal.local_conversion_complete,
            "local_conversion_complete must stay false at {crash_point:?}"
        );

        // Mutate legacy env input after snapshot: replay MUST ignore edited legacy
        // inputs and use the snapshotted values instead (A8.1 / A8.3).
        if config.join("vosu.env").exists() {
            fs::write(config.join("vosu.env"), "POLL_RATE=9999\n").unwrap();
        }

        // Simulate post-migration user edits to already-committed destinations:
        // replay MUST NEVER overwrite user edits in already-committed steps!
        let mut user_edited_settings = false;
        if mid_journal.plugin_settings.is_completed() {
            let edit_store = PluginSettingsStore::open(&config);
            let mut edited = edit_store.load(OSU_PLUGIN_ID).unwrap();
            edited.insert("POLL_RATE".to_owned(), json!(777.0));
            edit_store.store(OSU_PLUGIN_ID, &edited).unwrap();
            user_edited_settings = true;
        }
        let mut user_edited_toml = false;
        if mid_journal.core_config.is_completed() {
            let mut text = fs::read_to_string(config.join("vosu.toml")).unwrap();
            text.push_str("\nPOST_MIGRATION_USER_EDIT = true\n");
            fs::write(config.join("vosu.toml"), text).unwrap();
            user_edited_toml = true;
        }

        // Verify ordering invariant: `vosu.env` is NEVER deleted before `plugin_settings`
        // AND canonical `vosu.toml` are durable.
        if matches!(
            crash_point,
            CrashPoint::AfterSnapshot
                | CrashPoint::AfterPluginSettings
                | CrashPoint::AfterCoreConfigWriteBeforeEnvDelete
        ) {
            assert!(
                config.join("vosu.env").exists(),
                "vosu.env deleted too early at {crash_point:?}"
            );
        }
        if crash_point == CrashPoint::AfterSnapshot {
            assert!(
                !store_before.path().exists(),
                "plugin settings written before snapshot boundary"
            );
        }

        // Replay with fresh `PluginSettingsStore` to complete migration.
        let store_replay = PluginSettingsStore::open(&config);
        let MigrationOutcome::Completed(done) = run_local_migration_with_crash_hook(
            &config,
            &data,
            BuildChannel::Release,
            &schemas,
            &store_replay,
            None,
        )
        .unwrap() else {
            panic!("expected Completed after replay from {crash_point:?}");
        };

        assert!(done.local_conversion_complete);
        assert!(done.all_local_steps_completed());
        assert!(!config.join("vosu.env").exists());
        assert!(!config.join(PRE_RENAME_ENV_FILENAME).exists());

        // Check settings: either post-migration edit survived OR snapshotted 222.0 won (never 9999!).
        let osu = store_replay.load(OSU_PLUGIN_ID).unwrap();
        if user_edited_settings {
            assert_eq!(
                osu.get("POLL_RATE"),
                Some(&json!(777.0)),
                "post-migration edit overwritten at {crash_point:?}"
            );
        } else {
            assert_eq!(
                osu.get("POLL_RATE"),
                Some(&json!(222.0)),
                "mutated legacy env leaked into replay at {crash_point:?}"
            );
        }
        assert_eq!(osu.get("CALCULATE_PP"), Some(&Value::Bool(false)));

        // Check `vosu.toml` post-migration edit preservation.
        let final_toml = fs::read_to_string(config.join("vosu.toml")).unwrap();
        if user_edited_toml {
            assert!(
                final_toml.contains("POST_MIGRATION_USER_EDIT = true"),
                "post-migration vosu.toml edit overwritten at {crash_point:?}"
            );
        }

        // Check sources and user tools completed cleanly without duplicate intents.
        assert!(config.join("catalog-source-settings.json").exists());
        let saved: Value =
            serde_json::from_slice(&fs::read(config.join("tools/state.json")).unwrap()).unwrap();
        assert_eq!(saved["legacy-user-tool"]["enabled"], json!(false));
        assert_eq!(done.pending_intents.len(), 1);
        assert_eq!(
            done.pending_intents[0].intent_id,
            "af2:lazer-library:vosu.tools.lazer-library"
        );
    }
}

#[test]
fn double_run_after_completion_is_byte_identical_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    seed_full_1x_root(&config, &data);

    let schemas = real_schemas();
    let store = PluginSettingsStore::open(&config);
    let first =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap();
    let MigrationOutcome::Completed(first_journal) = first else {
        panic!("expected Completed on first run");
    };

    // Snapshot every file's exact bytes after first run.
    let watched = [
        journal_path(&config),
        config.join("vosu.toml"),
        store.path().to_path_buf(),
        config.join("catalog-source-settings.json"),
        config.join("tools/state.json"),
    ];
    let bytes_before: Vec<Vec<u8>> = watched.iter().map(|p| fs::read(p).unwrap()).collect();

    // Second run MUST be a fast no-op leaving every file byte-identical.
    let second =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap();
    assert_eq!(second, MigrationOutcome::AlreadyComplete(first_journal));

    for (path, before) in watched.iter().zip(bytes_before) {
        assert_eq!(
            fs::read(path).unwrap(),
            before,
            "file changed on second migration run: {}",
            path.display()
        );
    }

    // Even if user edits plugin settings post-migration, third run never touches them.
    store
        .store(
            OSU_PLUGIN_ID,
            &Map::from_iter([("POLL_RATE".to_owned(), json!(333.0))]),
        )
        .unwrap();
    let third =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap();
    assert!(matches!(third, MigrationOutcome::AlreadyComplete(_)));
    assert_eq!(
        store.load(OSU_PLUGIN_ID).unwrap().get("POLL_RATE"),
        Some(&json!(333.0))
    );
}
