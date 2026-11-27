//! Core unit tests for normal-root migration: journal validation, config/env
//! precedence, key stripping & preservation, malformed input fail-closed
//! redaction, verbatim catalog sources, and 1.x user-tool disablement.

use std::fs;
use std::path::Path;

use serde_json::{Map, Value, json};
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_sys::platform::BuildChannel;

use crate::catalog::sources::{OFFICIAL_SOURCES, SourceLifecycle, SourceStore, SourceStoreError};
use crate::config::PRE_RENAME_ENV_FILENAME;
use crate::plugin::settings::{PluginSettingsStore, SettingsStorage};

use super::*;

pub(super) fn real_schemas() -> MigrationSchemas {
    let osu = SettingsSchema::parse(
        r#"[
            {"uniqueID":"CALCULATE_PP","type":"checkbox","title":"Calculate pp","value":true},
            {"uniqueID":"POLL_RATE","type":"number","title":"Poll rate","value":150.0},
            {"uniqueID":"PRECISE_DATA_POLL_RATE","type":"number","title":"Precise data poll rate","value":10.0},
            {"uniqueID":"SHOW_MP_COMMANDS","type":"checkbox","title":"Show multiplayer commands","value":false},
            {"uniqueID":"READ_MANIA_SCROLL_SPEED","type":"checkbox","title":"Read mania scroll speed","value":true}
        ]"#,
    )
    .expect("valid osu schema");
    let overlay_host = SettingsSchema::parse(
        r#"[
            {"uniqueID":"static_folder_path","type":"text","title":"Overlay folder","value":""}
        ]"#,
    )
    .expect("valid overlayhost schema");
    MigrationSchemas { osu, overlay_host }
}

pub(super) struct NoopLifecycle;

impl SourceLifecycle for NoopLifecycle {
    fn reconcile_sources(&self, _identities: &[String]) -> Result<(), SourceStoreError> {
        Ok(())
    }
}

pub(super) fn write_legacy_user_tool(data_root: &Path, config_root: &Path, id: &str) {
    let tool_dir = data_root.join("tools").join(id);
    fs::create_dir_all(&tool_dir).unwrap();
    fs::write(
        tool_dir.join("tool.toml"),
        format!("id = \"{id}\"\n[service]\ncommand = \"./run\"\n"),
    )
    .unwrap();

    let tool_data = data_root.join("tool-data").join(id);
    fs::create_dir_all(&tool_data).unwrap();
    fs::write(tool_data.join("durable.db"), b"keep-1x-user-data").unwrap();

    let settings_dir = config_root.join("tools").join("settings");
    fs::create_dir_all(&settings_dir).unwrap();
    fs::write(
        settings_dir.join(format!("{id}.json")),
        r#"{"custom_key":"keep-1x-settings"}"#,
    )
    .unwrap();
}

