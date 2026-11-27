//! Realistic fixture double-run idempotence, post-migration edit preservation,
//! simulated crash recovery at every durable journal boundary, and
//! `read_published` / fail-closed target checks (M8-M04, M8-M05, M8-X01).

use std::fs;
use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{Method, StatusCode};
use serde_json::{Map, Value, json};
use vosu_core::catalog::sources::{OFFICIAL_SOURCES, SourceStore};
use vosu_core::migration::{
    JOURNAL_VERSION, MigrationError, MigrationJournal, MigrationOutcome, OSU_PLUGIN_ID,
    OVERLAY_HOST_PLUGIN_ID, OVERLAY_HOST_SETTING_KEY, StepState, journal_path, load_journal,
    run_local_migration, save_journal,
};
use vosu_core::plugin::settings::{PluginSettingsStore, SettingsStorage};
use vosu_core::tools::{ExtraRoots, ToolRegistry};
use vosu_overlay_host::CounterService;
use vosu_proptest::bounded;
use vosu_sys::platform::{BuildChannel, read_published};

use crate::support::{
    NoopSourceLifecycle, bundled_migration_schemas, pre_rename_env_filename, read_json_value,
    read_toml_table, run_release_startup, seed_realistic_1x_fixture, tool_dashboard_json,
};

