//! Plugin settings destination precedence, post-migration edit survival,
//! corrupt/oversized fail-closed behavior, and fresh 2.0 install invariants
//! (M8-S01, M8-M04, M8-M05, M8-X02).

use std::fs;

use axum::body::Bytes;
use axum::http::{Method, StatusCode};
use serde_json::{Map, Value, json};
use vosu_core::config::BINDING_ORDER;
use vosu_core::migration::{
    MigrationError, MigrationOutcome, OSU_PLUGIN_ID, OVERLAY_HOST_PLUGIN_ID,
    OVERLAY_HOST_SETTING_KEY, journal_path, run_local_migration,
};
use vosu_core::plugin::settings::{MAX_FILE_BYTES, PluginSettingsStore, SettingsStorage};
use vosu_core::tools::{ExtraRoots, ToolRegistry};
use vosu_proptest::bounded;
use vosu_sys::platform::BuildChannel;

use crate::support::{
    bundled_migration_schemas, pre_rename_env_filename, read_toml_table, run_release_startup,
    tool_dashboard_json,
};

#[tokio::test]
async fn existing_valid_plugin_settings_win_per_explicit_key_over_legacy() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();

    // Seed existing valid 2.0 overrides before migration runs.
    let pre_store = PluginSettingsStore::open(&config);
    pre_store
        .store(
            OSU_PLUGIN_ID,
            &Map::from_iter([
                ("POLL_RATE".to_owned(), json!(444.0)),
                ("CALCULATE_PP".to_owned(), Value::Bool(false)),
            ]),
        )
        .unwrap();
    pre_store
        .store(
            OVERLAY_HOST_PLUGIN_ID,
            &Map::from_iter([(
                OVERLAY_HOST_SETTING_KEY.to_owned(),
                json!("/existing-overlay-root"),
            )]),
        )
        .unwrap();

    // Legacy 1.x config tries to set all 6 moved keys to different values.
    fs::write(
        config.join("vosu.toml"),
        "\
CALCULATE_PP = true
POLL_RATE = 125.0
PRECISE_DATA_POLL_RATE = 8.0
SHOW_MP_COMMANDS = true
READ_MANIA_SCROLL_SPEED = false
STATIC_FOLDER_PATH = \"/legacy-overlay-root\"
",
    )
    .unwrap();

    let (outcome, config_store, settings_store) =
        run_release_startup(&config, &data).await.unwrap();
    let MigrationOutcome::Completed(journal) = outcome else {
        panic!("expected Completed");
    };

    let osu = settings_store.load(OSU_PLUGIN_ID).unwrap();
    // Existing destination keys won:
    assert_eq!(osu.get("POLL_RATE"), Some(&json!(444.0)));
    assert_eq!(osu.get("CALCULATE_PP"), Some(&Value::Bool(false)));
    // Missing keys imported from 1.x:
    assert_eq!(osu.get("PRECISE_DATA_POLL_RATE"), Some(&json!(8.0)));
    assert_eq!(osu.get("SHOW_MP_COMMANDS"), Some(&Value::Bool(true)));
    assert_eq!(
        osu.get("READ_MANIA_SCROLL_SPEED"),
        Some(&Value::Bool(false))
    );

    let overlay = settings_store.load(OVERLAY_HOST_PLUGIN_ID).unwrap();
    assert_eq!(
        overlay.get(OVERLAY_HOST_SETTING_KEY),
        Some(&json!("/existing-overlay-root"))
    );

    let receipt = journal.plugin_settings.receipt().unwrap();
    assert_eq!(
        receipt.osu_preserved_keys,
        vec!["CALCULATE_PP", "POLL_RATE"]
    );
    assert_eq!(
        receipt.osu_imported_keys,
        vec![
            "PRECISE_DATA_POLL_RATE",
            "READ_MANIA_SCROLL_SPEED",
            "SHOW_MP_COMMANDS",
        ]
    );
    assert_eq!(
        receipt.overlay_host_preserved_keys,
        vec![OVERLAY_HOST_SETTING_KEY]
    );
    assert!(receipt.overlay_host_imported_keys.is_empty());

    bounded!(config_store.shutdown()).unwrap();
}

#[tokio::test]
async fn post_migration_plugin_settings_edits_survive_subsequent_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();

    fs::write(
        config.join("vosu.toml"),
        "POLL_RATE = 180.0\nCALCULATE_PP = true\nSTATIC_FOLDER_PATH = \"initial\"\n",
    )
    .unwrap();

    let (_, first_cfg, first_settings) = run_release_startup(&config, &data).await.unwrap();
    bounded!(first_cfg.shutdown()).unwrap();

    // Post-migration user edits via PluginSettingsStore.
    first_settings
        .store(
            OSU_PLUGIN_ID,
            &Map::from_iter([
                ("POLL_RATE".to_owned(), json!(612.0)),
                ("CALCULATE_PP".to_owned(), Value::Bool(false)),
            ]),
        )
        .unwrap();
    first_settings
        .store(
            OVERLAY_HOST_PLUGIN_ID,
            &Map::from_iter([(
                OVERLAY_HOST_SETTING_KEY.to_owned(),
                json!("user-edited-dir"),
            )]),
        )
        .unwrap();
    let edited_bytes = fs::read(first_settings.path()).unwrap();

    // Restart twice and verify edits survive intact.
    for _ in 0..2 {
        let (outcome, cfg, restarted_settings) = run_release_startup(&config, &data).await.unwrap();
        assert!(matches!(outcome, MigrationOutcome::AlreadyComplete(_)));
        assert_eq!(fs::read(restarted_settings.path()).unwrap(), edited_bytes);
        let osu = restarted_settings.load(OSU_PLUGIN_ID).unwrap();
        assert_eq!(osu.get("POLL_RATE"), Some(&json!(612.0)));
        assert_eq!(osu.get("CALCULATE_PP"), Some(&Value::Bool(false)));
        let overlay = restarted_settings.load(OVERLAY_HOST_PLUGIN_ID).unwrap();
        assert_eq!(
            overlay.get(OVERLAY_HOST_SETTING_KEY),
            Some(&json!("user-edited-dir"))
        );
        bounded!(cfg.shutdown()).unwrap();
    }
}