#[test]
fn journal_fails_closed_on_corrupt_oversized_unknown_version_and_inconsistent_state() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();
    let schemas = real_schemas();
    let store = PluginSettingsStore::open(&config);

    // 1. Corrupt JSON must fail closed and never be overwritten.
    let jpath = journal_path(&config);
    fs::write(&jpath, b"{not-valid-json").unwrap();
    let err =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap_err();
    assert_eq!(err, MigrationError::JournalCorrupt);
    assert_eq!(fs::read(&jpath).unwrap(), b"{not-valid-json");

    // 2. Unknown field rejected (`deny_unknown_fields`).
    fs::write(
        &jpath,
        json!({
            "version": 1,
            "generation": 1,
            "unexpected": true
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        load_journal(&config).unwrap_err(),
        MigrationError::JournalCorrupt
    );

    // 3. Unsupported version fails closed without overwriting.
    let mut valid_journal = MigrationJournal::new(
        MigrationSnapshot {
            fingerprints: Default::default(),
            osu_overrides: Map::new(),
            overlay_host_overrides: Map::new(),
            core_config: CoreConfigSnapshot {
                canonical_toml: None,
                stripped_keys: Vec::new(),
                legacy_env_files: Vec::new(),
            },
            catalog_sources: None,
            user_tools_to_disable: Vec::new(),
            discovered_official_tools: Vec::new(),
            discovered_official_overlays: Vec::new(),
        },
        Vec::new(),
    );
    valid_journal.version = 99;
    valid_journal.generation = 1;
    let v99_bytes = serde_json::to_vec(&valid_journal).unwrap();
    fs::write(&jpath, &v99_bytes).unwrap();
    assert_eq!(
        load_journal(&config).unwrap_err(),
        MigrationError::JournalUnsupportedVersion(99)
    );
    assert_eq!(fs::read(&jpath).unwrap(), v99_bytes);

    // 4. Inconsistent `local_conversion_complete: true` with pending steps fails closed.
    valid_journal.version = JOURNAL_VERSION;
    valid_journal.local_conversion_complete = true;
    fs::write(&jpath, serde_json::to_vec(&valid_journal).unwrap()).unwrap();
    assert_eq!(
        load_journal(&config).unwrap_err(),
        MigrationError::JournalCorrupt
    );

    // 5. Oversized journal fails closed.
    fs::write(&jpath, vec![b' '; (MAX_JOURNAL_BYTES + 1) as usize]).unwrap();
    assert!(matches!(
        load_journal(&config).unwrap_err(),
        MigrationError::InputOversized {
            label: JOURNAL_FILENAME,
            ..
        }
    ));
}

#[test]
fn config_precedence_clamping_stripping_and_preservation() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(&data).unwrap();

    // Lowest precedence: pre-rename `.env`
    fs::write(
        config.join(PRE_RENAME_ENV_FILENAME),
        "\
CALCULATE_PP=false
POLL_RATE=50
SHOW_MP_COMMANDS=true
STATIC_FOLDER_PATH=\"\"
SERVER_IP=legacy-host
ENABLE_KEY_OVERLAY=true
",
    )
    .unwrap();

    // Middle precedence: `vosu.env`
    fs::write(
        config.join("vosu.env"),
        "\
POLL_RATE=0x100
PRECISE_DATA_POLL_RATE=0
READ_MANIA_SCROLL_SPEED=false
API_V1_SUBSCRIPTION_DEBOUNCE=7
V2_SUBSCRIPTION_DEBOUNCE=99
",
    )
    .unwrap();

    // Highest precedence: `vosu.toml`, including unrelated unknown keys and
    // nested tables with coincidentally matching leaf names.
    fs::write(
        config.join("vosu.toml"),
        "\
SERVER_PORT = 24099
POLL_RATE = 275
ENABLE_MANIA_KEY_HIT_ERRORS = true
ENABLE_HIT_OFFSETS = false
CUSTOM_TOP = \"keep-me\"

[custom]
POLL_RATE = 999
STATIC_FOLDER_PATH = \"/nested/keep\"
ENABLE_KEY_OVERLAY = true
",
    )
    .unwrap();

    // Pre-existing valid per-plugin override for `SHOW_MP_COMMANDS` must WIN over legacy.
    let store = PluginSettingsStore::open(&config);
    store
        .store(
            OSU_PLUGIN_ID,
            &Map::from_iter([("SHOW_MP_COMMANDS".to_owned(), Value::Bool(false))]),
        )
        .unwrap();

    let schemas = real_schemas();
    let outcome =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap();
    let MigrationOutcome::Completed(journal) = outcome else {
        panic!("expected Completed");
    };
    assert!(journal.local_conversion_complete);
    assert!(journal.generation >= 6);

    // Check `vosu.osu` overrides:
    // - SHOW_MP_COMMANDS preserved existing `false` (over legacy `true`)
    // - CALCULATE_PP imported explicit `false` from pre-rename env
    // - READ_MANIA_SCROLL_SPEED imported explicit `false` from vosu.env
    // - POLL_RATE imported `275.0` from vosu.toml (beat vosu.env 256 & pre-rename 50)
    // - PRECISE_DATA_POLL_RATE imported clamped `1.0` from vosu.env `0`
    let osu_stored = store.load(OSU_PLUGIN_ID).unwrap();
    assert_eq!(osu_stored.get("CALCULATE_PP"), Some(&Value::Bool(false)));
    assert_eq!(
        osu_stored.get("READ_MANIA_SCROLL_SPEED"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        osu_stored.get("SHOW_MP_COMMANDS"),
        Some(&Value::Bool(false))
    );
    assert_eq!(osu_stored.get("POLL_RATE"), Some(&json!(275.0)));
    assert_eq!(osu_stored.get("PRECISE_DATA_POLL_RATE"), Some(&json!(1.0)));

    let ps_receipt = journal.plugin_settings.receipt().unwrap();
    assert_eq!(ps_receipt.osu_preserved_keys, vec!["SHOW_MP_COMMANDS"]);
    assert_eq!(
        ps_receipt.osu_imported_keys,
        vec![
            "CALCULATE_PP",
            "POLL_RATE",
            "PRECISE_DATA_POLL_RATE",
            "READ_MANIA_SCROLL_SPEED",
        ]
    );

    // Check `vosu.overlayhost` preserved explicit empty string `""` from pre-rename env.
    let overlay_stored = store.load(OVERLAY_HOST_PLUGIN_ID).unwrap();
    assert_eq!(
        overlay_stored.get("static_folder_path"),
        Some(&Value::String(String::new()))
    );
    assert_eq!(
        ps_receipt.overlay_host_imported_keys,
        vec!["static_folder_path"]
    );

    // Check canonical `vosu.toml`:
    let toml_text = fs::read_to_string(config.join("vosu.toml")).unwrap();
    let parsed: toml::Table = toml::from_str(&toml_text).unwrap();
    for stripped in OSU_MOVED_KEYS
        .iter()
        .copied()
        .chain(RETIRED_KEYS.iter().copied())
        .chain(std::iter::once(OVERLAY_HOST_LEGACY_KEY))
    {
        assert!(
            !parsed.contains_key(stripped),
            "top-level key {stripped} must be stripped"
        );
    }
    assert_eq!(parsed["API_V1_SUBSCRIPTION_DEBOUNCE"].as_float(), Some(7.0));
    assert_eq!(parsed["SERVER_PORT"].as_float(), Some(24_099.0));
    assert_eq!(parsed["SERVER_IP"].as_str(), Some("legacy-host"));
    assert_eq!(parsed["CUSTOM_TOP"].as_str(), Some("keep-me"));
    // Nested table with matching leaf names MUST be preserved verbatim.
    let nested = parsed["custom"].as_table().unwrap();
    assert_eq!(nested["POLL_RATE"].as_integer(), Some(999));
    assert_eq!(nested["STATIC_FOLDER_PATH"].as_str(), Some("/nested/keep"));
    assert_eq!(nested["ENABLE_KEY_OVERLAY"].as_bool(), Some(true));

    // Legacy env files removed only after canonical `vosu.toml` is durable.
    assert!(!config.join("vosu.env").exists());
    assert!(!config.join(PRE_RENAME_ENV_FILENAME).exists());
}

#[test]
fn malformed_toml_env_or_invalid_setting_fails_closed_with_redacted_diagnostic() {
    let schemas = real_schemas();
    let secret = "super-secret-value-9f8e7d";

    // Case 1: malformed `vosu.toml`
    {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        fs::create_dir_all(&config).unwrap();
        let bad_toml = format!("POLL_RATE = [ \"{secret}\"");
        fs::write(config.join("vosu.toml"), &bad_toml).unwrap();
        fs::write(config.join("vosu.env"), "CALCULATE_PP=false\n").unwrap();
        let store = PluginSettingsStore::open(&config);

        let err = run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store)
            .unwrap_err();
        let msg = err.to_string();
        assert_eq!(err, MigrationError::MalformedInput { label: "vosu.toml" });
        assert!(!msg.contains(secret));
        assert!(!msg.contains(&config.display().to_string()));
        assert!(!journal_path(&config).exists());
        assert!(!store.path().exists());
        assert!(config.join("vosu.env").exists());
        assert_eq!(
            fs::read_to_string(config.join("vosu.toml")).unwrap(),
            bad_toml
        );
    }

    // Case 2: malformed `vosu.env` (unterminated quote / bad line)
    {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("vosu.env"),
            format!("STATIC_FOLDER_PATH=\"{secret}\n"),
        )
        .unwrap();
        let store = PluginSettingsStore::open(&config);

        let err = run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store)
            .unwrap_err();
        assert_eq!(err, MigrationError::MalformedInput { label: "vosu.env" });
        assert!(!err.to_string().contains(secret));
        assert!(!journal_path(&config).exists());
        assert!(config.join("vosu.env").exists());
    }

    // Case 3: invalid moved setting value (wrong type in TOML)
    {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("vosu.toml"),
            format!("POLL_RATE = \"{secret}\"\n"),
        )
        .unwrap();
        let store = PluginSettingsStore::open(&config);

        let err = run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store)
            .unwrap_err();
        assert_eq!(err, MigrationError::InvalidSetting { key: "POLL_RATE" });
        assert!(!err.to_string().contains(secret));
        assert!(!journal_path(&config).exists());
        assert!(!store.path().exists());
    }
}