#[tokio::test]
async fn realistic_fixture_double_run_is_byte_identical_with_zero_duplicate_intents() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(&cache).unwrap();
    seed_realistic_1x_fixture(&config, &data);

    let user_tool_data_before =
        fs::read(data.join("tool-data/custom-user-tool/state.dat")).unwrap();
    let user_tool_settings_before =
        fs::read(config.join("tools/settings/custom-user-tool.json")).unwrap();

    // First production startup migration.
    let (first_outcome, first_cfg, first_settings) =
        run_release_startup(&config, &data).await.unwrap();
    let MigrationOutcome::Completed(first_journal) = first_outcome else {
        panic!("expected Completed on first run");
    };
    assert!(first_journal.local_conversion_complete);
    assert!(!config.join("vosu.env").exists());
    assert!(!config.join(pre_rename_env_filename()).exists());

    // Verify migrated plugin settings from the 3-tier fixture precedence.
    let osu = first_settings.load(OSU_PLUGIN_ID).unwrap();
    assert_eq!(osu.get("POLL_RATE"), Some(&json!(240.0)));
    assert_eq!(osu.get("PRECISE_DATA_POLL_RATE"), Some(&json!(4.0)));
    assert_eq!(osu.get("CALCULATE_PP"), Some(&Value::Bool(false)));
    assert_eq!(osu.get("SHOW_MP_COMMANDS"), Some(&Value::Bool(true)));
    assert_eq!(
        osu.get("READ_MANIA_SCROLL_SPEED"),
        Some(&Value::Bool(false))
    );
    let overlay = first_settings.load(OVERLAY_HOST_PLUGIN_ID).unwrap();
    assert_eq!(
        overlay.get(OVERLAY_HOST_SETTING_KEY),
        Some(&json!("custom-overlays"))
    );

    // Verify canonical vosu.toml preserves core + unknown + nested [custom].
    let table = read_toml_table(&config.join("vosu.toml"));
    assert_eq!(
        table
            .get("API_V1_SUBSCRIPTION_DEBOUNCE")
            .and_then(toml::Value::as_float),
        Some(6.5)
    );
    assert_eq!(
        table
            .get("CUSTOM_TOP_SENTINEL")
            .and_then(toml::Value::as_str),
        Some("preserve-top")
    );
    assert_eq!(
        table["custom"]["POLL_RATE"].as_integer(),
        Some(999),
        "nested [custom].POLL_RATE must survive"
    );

    // Verify catalog sources via real SourceStore.
    let source_store = SourceStore::open(&config, Arc::new(NoopSourceLifecycle)).unwrap();
    let (_, sources) = source_store.snapshot().unwrap();
    assert_eq!(sources.len(), 6);
    assert_eq!(
        sources.iter().filter(|r| r.trusted).count(),
        OFFICIAL_SOURCES.len()
    );
    assert!(!sources[2].enabled); // 18-lab-dev/vosu-overlays stayed disabled
    assert!(!sources[1].trusted); // HTTPS URL alias untrusted

    // Verify 1.x user tool via real ToolRegistry and byte-identical data/settings.
    assert_eq!(
        fs::read(data.join("tool-data/custom-user-tool/state.dat")).unwrap(),
        user_tool_data_before
    );
    assert_eq!(
        fs::read(config.join("tools/settings/custom-user-tool.json")).unwrap(),
        user_tool_settings_before
    );
    let registry = bounded!(ToolRegistry::new(
        config.join("tools"),
        data.join("tools"),
        cache.join("tools"),
        ExtraRoots::default(),
        "http://127.0.0.1:24123".to_owned(),
        Some("http://127.0.0.1:24124".to_owned()),
        semver::Version::new(2, 0, 0),
    ))
    .unwrap();
    let (status, tools_json) =
        tool_dashboard_json(&registry, Method::GET, "/api/tools", Bytes::new()).await;
    assert_eq!(status, StatusCode::OK);
    let custom_tool = tools_json["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "custom-user-tool")
        .expect("custom-user-tool visible in ToolRegistry");
    assert_eq!(custom_tool["enabled"], json!(false));
    assert_eq!(custom_tool["status"], json!("disabled"));
    assert_eq!(custom_tool["status_detail"], json!("needs-2.0-update"));
    bounded!(registry.shutdown());

    // Verify user overlays remain discoverable and enabled through real CounterService.
    let counters = CounterService::production(data.join("static"), config.clone(), cache.clone())
        .discover()
        .unwrap();
    let counter_names: Vec<&str> = counters.iter().map(|c| c.name.as_str()).collect();
    assert!(counter_names.contains(&"My User Overlay"));
    assert!(counter_names.contains(&"Community HUD"));

    // Verify expected deduplicated pending intents (no official user-tool or community-hud).
    let expected_intent_count = first_journal.pending_intents.len();
    assert_eq!(
        expected_intent_count, 8,
        "expected downloader(tool+af2) + lazer-library(af2) + legacy-official-helper(tool) + vosulay(tool+af2) + pp-counter(overlay) + retired-overlay(overlay)"
    );

    bounded!(first_cfg.shutdown()).unwrap();

    // Snapshot exact bytes of all 5 persisted migration outputs.
    let watched = [
        config.join("vosu.toml"),
        first_settings.path().to_path_buf(),
        config.join("catalog-source-settings.json"),
        config.join("tools/state.json"),
        journal_path(&config),
    ];
    let bytes_after_first: Vec<Vec<u8>> =
        watched.iter().map(|p| read_published(p).unwrap()).collect();

    // Second startup run MUST be byte-identical across all 5 files with zero duplicate intents.
    let (second_outcome, second_cfg, _) = run_release_startup(&config, &data).await.unwrap();
    let MigrationOutcome::AlreadyComplete(second_journal) = second_outcome else {
        panic!("expected AlreadyComplete on second run");
    };
    assert_eq!(second_journal, first_journal);
    assert_eq!(second_journal.pending_intents.len(), expected_intent_count);
    bounded!(second_cfg.shutdown()).unwrap();

    for (path, before) in watched.iter().zip(bytes_after_first.iter()) {
        assert_eq!(
            &read_published(path).unwrap(),
            before,
            "second startup mutated {}",
            path.display()
        );
    }
}

