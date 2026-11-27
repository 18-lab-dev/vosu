//! Config precedence, moved/retired key conversion, unknown/nested preservation,
//! fail-closed malformed input handling, and post-migration B2-removal checks
//! (M8-M01, M8-M02, M8-M03, M8-M05).

use std::fs;

use serde_json::{Value, json};
use vosu_core::migration::{
    MigrationError, MigrationOutcome, OSU_MOVED_KEYS, OSU_PLUGIN_ID, OVERLAY_HOST_LEGACY_KEY,
    OVERLAY_HOST_PLUGIN_ID, OVERLAY_HOST_SETTING_KEY, RETIRED_KEYS, journal_path,
    run_local_migration,
};
use vosu_core::plugin::settings::{PluginSettingsStore, SettingsStorage};
use vosu_proptest::bounded;
use vosu_sys::platform::BuildChannel;

use crate::support::{
    bundled_migration_schemas, pre_rename_env_filename, read_toml_table, run_release_startup,
};

#[tokio::test]
async fn full_three_tier_precedence_across_all_ten_moved_and_retired_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();

    // Tier 3 (lowest): pre-rename env supplies all 10 keys.
    fs::write(
        config.join(pre_rename_env_filename()),
        "\
CALCULATE_PP=false
POLL_RATE=50
PRECISE_DATA_POLL_RATE=0
SHOW_MP_COMMANDS=true
READ_MANIA_SCROLL_SPEED=false
STATIC_FOLDER_PATH=\"/from-pre-rename\"
ENABLE_KEY_OVERLAY=true
ENABLE_MANIA_KEY_HIT_ERRORS=true
ENABLE_HIT_OFFSETS=true
V2_SUBSCRIPTION_DEBOUNCE=11
SERVER_IP=\"127.0.0.2\"
ALLOWED_IPS=\"127.0.0.1,from-pre-rename\"
",
    )
    .unwrap();

    // Tier 2 (middle): vosu.env overrides a subset of moved + retired + core keys.
    fs::write(
        config.join("vosu.env"),
        "\
POLL_RATE=185
PRECISE_DATA_POLL_RATE=5
SHOW_MP_COMMANDS=false
STATIC_FOLDER_PATH=\"./from-vosu-env\"
ENABLE_KEY_OVERLAY=false
V2_SUBSCRIPTION_DEBOUNCE=22
DASHBOARD_PORT=24111
API_V1_SUBSCRIPTION_DEBOUNCE=4.25
",
    )
    .unwrap();

    // Tier 1 (highest): vosu.toml overrides POLL_RATE, STATIC_FOLDER_PATH, and retired keys.
    fs::write(
        config.join("vosu.toml"),
        "\
SERVER_PORT = 24222.0
POLL_RATE = 333.0
STATIC_FOLDER_PATH = \"custom/from-toml\"
ENABLE_MANIA_KEY_HIT_ERRORS = false
ENABLE_HIT_OFFSETS = false
V2_SUBSCRIPTION_DEBOUNCE = 99.0
",
    )
    .unwrap();

    let (outcome, config_store, settings_store) =
        run_release_startup(&config, &data).await.unwrap();
    let MigrationOutcome::Completed(journal) = outcome else {
        panic!("expected Completed on first run");
    };
    assert!(journal.local_conversion_complete);

    // Both legacy env files removed only after durable publication.
    assert!(!config.join("vosu.env").exists());
    assert!(!config.join(pre_rename_env_filename()).exists());

    // Verify exact 3-tier precedence winners in real PluginSettingsStore:
    // - CALCULATE_PP: false (from pre-rename env)
    // - READ_MANIA_SCROLL_SPEED: false (from pre-rename env)
    // - SHOW_MP_COMMANDS: false (vosu.env beat pre-rename env true)
    // - PRECISE_DATA_POLL_RATE: 5.0 (vosu.env beat pre-rename env 0)
    // - POLL_RATE: 333.0 (vosu.toml beat vosu.env 185 & pre-rename 50)
    // - STATIC_FOLDER_PATH -> static_folder_path: "custom/from-toml" (vosu.toml beat both envs)
    let osu = settings_store.load(OSU_PLUGIN_ID).unwrap();
    assert_eq!(osu.get("CALCULATE_PP"), Some(&Value::Bool(false)));
    assert_eq!(
        osu.get("READ_MANIA_SCROLL_SPEED"),
        Some(&Value::Bool(false))
    );
    assert_eq!(osu.get("SHOW_MP_COMMANDS"), Some(&Value::Bool(false)));
    assert_eq!(osu.get("PRECISE_DATA_POLL_RATE"), Some(&json!(5.0)));
    assert_eq!(osu.get("POLL_RATE"), Some(&json!(333.0)));

    let overlay = settings_store.load(OVERLAY_HOST_PLUGIN_ID).unwrap();
    assert_eq!(
        overlay.get(OVERLAY_HOST_SETTING_KEY),
        Some(&json!("custom/from-toml"))
    );

    // Core config values preserved with exact 3-tier precedence.
    let current = config_store.current();
    assert_eq!(current.server_port, 24_222.0);
    assert_eq!(current.dashboard_port, 24_111.0);
    assert_eq!(current.api_v1_subscription_debounce, 4.25);
    assert_eq!(current.server_ip, "127.0.0.2");
    assert_eq!(current.allowed_ips, "127.0.0.1,from-pre-rename");

    // All 10 moved + retired top-level keys stripped from canonical `vosu.toml`.
    let table = read_toml_table(&config.join("vosu.toml"));
    for key in OSU_MOVED_KEYS
        .iter()
        .chain(std::iter::once(&OVERLAY_HOST_LEGACY_KEY))
        .chain(RETIRED_KEYS.iter())
    {
        assert!(
            !table.contains_key(*key),
            "top-level {key} must be stripped from canonical vosu.toml"
        );
    }
    assert!(table.contains_key("API_V1_SUBSCRIPTION_DEBOUNCE"));

    let core_receipt = journal.core_config.receipt().unwrap();
    assert_eq!(core_receipt.stripped_keys.len(), 10);

    bounded!(config_store.shutdown()).unwrap();
}