#[test]
fn sources_migrate_verbatim_with_exact_official_only_trust_and_absent_does_not_clobber() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(config.join("tools")).unwrap();
    let schemas = real_schemas();
    let store = PluginSettingsStore::open(&config);

    let legacy_sources = json!([
        {"source": "community/custom-pack", "enabled": true},
        {"source": "18-lab-dev/vosu-tools", "enabled": true},
        {"source": "18-lab-dev/vosu-tools@v2.0.0", "enabled": true},
        {"source": "https://github.com/18-lab-dev/vosu-tools", "enabled": true},
        {"source": "18-lab-dev/vosulay", "enabled": false},
        {"source": "18-lab-dev/vosu-overlays", "enabled": true},
        {"source": "https://mirror.example/18-lab-dev/vosu-tools/catalog-v2.json", "enabled": true}
    ]);
    fs::write(
        config.join("tools").join("catalog-sources.json"),
        serde_json::to_vec_pretty(&legacy_sources).unwrap(),
    )
    .unwrap();

    let MigrationOutcome::Completed(journal) =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
    else {
        panic!("expected Completed");
    };
    let receipt = journal.catalog_sources.receipt().unwrap();
    assert!(receipt.migrated);
    assert_eq!(receipt.source_count, 7);
    assert_eq!(receipt.trusted_official_count, 3);

    // Verify `SourceStore` reads exact verbatim order, enabled flags, and trust.
    let source_store = SourceStore::open(&config, std::sync::Arc::new(NoopLifecycle)).unwrap();
    let (_, records) = source_store.snapshot().unwrap();
    assert_eq!(records.len(), 7);
    let expected = [
        ("community/custom-pack", true, false),
        ("18-lab-dev/vosu-tools", true, true),
        ("18-lab-dev/vosu-tools@v2.0.0", true, false),
        ("https://github.com/18-lab-dev/vosu-tools", true, false),
        ("18-lab-dev/vosulay", false, true),
        ("18-lab-dev/vosu-overlays", true, true),
        (
            "https://mirror.example/18-lab-dev/vosu-tools/catalog-v2.json",
            true,
            false,
        ),
    ];
    for (index, (id, enabled, trusted)) in expected.iter().enumerate() {
        assert_eq!(records[index].order, index);
        assert_eq!(records[index].identity.as_str(), *id);
        assert_eq!(records[index].enabled, *enabled);
        assert_eq!(records[index].trusted, *trusted, "trust mismatch for {id}");
        assert!(!records[index].private_network);
    }
    assert!(!config.join("catalog-credentials.toml").exists());

    // Absent `catalog-sources.json` must NOT clobber existing `catalog-source-settings.json`.
    let tmp2 = tempfile::tempdir().unwrap();
    let cfg2 = tmp2.path().join("config");
    let data2 = tmp2.path().join("data");
    fs::create_dir_all(&cfg2).unwrap();
    let existing_store = SourceStore::open(&cfg2, std::sync::Arc::new(NoopLifecycle)).unwrap();
    existing_store
        .replace(&[
            crate::catalog::sources::SourceInput {
                source: OFFICIAL_SOURCES[0].to_owned(),
                enabled: true,
            },
            crate::catalog::sources::SourceInput {
                source: "keep/me".to_owned(),
                enabled: false,
            },
        ])
        .unwrap();
    let before = fs::read(cfg2.join("catalog-source-settings.json")).unwrap();
    let store2 = PluginSettingsStore::open(&cfg2);
    let MigrationOutcome::Completed(journal2) =
        run_local_migration(&cfg2, &data2, BuildChannel::Release, &schemas, &store2).unwrap()
    else {
        panic!("expected Completed");
    };
    assert!(!journal2.catalog_sources.receipt().unwrap().migrated);
    assert_eq!(
        fs::read(cfg2.join("catalog-source-settings.json")).unwrap(),
        before
    );
}