#[tokio::test]
async fn post_migration_edits_to_settings_and_toml_survive_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    seed_realistic_1x_fixture(&config, &data);

    let (_, first_cfg, first_settings) = run_release_startup(&config, &data).await.unwrap();
    bounded!(first_cfg.shutdown()).unwrap();

    // User edits both `plugins/settings.json` and `vosu.toml` after completion.
    // Prepend top-level key before any `[table]` header in `vosu.toml`.
    first_settings
        .store(
            OSU_PLUGIN_ID,
            &Map::from_iter([
                ("POLL_RATE".to_owned(), json!(777.0)),
                ("CALCULATE_PP".to_owned(), Value::Bool(true)),
            ]),
        )
        .unwrap();
    let toml_text = fs::read_to_string(config.join("vosu.toml")).unwrap();
    let edited_toml = format!("POST_MIGRATION_MARKER = \"keep-after-restart\"\n{toml_text}");
    fs::write(config.join("vosu.toml"), &edited_toml).unwrap();

    let (restarted_outcome, restarted_cfg, restarted_settings) =
        run_release_startup(&config, &data).await.unwrap();
    assert!(matches!(
        restarted_outcome,
        MigrationOutcome::AlreadyComplete(_)
    ));

    let osu = restarted_settings.load(OSU_PLUGIN_ID).unwrap();
    assert_eq!(osu.get("POLL_RATE"), Some(&json!(777.0)));
    assert_eq!(osu.get("CALCULATE_PP"), Some(&Value::Bool(true)));

    let table = read_toml_table(&config.join("vosu.toml"));
    assert_eq!(
        table
            .get("POST_MIGRATION_MARKER")
            .and_then(toml::Value::as_str),
        Some("keep-after-restart")
    );
    bounded!(restarted_cfg.shutdown()).unwrap();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CrashBoundary {
    SnapshotOnly,
    AfterPluginSettings,
    AfterCoreConfigWriteBeforeEnvDelete,
    AfterCoreConfig,
    AfterCatalogSources,
    AfterUserToolsWriteBeforeReceipt,
    BeforeLocalConversionComplete,
}