#[tokio::test]
async fn explicit_false_zero_empty_preserved_and_absent_keys_stay_absent() {
    let schemas = bundled_migration_schemas();

    // Case A: explicit false, 0 (clamped to schema min 1.0 / 100.0), and "" preserved verbatim.
    let tmp_explicit = tempfile::tempdir().unwrap();
    let cfg_a = tmp_explicit.path().join("config");
    let data_a = tmp_explicit.path().join("data");
    fs::create_dir_all(&cfg_a).unwrap();
    fs::create_dir_all(&data_a).unwrap();
    fs::write(
        cfg_a.join("vosu.toml"),
        "\
CALCULATE_PP = false
SHOW_MP_COMMANDS = false
READ_MANIA_SCROLL_SPEED = false
POLL_RATE = 0
PRECISE_DATA_POLL_RATE = 0
STATIC_FOLDER_PATH = \"\"
",
    )
    .unwrap();

    let (_, store_a, settings_a) = run_release_startup(&cfg_a, &data_a).await.unwrap();
    let osu_a = settings_a.load(OSU_PLUGIN_ID).unwrap();
    assert_eq!(osu_a.get("CALCULATE_PP"), Some(&Value::Bool(false)));
    assert_eq!(osu_a.get("SHOW_MP_COMMANDS"), Some(&Value::Bool(false)));
    assert_eq!(
        osu_a.get("READ_MANIA_SCROLL_SPEED"),
        Some(&Value::Bool(false))
    );
    assert_eq!(osu_a.get("POLL_RATE"), Some(&json!(100.0)));
    assert_eq!(osu_a.get("PRECISE_DATA_POLL_RATE"), Some(&json!(1.0)));

    let overlay_a = settings_a.load(OVERLAY_HOST_PLUGIN_ID).unwrap();
    assert_eq!(overlay_a.get(OVERLAY_HOST_SETTING_KEY), Some(&json!("")));
    bounded!(store_a.shutdown()).unwrap();

    // Case B: only POLL_RATE present -> absent keys MUST stay absent from stored
    // overrides while defaults apply at read time through SettingsSchema::merge.
    let tmp_sparse = tempfile::tempdir().unwrap();
    let cfg_b = tmp_sparse.path().join("config");
    let data_b = tmp_sparse.path().join("data");
    fs::create_dir_all(&cfg_b).unwrap();
    fs::create_dir_all(&data_b).unwrap();
    fs::write(cfg_b.join("vosu.toml"), "POLL_RATE = 250.0\n").unwrap();

    let (_, store_b, settings_b) = run_release_startup(&cfg_b, &data_b).await.unwrap();
    let osu_b = settings_b.load(OSU_PLUGIN_ID).unwrap();
    assert_eq!(osu_b.len(), 1);
    assert_eq!(osu_b.get("POLL_RATE"), Some(&json!(250.0)));
    assert!(!osu_b.contains_key("CALCULATE_PP"));
    assert!(!osu_b.contains_key("PRECISE_DATA_POLL_RATE"));
    assert!(!osu_b.contains_key("SHOW_MP_COMMANDS"));
    assert!(!osu_b.contains_key("READ_MANIA_SCROLL_SPEED"));
    assert!(settings_b.load(OVERLAY_HOST_PLUGIN_ID).unwrap().is_empty());

    // Effective settings apply schema defaults for absent keys.
    let effective = schemas.osu.merge(&osu_b).unwrap();
    assert_eq!(effective.get("CALCULATE_PP"), Some(&Value::Bool(true)));
    assert_eq!(effective.get("POLL_RATE"), Some(&json!(250.0)));
    assert_eq!(effective.get("PRECISE_DATA_POLL_RATE"), Some(&json!(10.0)));
    assert_eq!(effective.get("SHOW_MP_COMMANDS"), Some(&Value::Bool(false)));
    assert_eq!(
        effective.get("READ_MANIA_SCROLL_SPEED"),
        Some(&Value::Bool(true))
    );
    bounded!(store_b.shutdown()).unwrap();
}