#[test]
fn corrupt_and_oversized_plugin_settings_fail_closed_without_touching_legacy() {
    let schemas = bundled_migration_schemas();
    let cases: [(&str, Vec<u8>); 3] = [
        ("corrupt_json", b"{not valid json".to_vec()),
        (
            "unsupported_version",
            br#"{"version":99,"plugins":{}}"#.to_vec(),
        ),
        ("oversized", vec![b' '; (MAX_FILE_BYTES + 16) as usize]),
    ];

    for (label, bad_bytes) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        fs::create_dir_all(config.join("plugins")).unwrap();
        fs::create_dir_all(&data).unwrap();

        let settings_path = config.join("plugins").join("settings.json");
        fs::write(&settings_path, &bad_bytes).unwrap();

        let toml_bytes = b"POLL_RATE = 210.0\nV2_SUBSCRIPTION_DEBOUNCE = 15.0\n";
        let env_bytes = b"CALCULATE_PP=false\n";
        let pre_env_bytes = b"SHOW_MP_COMMANDS=true\n";
        fs::write(config.join("vosu.toml"), toml_bytes).unwrap();
        fs::write(config.join("vosu.env"), env_bytes).unwrap();
        fs::write(config.join(pre_rename_env_filename()), pre_env_bytes).unwrap();

        let store = PluginSettingsStore::open(&config);
        let err = run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store)
            .expect_err(&format!("{label} must fail closed"));
        assert_eq!(
            err,
            MigrationError::PluginSettingsStoreFailed,
            "case {label}"
        );

        // Corrupt destination was NOT overwritten or reset, no journal written,
        // and all legacy files remain completely intact.
        assert_eq!(fs::read(&settings_path).unwrap(), bad_bytes, "case {label}");
        assert!(!journal_path(&config).exists(), "case {label}");
        assert_eq!(
            fs::read(config.join("vosu.toml")).unwrap(),
            toml_bytes,
            "case {label}"
        );
        assert_eq!(
            fs::read(config.join("vosu.env")).unwrap(),
            env_bytes,
            "case {label}"
        );
        assert_eq!(
            fs::read(config.join(pre_rename_env_filename())).unwrap(),
            pre_env_bytes,
            "case {label}"
        );
    }
}

#[tokio::test]
async fn fresh_2_0_install_creates_clean_defaults_with_zero_fake_legacy_records() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(&cache).unwrap();

    let (outcome, config_store, settings_store) =
        run_release_startup(&config, &data).await.unwrap();
    let MigrationOutcome::Completed(journal) = outcome else {
        panic!("expected Completed on fresh install");
    };
    assert!(journal.local_conversion_complete);
    assert!(journal.pending_intents.is_empty());
    assert!(journal.snapshot.user_tools_to_disable.is_empty());
    assert!(journal.snapshot.discovered_official_tools.is_empty());
    assert!(journal.snapshot.discovered_official_overlays.is_empty());
    assert!(journal.snapshot.catalog_sources.is_none());
    assert!(journal.snapshot.osu_overrides.is_empty());
    assert!(journal.snapshot.overlay_host_overrides.is_empty());

    // No fake legacy state files created.
    assert!(!config.join("tools").join("state.json").exists());
    assert!(!config.join("catalog-source-settings.json").exists());
    assert!(settings_store.load(OSU_PLUGIN_ID).unwrap().is_empty());
    assert!(
        settings_store
            .load(OVERLAY_HOST_PLUGIN_ID)
            .unwrap()
            .is_empty()
    );

    // Canonical default vosu.toml contains only the 12 core bindings.
    let table = read_toml_table(&config.join("vosu.toml"));
    assert_eq!(table.len(), BINDING_ORDER.len());
    for key in BINDING_ORDER {
        assert!(table.contains_key(key), "missing default core key {key}");
    }

    // Real ToolRegistry reports zero tools (no fake disabled 1.x entries).
    let registry = bounded!(ToolRegistry::new(
        config.join("tools"),
        data.join("tools"),
        cache.join("tools"),
        ExtraRoots::default(),
        "http://127.0.0.1:24050".to_owned(),
        Some("http://127.0.0.1:24051".to_owned()),
        semver::Version::new(2, 0, 0),
    ))
    .unwrap();
    let (status, body) =
        tool_dashboard_json(&registry, Method::GET, "/api/tools", Bytes::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tools"], json!([]));

    bounded!(registry.shutdown());
    bounded!(config_store.shutdown()).unwrap();
}