#[tokio::test]
async fn simulated_crash_at_every_durable_boundary_resumes_from_snapshot() {
    let schemas = bundled_migration_schemas();

    // Run once in a reference temp directory to capture the genuine production
    // snapshot, receipts, and destination bytes for the realistic 1.x fixture.
    let oracle_tmp = tempfile::tempdir().unwrap();
    let oracle_cfg = oracle_tmp.path().join("config");
    let oracle_data = oracle_tmp.path().join("data");
    seed_realistic_1x_fixture(&oracle_cfg, &oracle_data);
    let oracle_store = PluginSettingsStore::open(&oracle_cfg);
    let MigrationOutcome::Completed(oracle_journal) = run_local_migration(
        &oracle_cfg,
        &oracle_data,
        BuildChannel::Release,
        &schemas,
        &oracle_store,
    )
    .unwrap() else {
        panic!("expected oracle migration to complete");
    };
    let oracle_settings_bytes = fs::read(oracle_store.path()).unwrap();
    let oracle_toml_bytes = fs::read(oracle_cfg.join("vosu.toml")).unwrap();
    let oracle_sources_bytes = fs::read(oracle_cfg.join("catalog-source-settings.json")).unwrap();
    let oracle_tool_state_bytes = fs::read(oracle_cfg.join("tools/state.json")).unwrap();

    let boundaries = [
        CrashBoundary::SnapshotOnly,
        CrashBoundary::AfterPluginSettings,
        CrashBoundary::AfterCoreConfigWriteBeforeEnvDelete,
        CrashBoundary::AfterCoreConfig,
        CrashBoundary::AfterCatalogSources,
        CrashBoundary::AfterUserToolsWriteBeforeReceipt,
        CrashBoundary::BeforeLocalConversionComplete,
    ];

    for boundary in boundaries {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        seed_realistic_1x_fixture(&config, &data);

        // Construct the exact on-disk state & incomplete journal at `boundary`.
        let mut partial = MigrationJournal {
            version: JOURNAL_VERSION,
            generation: 0,
            snapshot: oracle_journal.snapshot.clone(),
            plugin_settings: StepState::Pending,
            core_config: StepState::Pending,
            catalog_sources: StepState::Pending,
            user_tools: StepState::Pending,
            local_conversion_complete: false,
            pending_intents: oracle_journal.pending_intents.clone(),
        };

        let mut edited_settings = false;
        let mut edited_toml = false;
        let mut edited_sources = false;

        if matches!(
            boundary,
            CrashBoundary::AfterPluginSettings
                | CrashBoundary::AfterCoreConfigWriteBeforeEnvDelete
                | CrashBoundary::AfterCoreConfig
                | CrashBoundary::AfterCatalogSources
                | CrashBoundary::AfterUserToolsWriteBeforeReceipt
                | CrashBoundary::BeforeLocalConversionComplete
        ) {
            fs::create_dir_all(config.join("plugins")).unwrap();
            fs::write(config.join("plugins/settings.json"), &oracle_settings_bytes).unwrap();
            partial.plugin_settings = oracle_journal.plugin_settings.clone();

            // Simulate a post-step user edit to already-committed plugin settings.
            let edit_store = PluginSettingsStore::open(&config);
            let mut osu = edit_store.load(OSU_PLUGIN_ID).unwrap();
            osu.insert("POLL_RATE".to_owned(), json!(888.0));
            edit_store.store(OSU_PLUGIN_ID, &osu).unwrap();
            edited_settings = true;
        }

        if boundary == CrashBoundary::AfterCoreConfigWriteBeforeEnvDelete {
            // Canonical vosu.toml published, but env files not yet deleted and receipt still Pending.
            fs::write(config.join("vosu.toml"), &oracle_toml_bytes).unwrap();
        }

        if matches!(
            boundary,
            CrashBoundary::AfterCoreConfig
                | CrashBoundary::AfterCatalogSources
                | CrashBoundary::AfterUserToolsWriteBeforeReceipt
                | CrashBoundary::BeforeLocalConversionComplete
        ) {
            fs::write(config.join("vosu.toml"), &oracle_toml_bytes).unwrap();
            let _ = fs::remove_file(config.join("vosu.env"));
            let _ = fs::remove_file(config.join(pre_rename_env_filename()));
            partial.core_config = oracle_journal.core_config.clone();

            // Simulate a post-step user edit to already-committed vosu.toml
            // (prepended at top level before any `[table]` header).
            let text = fs::read_to_string(config.join("vosu.toml")).unwrap();
            let edited = format!("CRASH_EDIT_SURVIVES = true\n{text}");
            fs::write(config.join("vosu.toml"), edited).unwrap();
            edited_toml = true;
        }

        if matches!(
            boundary,
            CrashBoundary::AfterCatalogSources
                | CrashBoundary::AfterUserToolsWriteBeforeReceipt
                | CrashBoundary::BeforeLocalConversionComplete
        ) {
            fs::write(
                config.join("catalog-source-settings.json"),
                &oracle_sources_bytes,
            )
            .unwrap();
            partial.catalog_sources = oracle_journal.catalog_sources.clone();

            // Simulate user enabling a source in already-committed catalog-source-settings.json.
            let mut records: Vec<Value> = serde_json::from_slice(&oracle_sources_bytes).unwrap();
            records[2]["enabled"] = json!(true);
            fs::write(
                config.join("catalog-source-settings.json"),
                serde_json::to_vec_pretty(&records).unwrap(),
            )
            .unwrap();
            edited_sources = true;
        }

        if boundary == CrashBoundary::AfterUserToolsWriteBeforeReceipt {
            // `tools/state.json` written with disabled user tool, receipt still Pending.
            fs::write(config.join("tools/state.json"), &oracle_tool_state_bytes).unwrap();
        }

        if boundary == CrashBoundary::BeforeLocalConversionComplete {
            fs::write(config.join("tools/state.json"), &oracle_tool_state_bytes).unwrap();
            partial.user_tools = oracle_journal.user_tools.clone();
        }

        save_journal(&config, &mut partial).unwrap();

        // Poison/mutate remaining legacy files on disk BEFORE replay:
        // replay MUST resume strictly from `partial.snapshot` and NEVER reread mutated legacy files!
        if config.join("vosu.env").exists() {
            fs::write(
                config.join("vosu.env"),
                "POLL_RATE=9999\nCALCULATE_PP=true\n",
            )
            .unwrap();
        }
        if config.join(pre_rename_env_filename()).exists() {
            fs::write(
                config.join(pre_rename_env_filename()),
                "READ_MANIA_SCROLL_SPEED=true\n",
            )
            .unwrap();
        }
        if !edited_toml {
            fs::write(
                config.join("vosu.toml"),
                "SERVER_PORT = 29999.0\nPOLL_RATE = 9999.0\n",
            )
            .unwrap();
        }
        fs::write(
            config.join("tools/catalog-sources.json"),
            r#"[{"source":"poisoned-org/must-not-be-read","enabled":true}]"#,
        )
        .unwrap();

        // Resume through production startup.
        let (resumed_outcome, cfg, settings_store) =
            run_release_startup(&config, &data).await.unwrap();
        let MigrationOutcome::Completed(done) = resumed_outcome else {
            panic!("expected Completed after resuming from {boundary:?}");
        };
        assert!(done.local_conversion_complete, "{boundary:?}");
        assert!(!config.join("vosu.env").exists(), "{boundary:?}");
        assert!(
            !config.join(pre_rename_env_filename()).exists(),
            "{boundary:?}"
        );
        assert_eq!(
            done.pending_intents, oracle_journal.pending_intents,
            "duplicate or drifted intents at {boundary:?}"
        );

        // Plugin settings: post-step edit survived OR snapshot 240.0 won (never poisoned 9999.0!).
        let osu = settings_store.load(OSU_PLUGIN_ID).unwrap();
        if edited_settings {
            assert_eq!(osu.get("POLL_RATE"), Some(&json!(888.0)), "{boundary:?}");
        } else {
            assert_eq!(osu.get("POLL_RATE"), Some(&json!(240.0)), "{boundary:?}");
        }
        assert_eq!(
            osu.get("CALCULATE_PP"),
            Some(&Value::Bool(false)),
            "{boundary:?}"
        );
        assert_eq!(
            osu.get("READ_MANIA_SCROLL_SPEED"),
            Some(&Value::Bool(false)),
            "{boundary:?}"
        );

        // Core config: post-step edit survived OR snapshot port 24123.0 won (never poisoned 29999.0!).
        if edited_toml {
            let table = read_toml_table(&config.join("vosu.toml"));
            assert_eq!(
                table
                    .get("CRASH_EDIT_SURVIVES")
                    .and_then(toml::Value::as_bool),
                Some(true),
                "{boundary:?}"
            );
        } else {
            assert_eq!(cfg.current().server_port, 24_123.0, "{boundary:?}");
        }

        // Catalog sources: poisoned legacy file NEVER read; committed edit survived.
        let src_store = SourceStore::open(&config, Arc::new(NoopSourceLifecycle)).unwrap();
        let (_, records) = src_store.snapshot().unwrap();
        assert_eq!(records.len(), 6, "{boundary:?}");
        assert!(
            records
                .iter()
                .all(|r| r.identity.as_str() != "poisoned-org/must-not-be-read"),
            "mutated legacy catalog-sources.json leaked during replay at {boundary:?}"
        );
        if edited_sources {
            assert!(
                records[2].enabled,
                "committed source edit lost at {boundary:?}"
            );
        } else {
            assert!(!records[2].enabled, "{boundary:?}");
        }

        // User tool disabled cleanly.
        let tool_state = read_json_value(&config.join("tools/state.json"));
        assert_eq!(
            tool_state["custom-user-tool"]["enabled"],
            json!(false),
            "{boundary:?}"
        );

        bounded!(cfg.shutdown()).unwrap();
    }
}