#[tokio::test]
async fn preserves_api_v1_debounce_unknown_keys_and_nested_matching_leaf_names() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();

    fs::write(
        config.join("vosu.toml"),
        "\
API_V1_SUBSCRIPTION_DEBOUNCE = 7.5
POLL_RATE = 200.0
STATIC_FOLDER_PATH = \"/top-moved\"
ENABLE_KEY_OVERLAY = true
ENABLE_MANIA_KEY_HIT_ERRORS = true
ENABLE_HIT_OFFSETS = true
V2_SUBSCRIPTION_DEBOUNCE = 55.0
UNRELATED_TOP_KEY = \"keep-top-verbatim\"

[custom]
POLL_RATE = 999
STATIC_FOLDER_PATH = \"/keep\"
ENABLE_KEY_OVERLAY = false
V2_SUBSCRIPTION_DEBOUNCE = 888

[custom.nested]
CALCULATE_PP = \"user-string-must-survive\"
",
    )
    .unwrap();

    let (_, config_store, _) = run_release_startup(&config, &data).await.unwrap();
    assert_eq!(config_store.current().api_v1_subscription_debounce, 7.5);
    assert!(config_store.update(|c| c.debug_log = true));
    bounded!(config_store.flush()).unwrap();
    bounded!(config_store.shutdown()).unwrap();

    let table = read_toml_table(&config.join("vosu.toml"));
    assert_eq!(
        table
            .get("API_V1_SUBSCRIPTION_DEBOUNCE")
            .and_then(toml::Value::as_float),
        Some(7.5)
    );
    assert_eq!(
        table.get("UNRELATED_TOP_KEY").and_then(toml::Value::as_str),
        Some("keep-top-verbatim")
    );
    assert!(!table.contains_key("POLL_RATE"));
    assert!(!table.contains_key("STATIC_FOLDER_PATH"));
    assert!(!table.contains_key("ENABLE_KEY_OVERLAY"));
    assert!(!table.contains_key("V2_SUBSCRIPTION_DEBOUNCE"));

    let custom = table
        .get("custom")
        .and_then(toml::Value::as_table)
        .expect("[custom] table must be preserved");
    assert_eq!(
        custom.get("POLL_RATE").and_then(toml::Value::as_integer),
        Some(999)
    );
    assert_eq!(
        custom
            .get("STATIC_FOLDER_PATH")
            .and_then(toml::Value::as_str),
        Some("/keep")
    );
    assert_eq!(
        custom
            .get("ENABLE_KEY_OVERLAY")
            .and_then(toml::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        custom
            .get("V2_SUBSCRIPTION_DEBOUNCE")
            .and_then(toml::Value::as_integer),
        Some(888)
    );
    assert_eq!(
        custom
            .get("nested")
            .and_then(toml::Value::as_table)
            .and_then(|t| t.get("CALCULATE_PP"))
            .and_then(toml::Value::as_str),
        Some("user-string-must-survive")
    );
}