#[test]
fn user_tools_are_disabled_while_preserving_data_settings_and_leaving_user_overlays_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(config.join("tools")).unwrap();
    fs::create_dir_all(data.join("tools")).unwrap();

    // User 1.x tool (tool.toml only, not official)
    write_legacy_user_tool(&data, &config, "my-custom-tool");
    // Already-v2 tool (manifest.toml) must NOT be disabled
    let v2_tool = data.join("tools").join("modern-tool");
    fs::create_dir_all(&v2_tool).unwrap();
    fs::write(v2_tool.join("manifest.toml"), "manifest_version = 1\n").unwrap();
    // Official 1.x tool (tool.toml + official state.json) must NOT be disabled as user tool
    let official_dir = data.join("tools").join("map-analysis");
    fs::create_dir_all(&official_dir).unwrap();
    fs::write(official_dir.join("tool.toml"), "id = \"map-analysis\"\n").unwrap();

    // User overlay directory (completely outside tools) + non-official overlay entry
    let overlay_dir = data.join("static").join("my-user-overlay");
    fs::create_dir_all(&overlay_dir).unwrap();
    fs::write(overlay_dir.join("index.html"), "<html>overlay</html>").unwrap();

    fs::write(
        config.join("tools").join("state.json"),
        serde_json::to_vec_pretty(&json!({
            "my-custom-tool": { "enabled": true, "trusted": true, "official": false },
            "modern-tool": { "enabled": true, "trusted": true, "official": false },
            "map-analysis": {
                "enabled": true,
                "trusted": true,
                "official": true,
                "catalog_source": "18-lab-dev/vosu-tools"
            }
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        config.join("tools").join("overlay-catalog-state.json"),
        serde_json::to_vec_pretty(&json!({
            "pp-counter": { "source": "18-lab-dev/vosu-overlays", "version": "0.4.0" },
            "my-user-overlay": { "source": "community/overlays", "version": "1.0.0" }
        }))
        .unwrap(),
    )
    .unwrap();

    let schemas = real_schemas();
    let store = PluginSettingsStore::open(&config);
    let MigrationOutcome::Completed(journal) =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
    else {
        panic!("expected Completed");
    };

    assert_eq!(
        journal.user_tools.receipt().unwrap().disabled_user_tools,
        vec!["my-custom-tool"]
    );

    let saved: Value =
        serde_json::from_slice(&fs::read(config.join("tools").join("state.json")).unwrap())
            .unwrap();
    assert_eq!(saved["my-custom-tool"]["enabled"], json!(false));
    assert_eq!(saved["my-custom-tool"]["trusted"], json!(true));
    assert_eq!(saved["modern-tool"]["enabled"], json!(true));
    assert_eq!(saved["map-analysis"]["enabled"], json!(true));

    // User tool durable data & settings untouched.
    assert_eq!(
        fs::read(data.join("tool-data/my-custom-tool/durable.db")).unwrap(),
        b"keep-1x-user-data"
    );
    assert_eq!(
        fs::read_to_string(config.join("tools/settings/my-custom-tool.json")).unwrap(),
        r#"{"custom_key":"keep-1x-settings"}"#
    );
    // User overlay untouched and NOT queued as an official replacement intent.
    assert_eq!(
        fs::read_to_string(overlay_dir.join("index.html")).unwrap(),
        "<html>overlay</html>"
    );
    assert!(
        !journal
            .pending_intents
            .iter()
            .any(|intent| intent.legacy_id == "my-user-overlay")
    );
    // Official intents recorded for `map-analysis` (tool + AF.2) and `pp-counter` (overlay).
    let ids: Vec<&str> = journal
        .pending_intents
        .iter()
        .map(|intent| intent.intent_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec![
            "af2:map-analysis:vosu.tools.map-analysis",
            "overlay:18-lab-dev/vosu-overlays:pp-counter",
            "tool:18-lab-dev/vosu-tools:map-analysis",
        ]
    );
}

#[test]
fn nested_matching_leaf_names_alone_do_not_extract_or_rewrite_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(&config).unwrap();

    let original =
        "SERVER_PORT = 24050.0\n\n[custom]\nPOLL_RATE = 999\nSTATIC_FOLDER_PATH = \"/nested\"\n";
    fs::write(config.join("vosu.toml"), original).unwrap();

    let schemas = real_schemas();
    let store = PluginSettingsStore::open(&config);
    let MigrationOutcome::Completed(journal) =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
    else {
        panic!("expected Completed");
    };
    assert!(!journal.core_config.receipt().unwrap().wrote_toml);
    assert_eq!(
        fs::read_to_string(config.join("vosu.toml")).unwrap(),
        original
    );
    assert!(store.load(OSU_PLUGIN_ID).unwrap().is_empty());
    assert!(store.load(OVERLAY_HOST_PLUGIN_ID).unwrap().is_empty());
}

#[test]
fn root_catalog_sources_fallback_and_malformed_sources_fail_closed() {
    let schemas = real_schemas();

    // Direct `<config>/catalog-sources.json` works when `tools/catalog-sources.json` is absent.
    {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        fs::create_dir_all(&config).unwrap();
        fs::write(
            config.join("catalog-sources.json"),
            r#"[{"source":"18-lab-dev/vosulay","enabled":true},{"source":"author/pack","enabled":false}]"#,
        )
        .unwrap();
        let store = PluginSettingsStore::open(&config);
        let MigrationOutcome::Completed(journal) =
            run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
        else {
            panic!("expected Completed");
        };
        assert_eq!(journal.catalog_sources.receipt().unwrap().source_count, 2);
        assert_eq!(
            journal
                .catalog_sources
                .receipt()
                .unwrap()
                .trusted_official_count,
            1
        );
    }

    // Malformed `catalog-sources.json` fails closed before ANY destructive write.
    {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("vosu.env"), "POLL_RATE=200\n").unwrap();
        fs::write(config.join("catalog-sources.json"), b"{bad-json").unwrap();
        let store = PluginSettingsStore::open(&config);
        let err = run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store)
            .unwrap_err();
        assert_eq!(
            err,
            MigrationError::MalformedInput {
                label: "catalog-sources.json"
            }
        );
        assert!(!journal_path(&config).exists());
        assert!(!store.path().exists());
        assert!(config.join("vosu.env").exists());
    }
}