#[test]
fn inconsistent_complete_marker_and_corrupt_journal_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    seed_realistic_1x_fixture(&config, &data);

    // Run once in a throwaway dir to get a structurally valid snapshot.
    let ref_tmp = tempfile::tempdir().unwrap();
    let ref_cfg = ref_tmp.path().join("config");
    let ref_data = ref_tmp.path().join("data");
    seed_realistic_1x_fixture(&ref_cfg, &ref_data);
    let schemas = bundled_migration_schemas();
    let ref_store = PluginSettingsStore::open(&ref_cfg);
    let MigrationOutcome::Completed(mut bogus) = run_local_migration(
        &ref_cfg,
        &ref_data,
        BuildChannel::Release,
        &schemas,
        &ref_store,
    )
    .unwrap() else {
        panic!("expected Completed");
    };

    // Craft an invariant-violating journal: `local_conversion_complete: true`
    // while `plugin_settings` is still `Pending` (M8-M04).
    bogus.plugin_settings = StepState::Pending;
    bogus.local_conversion_complete = true;
    let bogus_bytes = serde_json::to_vec_pretty(&bogus).unwrap();
    fs::write(journal_path(&config), &bogus_bytes).unwrap();

    let store = PluginSettingsStore::open(&config);
    let err =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap_err();
    assert_eq!(err, MigrationError::JournalCorrupt);
    assert_eq!(fs::read(journal_path(&config)).unwrap(), bogus_bytes);
    assert!(config.join("vosu.env").exists());
    assert!(config.join(pre_rename_env_filename()).exists());
    assert!(load_journal(&config).is_err());
}