#[test]
fn malformed_toml_env_and_invalid_values_fail_closed_with_redacted_diagnostics() {
    let schemas = bundled_migration_schemas();
    let secret_sentinel = "SUPER_SECRET_SENTINEL_918273";

    let cases: [(&str, &str, &str); 5] = [
        (
            "malformed_toml",
            "vosu.toml",
            "POLL_RATE = [\"SUPER_SECRET_SENTINEL_918273\"\n",
        ),
        (
            "malformed_vosu_env_unterminated_quote",
            "vosu.env",
            "STATIC_FOLDER_PATH=\"SUPER_SECRET_SENTINEL_918273\n",
        ),
        (
            "malformed_pre_rename_env_no_separator",
            "PRE_RENAME",
            "BROKEN_LINE_WITHOUT_EQUALS_SUPER_SECRET_SENTINEL_918273\n",
        ),
        (
            "invalid_poll_rate_type_in_toml",
            "vosu.toml",
            "POLL_RATE = \"SUPER_SECRET_SENTINEL_918273\"\n",
        ),
        (
            "invalid_bool_in_env",
            "vosu.env",
            "CALCULATE_PP=SUPER_SECRET_SENTINEL_918273\n",
        ),
    ];

    for (name, target_kind, bad_content) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&data).unwrap();

        let target_name = if target_kind == "PRE_RENAME" {
            pre_rename_env_filename()
        } else {
            target_kind.to_owned()
        };
        let target_path = config.join(&target_name);
        fs::write(&target_path, bad_content).unwrap();

        // Also place a valid sibling file that must stay untouched on failure.
        let sibling_path = if target_name == "vosu.toml" {
            config.join("vosu.env")
        } else {
            config.join("vosu.toml")
        };
        let sibling_bytes = if sibling_path.ends_with("vosu.toml") {
            b"SERVER_PORT = 24099.0\n".to_vec()
        } else {
            b"POLL_RATE=150\n".to_vec()
        };
        fs::write(&sibling_path, &sibling_bytes).unwrap();

        let store = PluginSettingsStore::open(&config);
        let err = run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store)
            .expect_err(&format!("case {name} must fail closed"));

        assert!(
            matches!(
                err,
                MigrationError::MalformedInput { .. } | MigrationError::InvalidSetting { .. }
            ),
            "case {name}: unexpected error {err:?}"
        );

        // Diagnostic must be redacted: never leak raw setting values or local temp path.
        let msg = err.to_string();
        assert!(
            !msg.contains(secret_sentinel),
            "case {name}: diagnostic leaked raw value: {msg}"
        );
        assert!(
            !msg.contains(&tmp.path().display().to_string()),
            "case {name}: diagnostic leaked filesystem path: {msg}"
        );

        // Zero destructive writes: no journal, no plugins/settings.json, sources untouched.
        assert!(
            !journal_path(&config).exists(),
            "case {name}: wrote journal"
        );
        assert!(
            !store.path().exists(),
            "case {name}: wrote plugins/settings.json"
        );
        assert_eq!(
            fs::read(&target_path).unwrap(),
            bad_content.as_bytes(),
            "case {name}: mutated failing input"
        );
        assert_eq!(
            fs::read(&sibling_path).unwrap(),
            sibling_bytes,
            "case {name}: mutated sibling input"
        );
    }
}

#[tokio::test]
async fn post_migration_toml_poll_rate_edit_does_not_control_osu_and_is_stripped() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();

    fs::write(
        config.join("vosu.toml"),
        "SERVER_PORT = 24077.0\nPOLL_RATE = 310.0\nCALCULATE_PP = false\n",
    )
    .unwrap();

    let (first_outcome, first_cfg, first_settings) =
        run_release_startup(&config, &data).await.unwrap();
    assert!(matches!(first_outcome, MigrationOutcome::Completed(_)));
    assert_eq!(
        first_settings.load(OSU_PLUGIN_ID).unwrap().get("POLL_RATE"),
        Some(&json!(310.0))
    );
    bounded!(first_cfg.shutdown()).unwrap();

    // User manually re-adds POLL_RATE = 500 and CALCULATE_PP = true into vosu.toml AFTER migration.
    let mut edited_toml = fs::read_to_string(config.join("vosu.toml")).unwrap();
    edited_toml.push_str("\nPOLL_RATE = 500.0\nCALCULATE_PP = true\n");
    fs::write(config.join("vosu.toml"), edited_toml).unwrap();

    let settings_bytes_before = fs::read(first_settings.path()).unwrap();

    // Restart: migration is already complete; ConfigStore::initialize strips the
    // retired/moved keys from vosu.toml without touching plugins/settings.json.
    let (second_outcome, second_cfg, second_settings) =
        run_release_startup(&config, &data).await.unwrap();
    assert!(matches!(
        second_outcome,
        MigrationOutcome::AlreadyComplete(_)
    ));
    assert_eq!(
        fs::read(second_settings.path()).unwrap(),
        settings_bytes_before,
        "editing vosu.toml after cutover must not touch plugins/settings.json"
    );
    let osu = second_settings.load(OSU_PLUGIN_ID).unwrap();
    assert_eq!(osu.get("POLL_RATE"), Some(&json!(310.0)));
    assert_eq!(osu.get("CALCULATE_PP"), Some(&Value::Bool(false)));

    // ConfigStore::initialize rewrites/strips POLL_RATE from vosu.toml on flush/init.
    assert!(second_cfg.update(|c| c.debug_log = true));
    bounded!(second_cfg.flush()).unwrap();
    bounded!(second_cfg.shutdown()).unwrap();

    let final_table = read_toml_table(&config.join("vosu.toml"));
    assert!(!final_table.contains_key("POLL_RATE"));
    assert!(!final_table.contains_key("CALCULATE_PP"));
}
