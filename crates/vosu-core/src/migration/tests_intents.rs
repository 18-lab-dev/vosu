//! Offline official replacement intent resolution (0 / 1 / >1 / lookup error)
//! and AF.2 closed cross-kind copy-only data/settings transfer & collision tests.

use std::fs;

use serde_json::{Map, Value, json};
use vosu_plugin_api::manifest::PackageKind;
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_sys::platform::BuildChannel;

use crate::plugin::settings::{PluginSettingsStore, SettingsStorage};

use super::tests::real_schemas;
use super::*;

#[test]
fn offline_resolve_intents_handles_zero_one_multiple_and_lookup_error_without_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(config.join("tools")).unwrap();

    fs::write(
        config.join("tools/state.json"),
        serde_json::to_vec_pretty(&json!({
            "map-analysis": {"official": true, "catalog_source": "18-lab-dev/vosu-tools"},
            "practice": {"official": true, "catalog_source": "18-lab-dev/vosu-tools"},
            "custom-official": {"official": true, "catalog_source": "18-lab-dev/vosu-tools"},
            "vosulay": {"official": true, "catalog_source": "18-lab-dev/vosulay"}
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        config.join("tools/overlay-catalog-state.json"),
        serde_json::to_vec_pretty(&json!({
            "pp-counter": {"source": "18-lab-dev/vosu-overlays", "version": "0.4.0"},
            "unported-overlay": {"source": "18-lab-dev/vosu-overlays", "version": "0.4.0"}
        }))
        .unwrap(),
    )
    .unwrap();

    let schemas = real_schemas();
    let store = PluginSettingsStore::open(&config);
    let MigrationOutcome::Completed(mut journal) =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
    else {
        panic!("expected Completed");
    };

    // Provide validated catalog entries:
    // - `18-lab-dev/vosu-tools`:
    //   - `map-analysis` has 1 matching tool (`map-analysis-v2`) + plugin (`vosu.tools.map-analysis`)
    //   - `practice` has 2 conflicting tools (`practice-v2`, `practice-alt`)
    //   - `custom-official` has 0 tool claims (only a plugin claiming `replaces`, wrong kind!)
    // - `18-lab-dev/vosulay`: errored lookup -> MUST stay RetryableError (no fallback to vosu-tools!)
    // - `18-lab-dev/vosu-overlays`:
    //   - `pp-counter` has 1 matching overlay (`pp-counter-v2`)
    //   - `unported-overlay` has 0 claims -> Unmanaged
    let lookups = vec![
        CatalogSourceLookup {
            source: "18-lab-dev/vosu-tools".to_owned(),
            entries: Ok(vec![
                ValidatedCatalogEntry {
                    id: "map-analysis-v2".to_owned(),
                    kind: PackageKind::Tool,
                    replaces: vec!["map-analysis".to_owned()],
                },
                ValidatedCatalogEntry {
                    id: "vosu.tools.map-analysis".to_owned(),
                    kind: PackageKind::Plugin,
                    replaces: Vec::new(),
                },
                ValidatedCatalogEntry {
                    id: "practice-v2".to_owned(),
                    kind: PackageKind::Tool,
                    replaces: vec!["practice".to_owned()],
                },
                ValidatedCatalogEntry {
                    id: "practice-alt".to_owned(),
                    kind: PackageKind::Tool,
                    replaces: vec!["practice".to_owned()],
                },
                ValidatedCatalogEntry {
                    id: "vosu.tools.practice".to_owned(),
                    kind: PackageKind::Plugin,
                    replaces: Vec::new(),
                },
                // Wrong kind (Plugin claiming a 1.x tool id) must NOT match tool replacement!
                ValidatedCatalogEntry {
                    id: "vosu.tools.custom".to_owned(),
                    kind: PackageKind::Plugin,
                    replaces: vec!["custom-official".to_owned()],
                },
                // Even if vosu-tools mentions vosulay, it MUST NOT satisfy 18-lab-dev/vosulay!
                ValidatedCatalogEntry {
                    id: "wrong-source-vosulay".to_owned(),
                    kind: PackageKind::Tool,
                    replaces: vec!["vosulay".to_owned()],
                },
            ]),
        },
        CatalogSourceLookup {
            source: "18-lab-dev/vosulay".to_owned(),
            entries: Err("network timeout".to_owned()),
        },
        CatalogSourceLookup {
            source: "18-lab-dev/vosu-overlays".to_owned(),
            entries: Ok(vec![ValidatedCatalogEntry {
                id: "pp-counter-v2".to_owned(),
                kind: PackageKind::Overlay,
                replaces: vec!["pp-counter".to_owned()],
            }]),
        },
    ];

    let summary = resolve_intents(&mut journal, &lookups);
    assert_eq!(summary.conflicts, 1);
    assert_eq!(summary.unmanaged, 2);
    assert_eq!(summary.retryable_errors, 2); // vosulay tool + vosulay af2
    assert_eq!(summary.pending_resolved, 4); // map-analysis tool+af2, practice af2, pp-counter overlay

    let by_id = |id: &str| {
        journal
            .pending_intents
            .iter()
            .find(|intent| intent.intent_id == id)
            .unwrap()
    };

    // 1 claim -> Pending with resolved_target_id
    let map_tool = by_id("tool:18-lab-dev/vosu-tools:map-analysis");
    assert_eq!(map_tool.status, IntentStatus::Pending);
    assert_eq!(
        map_tool.resolved_target_id.as_deref(),
        Some("map-analysis-v2")
    );

    // >1 claims -> Conflict with sorted candidates (never arbitrary first pick)
    let practice_tool = by_id("tool:18-lab-dev/vosu-tools:practice");
    assert_eq!(
        practice_tool.status,
        IntentStatus::Conflict {
            candidates: vec!["practice-alt".to_owned(), "practice-v2".to_owned()],
        }
    );
    assert_eq!(practice_tool.resolved_target_id, None);

    // 0 same-kind claims -> Unmanaged
    let custom_tool = by_id("tool:18-lab-dev/vosu-tools:custom-official");
    assert_eq!(custom_tool.status, IntentStatus::Unmanaged);
    assert_eq!(custom_tool.resolved_target_id, None);

    let unported_overlay = by_id("overlay:18-lab-dev/vosu-overlays:unported-overlay");
    assert_eq!(unported_overlay.status, IntentStatus::Unmanaged);

    let pp_overlay = by_id("overlay:18-lab-dev/vosu-overlays:pp-counter");
    assert_eq!(pp_overlay.status, IntentStatus::Pending);
    assert_eq!(
        pp_overlay.resolved_target_id.as_deref(),
        Some("pp-counter-v2")
    );

    // Errored source -> RetryableError, never falling back to vosu-tools
    let vosulay_tool = by_id("tool:18-lab-dev/vosulay:vosulay");
    assert!(matches!(
        vosulay_tool.status,
        IntentStatus::RetryableError { .. }
    ));
    assert_eq!(vosulay_tool.resolved_target_id, None);

    // Queueing NEVER creates `catalog-installed.json`.
    assert!(!data.join("catalog/catalog-installed.json").exists());
}

#[test]
fn af2_closed_map_covers_all_nine_official_rows() {
    assert_eq!(AF2_TRANSFER_MAP.len(), 9);
    assert!(
        af2_row_for("lazer-library", "18-lab-dev/vosu-tools")
            .unwrap()
            .target_web_tool
            .is_none()
    );
    assert_eq!(
        af2_row_for("replay-renderer", "18-lab-dev/vosu-tools")
            .unwrap()
            .plugin_settings_keys
            .len(),
        22
    );
    assert_eq!(
        af2_row_for("downloader", "18-lab-dev/vosu-tools")
            .unwrap()
            .secret_plugin_keys,
        &["client_secret"]
    );
    assert_eq!(
        af2_row_for("vosulay", "18-lab-dev/vosulay")
            .unwrap()
            .target_plugin,
        Some("vosu.vosulay")
    );
    assert!(af2_row_for("vosulay", "18-lab-dev/vosu-tools").is_none());
    assert!(af2_row_for("unlisted-tool", "18-lab-dev/vosu-tools").is_none());
}

#[test]
fn af2_transfer_is_copy_only_splits_secrets_and_never_reruns_after_completed() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(config.join("tools/settings")).unwrap();
    fs::create_dir_all(data.join("tool-data/downloader/nested")).unwrap();

    fs::write(
        config.join("tools/state.json"),
        serde_json::to_vec_pretty(&json!({
            "downloader": {"official": true, "catalog_source": "18-lab-dev/vosu-tools"}
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        data.join("tool-data/downloader/nested/queue.db"),
        b"1x-queue-bytes",
    )
    .unwrap();
    let secret_value = "oauth-secret-42-do-not-leak";
    fs::write(
        config.join("tools/settings/downloader.json"),
        serde_json::to_vec_pretty(&json!({
            "concurrency": 4,
            "client_id": "12345",
            "client_secret": secret_value,
            "hide_owned": true,
            "unlisted_legacy_key": "must-not-migrate"
        }))
        .unwrap(),
    )
    .unwrap();

    let schemas = real_schemas();
    let store = PluginSettingsStore::open(&config);
    let MigrationOutcome::Completed(mut journal) =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
    else {
        panic!("expected Completed");
    };

    let downloader_schema = SettingsSchema::parse(
        r#"[
            {"uniqueID":"concurrency","type":"number","title":"Concurrency","value":2},
            {"uniqueID":"client_id","type":"text","title":"Client ID","value":""},
            {"uniqueID":"client_secret","type":"password","title":"Client secret","value":""}
        ]"#,
    )
    .unwrap();

    let intent_id = "af2:downloader:vosu.tools.downloader";
    let outcome = execute_af2_transfer(
        &config,
        &data,
        &mut journal,
        intent_id,
        &store,
        Some(&downloader_schema),
    )
    .unwrap();

    assert_eq!(
        outcome,
        Af2TransferOutcome::Completed {
            copied_data_files: 1,
            imported_plugin_keys: vec![
                "client_id".to_owned(),
                "client_secret".to_owned(),
                "concurrency".to_owned(),
            ],
            imported_web_tool_keys: vec!["hide_owned".to_owned()],
        }
    );

    // 1.x source files remain completely intact (copy-only, never moved/deleted).
    assert_eq!(
        fs::read(data.join("tool-data/downloader/nested/queue.db")).unwrap(),
        b"1x-queue-bytes"
    );
    assert!(config.join("tools/settings/downloader.json").exists());

    // Destination plugin data copied verbatim.
    assert_eq!(
        fs::read(data.join("plugins/vosu.tools.downloader/nested/queue.db")).unwrap(),
        b"1x-queue-bytes"
    );

    // Secret went ONLY into plugin settings, NEVER into web tool settings; unlisted dropped.
    let plugin_vals = store.load("vosu.tools.downloader").unwrap();
    assert_eq!(plugin_vals["client_secret"], json!(secret_value));
    assert_eq!(plugin_vals["concurrency"], json!(4));
    assert!(!plugin_vals.contains_key("unlisted_legacy_key"));
    assert!(!plugin_vals.contains_key("hide_owned"));

    let web_vals: Map<String, Value> = serde_json::from_slice(
        &fs::read(config.join("tools/settings/downloader-v2.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(web_vals.get("hide_owned"), Some(&json!(true)));
    assert!(!web_vals.contains_key("client_secret"));
    assert!(!web_vals.contains_key("unlisted_legacy_key"));

    // Calling again after Completed MUST be a strict no-op (never re-runs).
    let gen_after = journal.generation;
    let second = execute_af2_transfer(
        &config,
        &data,
        &mut journal,
        intent_id,
        &store,
        Some(&downloader_schema),
    )
    .unwrap();
    assert_eq!(second, Af2TransferOutcome::AlreadyCompleted);
    assert_eq!(journal.generation, gen_after);
}

#[test]
fn af2_transfer_detects_data_and_settings_collisions_and_leaves_both_sides_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(config.join("tools/settings")).unwrap();
    fs::create_dir_all(data.join("tool-data/lazer-library")).unwrap();
    fs::create_dir_all(data.join("plugins/vosu.tools.lazer-library")).unwrap();

    fs::write(
        config.join("tools/state.json"),
        serde_json::to_vec_pretty(&json!({
            "lazer-library": {"official": true, "catalog_source": "18-lab-dev/vosu-tools"}
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(data.join("tool-data/lazer-library/old.dat"), b"1x-bytes").unwrap();
    fs::write(
        data.join("plugins/vosu.tools.lazer-library/existing.dat"),
        b"2x-existing",
    )
    .unwrap();
    fs::write(
        config.join("tools/settings/lazer-library.json"),
        r#"{"lazer_data_dir":"/old/lazer"}"#,
    )
    .unwrap();

    let schemas = real_schemas();
    let store = PluginSettingsStore::open(&config);
    let MigrationOutcome::Completed(mut journal) =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
    else {
        panic!("expected Completed");
    };

    let intent_id = "af2:lazer-library:vosu.tools.lazer-library";
    let outcome =
        execute_af2_transfer(&config, &data, &mut journal, intent_id, &store, None).unwrap();
    assert_eq!(
        outcome,
        Af2TransferOutcome::Collision {
            path_hint: "plugins/vosu.tools.lazer-library".to_owned(),
        }
    );

    // Both sides untouched; no partial settings import occurred.
    assert_eq!(
        fs::read(data.join("plugins/vosu.tools.lazer-library/existing.dat")).unwrap(),
        b"2x-existing"
    );
    assert!(
        !data
            .join("plugins/vosu.tools.lazer-library/old.dat")
            .exists()
    );
    assert_eq!(
        fs::read(data.join("tool-data/lazer-library/old.dat")).unwrap(),
        b"1x-bytes"
    );
    assert!(store.load("vosu.tools.lazer-library").unwrap().is_empty());

    // Persisted in journal as `Collision`.
    let reloaded = load_journal(&config).unwrap().unwrap();
    assert_eq!(
        reloaded.pending_intents[0].status,
        IntentStatus::Collision {
            path_hint: "plugins/vosu.tools.lazer-library".to_owned(),
        }
    );

    // Settings-level collision (when plugin data dir is clean, but 2.0 plugin setting exists).
    fs::remove_dir_all(data.join("plugins/vosu.tools.lazer-library")).unwrap();
    journal.pending_intents[0].status = IntentStatus::Pending;
    store
        .store(
            "vosu.tools.lazer-library",
            &Map::from_iter([("lazer_data_dir".to_owned(), json!("/2x/lazer"))]),
        )
        .unwrap();
    let settings_collision =
        execute_af2_transfer(&config, &data, &mut journal, intent_id, &store, None).unwrap();
    assert_eq!(
        settings_collision,
        Af2TransferOutcome::Collision {
            path_hint: "plugins/settings.json:vosu.tools.lazer-library".to_owned(),
        }
    );
    assert!(
        !data
            .join("plugins/vosu.tools.lazer-library/old.dat")
            .exists()
    );
    assert_eq!(
        store.load("vosu.tools.lazer-library").unwrap()["lazer_data_dir"],
        json!("/2x/lazer")
    );
}

#[test]
fn vosulay_af2_copies_config_json_without_promoting_backend_setting() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(config.join("tools")).unwrap();
    fs::create_dir_all(data.join("tool-data/vosulay")).unwrap();

    fs::write(
        config.join("tools/state.json"),
        serde_json::to_vec_pretty(&json!({
            "vosulay": {"official": true, "catalog_source": "18-lab-dev/vosulay"}
        }))
        .unwrap(),
    )
    .unwrap();
    let config_json = r#"{"settings":{"vosu_url":"http://127.0.0.1:24050"}}"#;
    fs::write(data.join("tool-data/vosulay/config.json"), config_json).unwrap();

    let schemas = real_schemas();
    let store = PluginSettingsStore::open(&config);
    let MigrationOutcome::Completed(mut journal) =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
    else {
        panic!("expected Completed");
    };

    let outcome = execute_af2_transfer(
        &config,
        &data,
        &mut journal,
        "af2:vosulay:vosu.vosulay",
        &store,
        None,
    )
    .unwrap();
    assert_eq!(
        outcome,
        Af2TransferOutcome::Completed {
            copied_data_files: 1,
            imported_plugin_keys: Vec::new(),
            imported_web_tool_keys: Vec::new(),
        }
    );
    assert_eq!(
        fs::read_to_string(data.join("plugins/vosu.vosulay/config.json")).unwrap(),
        config_json
    );
    assert_eq!(
        fs::read_to_string(data.join("tool-data/vosulay/config.json")).unwrap(),
        config_json
    );
    assert!(store.load("vosu.vosulay").unwrap().is_empty());
}