/// Verifies M8-X01: when a destination file cannot be published (exclusively
/// locked `vosu.toml` on native Windows, or unwritable `plugins/` directory on
/// non-root Unix), migration fails closed without deleting legacy inputs or
/// marking `local_conversion_complete`, and resumes cleanly once unlocked.
#[test]
fn locked_destination_fails_closed_and_resumes_cleanly_once_unlocked() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    seed_realistic_1x_fixture(&config, &data);

    let schemas = bundled_migration_schemas();
    let store = PluginSettingsStore::open(&config);

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;

        // Hold an exclusive lock (share_mode = 0) on `vosu.toml` so
        // `atomic_replace` cannot replace it after `plugin_settings` commits.
        // Note: snapshot reads `vosu.toml` first, so pre-stage the snapshot
        // journal before locking `vosu.toml` exclusively.
        let ref_tmp = tempfile::tempdir().unwrap();
        let ref_cfg = ref_tmp.path().join("config");
        let ref_data = ref_tmp.path().join("data");
        seed_realistic_1x_fixture(&ref_cfg, &ref_data);
        let ref_store = PluginSettingsStore::open(&ref_cfg);
        let MigrationOutcome::Completed(oracle) = run_local_migration(
            &ref_cfg,
            &ref_data,
            BuildChannel::Release,
            &schemas,
            &ref_store,
        )
        .unwrap() else {
            panic!("expected oracle Completed");
        };
        let mut snap_only = MigrationJournal {
            version: JOURNAL_VERSION,
            generation: 0,
            snapshot: oracle.snapshot,
            plugin_settings: StepState::Pending,
            core_config: StepState::Pending,
            catalog_sources: StepState::Pending,
            user_tools: StepState::Pending,
            local_conversion_complete: false,
            pending_intents: oracle.pending_intents,
        };
        save_journal(&config, &mut snap_only).unwrap();

        let lock_handle = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(config.join("vosu.toml"))
            .unwrap();

        let err = run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store)
            .expect_err("exclusively locked vosu.toml must fail closed");
        assert!(
            matches!(
                err,
                MigrationError::Io {
                    label: "vosu.toml",
                    ..
                }
            ),
            "unexpected error on locked vosu.toml: {err:?}"
        );

        let mid = load_journal(&config).unwrap().expect("journal exists");
        assert!(!mid.local_conversion_complete);
        assert!(mid.plugin_settings.is_completed());
        assert!(!mid.core_config.is_completed());
        assert!(config.join("vosu.env").exists());
        assert!(config.join(pre_rename_env_filename()).exists());

        drop(lock_handle);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        // Pre-create `plugins/` with read-only permissions so Step 1 cannot
        // stage `plugins/settings.json` after the snapshot journal is written.
        let plugins_dir = config.join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();
        fs::set_permissions(&plugins_dir, fs::Permissions::from_mode(0o555)).unwrap();

        // Only assert the write failure when running unprivileged (root bypasses 0555).
        let can_bypass = fs::write(plugins_dir.join(".root-probe"), b"").is_ok();
        if can_bypass {
            let _ = fs::remove_file(plugins_dir.join(".root-probe"));
            fs::set_permissions(&plugins_dir, fs::Permissions::from_mode(0o755)).unwrap();
        } else {
            let err = run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store)
                .expect_err("unwritable plugins dir must fail closed");
            assert_eq!(err, MigrationError::PluginSettingsStoreFailed);

            let mid = load_journal(&config)
                .unwrap()
                .expect("snapshot journal exists");
            assert!(!mid.local_conversion_complete);
            assert!(!mid.plugin_settings.is_completed());
            assert!(config.join("vosu.env").exists());
            assert!(config.join(pre_rename_env_filename()).exists());

            fs::set_permissions(&plugins_dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    // Once unlocked/writable, replay completes cleanly from the durable journal.
    let fresh_store = PluginSettingsStore::open(&config);
    let MigrationOutcome::Completed(done) = run_local_migration(
        &config,
        &data,
        BuildChannel::Release,
        &schemas,
        &fresh_store,
    )
    .unwrap() else {
        panic!("expected Completed after unlocking destination");
    };
    assert!(done.local_conversion_complete);
    assert!(!config.join("vosu.env").exists());
    assert!(!config.join(pre_rename_env_filename()).exists());
    assert_eq!(
        fresh_store.load(OSU_PLUGIN_ID).unwrap().get("POLL_RATE"),
        Some(&json!(240.0))
    );
}
