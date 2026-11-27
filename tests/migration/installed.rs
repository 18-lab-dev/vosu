//! Installed 1.x user tools, 1.x user overlays, official replacement intent
//! resolution, and AF.2 copy-only cross-kind data/settings transfer & collision
//! recovery tests (M8-M07, M8-M08, M8-M09, M8-M10).

use std::fs;

use axum::body::Bytes;
use axum::http::{Method, StatusCode};
use serde_json::{Map, Value, json};
use vosu_core::migration::{
    AF2_TRANSFER_MAP, Af2TransferOutcome, CatalogSourceLookup, IntentStatus, MigrationOutcome,
    ValidatedCatalogEntry, execute_af2_transfer, load_journal, resolve_intents,
    run_local_migration, save_journal,
};
use vosu_core::plugin::settings::{PluginSettingsStore, SettingsStorage};
use vosu_core::tools::{ExtraRoots, ToolRegistry};
use vosu_overlay_host::CounterService;
use vosu_plugin_api::manifest::PackageKind;
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_proptest::bounded;
use vosu_sys::platform::BuildChannel;

use crate::support::{
    bundled_migration_schemas, read_json_value, run_release_startup, tool_dashboard_json,
};

fn downloader_schema() -> SettingsSchema {
    SettingsSchema::parse(
        r#"[
            {"uniqueID":"concurrency","type":"number","title":"Concurrency","value":2},
            {"uniqueID":"max_file_mb","type":"number","title":"Max MB","value":100},
            {"uniqueID":"no_video","type":"checkbox","title":"No video","value":false},
            {"uniqueID":"client_id","type":"text","title":"Client ID","value":""},
            {"uniqueID":"client_secret","type":"password","title":"Client secret","value":""}
        ]"#,
    )
    .expect("valid downloader schema")
}

#[tokio::test]
async fn legacy_user_tool_disabled_with_needs_2_0_update_in_tool_registry_and_data_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(config.join("tools/settings")).unwrap();
    fs::create_dir_all(data.join("tools/user-legacy-svc")).unwrap();
    fs::create_dir_all(data.join("tool-data/user-legacy-svc/sub")).unwrap();
    fs::create_dir_all(&cache).unwrap();

    // 1.x `tool.toml` containing [service], [api], and [[requires]] sections.
    let tool_toml_bytes = b"\
id = \"user-legacy-svc\"
name = \"Legacy User Service\"
version = \"1.4.0\"

[service]
command = \"./legacy-daemon\"
autostart = true

[api]
port = 29111

[[requires]]
capability = \"network\"
";
    fs::write(
        data.join("tools/user-legacy-svc/tool.toml"),
        tool_toml_bytes,
    )
    .unwrap();

    let durable_bytes = b"\x00\x01durable-sqlite-bytes-preserved\xff";
    fs::write(
        data.join("tool-data/user-legacy-svc/sub/db.bin"),
        durable_bytes,
    )
    .unwrap();

    let settings_bytes = br#"{"token":"keep-1x-user-settings","custom":42}"#;
    fs::write(
        config.join("tools/settings/user-legacy-svc.json"),
        settings_bytes,
    )
    .unwrap();

    // Before migration, user tool had `enabled: true`, `trusted: true`, `autostart: true`.
    fs::write(
        config.join("tools/state.json"),
        serde_json::to_vec_pretty(&json!({
            "user-legacy-svc": {
                "enabled": true,
                "trusted": true,
                "autostart": true
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let (outcome, cfg, _) = run_release_startup(&config, &data).await.unwrap();
    let MigrationOutcome::Completed(journal) = outcome else {
        panic!("expected Completed");
    };
    assert_eq!(
        journal.user_tools.receipt().unwrap().disabled_user_tools,
        vec!["user-legacy-svc"]
    );
    assert!(journal.pending_intents.is_empty());

    // `tools/state.json` persisted `enabled: false` while preserving extra fields.
    let saved_state = read_json_value(&config.join("tools/state.json"));
    assert_eq!(saved_state["user-legacy-svc"]["enabled"], json!(false));
    assert_eq!(saved_state["user-legacy-svc"]["autostart"], json!(true));

    // `tool.toml`, durable data, and settings file are byte-identical and untouched,
    // and NO `manifest.toml` was synthesized or `[service]`/`[api]`/`[[requires]]` translated.
    assert!(!data.join("tools/user-legacy-svc/manifest.toml").exists());
    assert_eq!(
        fs::read(data.join("tools/user-legacy-svc/tool.toml")).unwrap(),
        tool_toml_bytes
    );
    assert_eq!(
        fs::read(data.join("tool-data/user-legacy-svc/sub/db.bin")).unwrap(),
        durable_bytes
    );
    assert_eq!(
        fs::read(config.join("tools/settings/user-legacy-svc.json")).unwrap(),
        settings_bytes
    );

    // Verify through the REAL production `ToolRegistry`:
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

    let (list_status, list_body) =
        tool_dashboard_json(&registry, Method::GET, "/api/tools", Bytes::new()).await;
    assert_eq!(list_status, StatusCode::OK);
    let tools = list_body["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["id"], json!("user-legacy-svc"));
    assert_eq!(tools[0]["enabled"], json!(false));
    assert_eq!(tools[0]["status"], json!("disabled"));
    assert_eq!(tools[0]["status_detail"], json!("needs-2.0-update"));
    assert_eq!(tools[0]["requires"], json!([]));
    assert_eq!(tools[0]["optional"], json!([]));
    assert_eq!(tools[0]["ui_url"], Value::Null);

    // Attempting to enable or launch the 1.x user tool is refused with `needs-2.0-update`.
    let (enable_status, enable_body) = tool_dashboard_json(
        &registry,
        Method::POST,
        "/api/tools/user-legacy-svc/enable",
        Bytes::new(),
    )
    .await;
    assert_eq!(enable_status, StatusCode::BAD_REQUEST);
    assert_eq!(enable_body["error"], json!("needs-2.0-update"));

    let (launch_status, launch_body) = tool_dashboard_json(
        &registry,
        Method::POST,
        "/api/tools/user-legacy-svc/launch",
        Bytes::from_static(b"{}"),
    )
    .await;
    assert_eq!(launch_status, StatusCode::BAD_REQUEST);
    assert_eq!(launch_body["error"], json!("needs-2.0-update"));

    bounded!(registry.shutdown());
    bounded!(cfg.shutdown()).unwrap();
}

#[tokio::test]
async fn legacy_user_overlay_stays_installed_and_enabled_never_disabled_as_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(config.join("tools")).unwrap();
    fs::create_dir_all(data.join("static/local-user-hud")).unwrap();
    fs::create_dir_all(data.join("static/third-party-hud")).unwrap();
    fs::create_dir_all(&cache).unwrap();

    let local_html = b"<!doctype html><h1>Local User HUD</h1>";
    fs::write(data.join("static/local-user-hud/index.html"), local_html).unwrap();
    fs::write(
        data.join("static/local-user-hud/metadata.txt"),
        "Usecase: ingame\nName: Local User HUD\nVersion: 1.0\nAuthor: LocalDev\nCompatibleWith: vosu\nResolution: 300x100\nauthorLinks: https://example.invalid\n",
    )
    .unwrap();

    fs::write(
        data.join("static/third-party-hud/index.html"),
        b"<!doctype html><h1>Third Party HUD</h1>",
    )
    .unwrap();
    fs::write(
        data.join("static/third-party-hud/metadata.txt"),
        "Usecase: ingame\nName: Third Party HUD\nVersion: 2.1\nAuthor: Community\nCompatibleWith: vosu\nResolution: 400x120\nauthorLinks: https://example.invalid\n",
    )
    .unwrap();

    // Non-official community overlay entry in overlay-catalog-state.json.
    fs::write(
        config.join("tools/overlay-catalog-state.json"),
        serde_json::to_vec_pretty(&json!({
            "third-party-hud": {
                "source": "community-org/overlays",
                "version": "2.1.0"
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let (outcome, cfg, _) = run_release_startup(&config, &data).await.unwrap();
    let MigrationOutcome::Completed(journal) = outcome else {
        panic!("expected Completed");
    };
    assert!(
        journal
            .user_tools
            .receipt()
            .unwrap()
            .disabled_user_tools
            .is_empty()
    );
    assert!(journal.pending_intents.is_empty());
    assert!(!config.join("tools/state.json").exists());
    assert_eq!(
        fs::read(data.join("static/local-user-hud/index.html")).unwrap(),
        local_html
    );

    // Real production CounterService discovers both user overlays as enabled/installed.
    let counters = CounterService::production(data.join("static"), config.clone(), cache)
        .discover()
        .unwrap();
    let folders: Vec<&str> = counters.iter().map(|c| c.folder_name.as_str()).collect();
    assert!(folders.contains(&"local-user-hud"));
    assert!(folders.contains(&"third-party-hud"));

    bounded!(cfg.shutdown()).unwrap();
}

#[test]
fn official_intents_cover_zero_one_multiple_wrong_kind_and_lookup_error_without_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config");
    let data = tmp.path().join("data");
    fs::create_dir_all(config.join("tools")).unwrap();
    fs::create_dir_all(data.join("tools/unclaimed-official")).unwrap();
    fs::write(
        data.join("tools/unclaimed-official/tool.toml"),
        "id = \"unclaimed-official\"\n",
    )
    .unwrap();

    fs::write(
        config.join("tools/state.json"),
        serde_json::to_vec_pretty(&json!({
            "map-analysis": {"official": true, "catalog_source": "18-lab-dev/vosu-tools"},
            "practice": {"official": true, "catalog_source": "18-lab-dev/vosu-tools"},
            "unclaimed-official": {"official": true, "catalog_source": "18-lab-dev/vosu-tools"},
            "vosulay": {"official": true, "catalog_source": "18-lab-dev/vosulay"}
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        config.join("tools/overlay-catalog-state.json"),
        serde_json::to_vec_pretty(&json!({
            "pp-counter": {"source": "18-lab-dev/vosu-overlays", "version": "1.0.0"},
            "wrong-kind-overlay": {"source": "18-lab-dev/vosu-overlays", "version": "1.0.0"},
            "ambiguous-overlay": {"source": "18-lab-dev/vosu-overlays", "version": "1.0.0"}
        }))
        .unwrap(),
    )
    .unwrap();

    let schemas = bundled_migration_schemas();
    let store = PluginSettingsStore::open(&config);
    let MigrationOutcome::Completed(mut journal) =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
    else {
        panic!("expected Completed");
    };

    let lookups = vec![
        CatalogSourceLookup {
            source: "18-lab-dev/vosu-tools".to_owned(),
            entries: Ok(vec![
                // Exact 1 claim -> Pending.
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
                // >1 same-kind claims -> Conflict (never arbitrary first pick).
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
                // Wrong kind (Plugin claiming 1.x Tool) MUST be rejected -> Unmanaged.
                ValidatedCatalogEntry {
                    id: "vosu.tools.unclaimed".to_owned(),
                    kind: PackageKind::Plugin,
                    replaces: vec!["unclaimed-official".to_owned()],
                },
                // Wrong source claiming vosulay MUST NOT be used as fallback for errored 18-lab-dev/vosulay.
                ValidatedCatalogEntry {
                    id: "wrong-source-vosulay".to_owned(),
                    kind: PackageKind::Tool,
                    replaces: vec!["vosulay".to_owned()],
                },
            ]),
        },
        CatalogSourceLookup {
            source: "18-lab-dev/vosulay".to_owned(),
            entries: Err("catalog fetch timed out".to_owned()),
        },
        CatalogSourceLookup {
            source: "18-lab-dev/vosu-overlays".to_owned(),
            entries: Ok(vec![
                ValidatedCatalogEntry {
                    id: "pp-counter-v2".to_owned(),
                    kind: PackageKind::Overlay,
                    replaces: vec!["pp-counter".to_owned()],
                },
                // Wrong kind (Tool claiming 1.x Overlay) MUST be rejected -> Unmanaged.
                ValidatedCatalogEntry {
                    id: "tool-claiming-overlay".to_owned(),
                    kind: PackageKind::Tool,
                    replaces: vec!["wrong-kind-overlay".to_owned()],
                },
                // >1 overlay claims -> Conflict.
                ValidatedCatalogEntry {
                    id: "ambiguous-a".to_owned(),
                    kind: PackageKind::Overlay,
                    replaces: vec!["ambiguous-overlay".to_owned()],
                },
                ValidatedCatalogEntry {
                    id: "ambiguous-b".to_owned(),
                    kind: PackageKind::Overlay,
                    replaces: vec!["ambiguous-overlay".to_owned()],
                },
            ]),
        },
    ];

    let summary = resolve_intents(&mut journal, &lookups);
    assert_eq!(summary.conflicts, 2);
    assert_eq!(summary.unmanaged, 3); // unclaimed-official tool + practice af2 (missing plugin) + wrong-kind-overlay
    assert_eq!(summary.retryable_errors, 2); // vosulay tool + vosulay af2
    assert_eq!(summary.pending_resolved, 3); // map-analysis tool + map-analysis af2 + pp-counter overlay

    let find = |id: &str| {
        journal
            .pending_intents
            .iter()
            .find(|intent| intent.intent_id == id)
            .unwrap_or_else(|| panic!("missing intent {id}"))
    };

    // 1 claim -> Pending, NEVER marked installed in catalog-installed.json.
    let map_tool = find("tool:18-lab-dev/vosu-tools:map-analysis");
    assert_eq!(map_tool.status, IntentStatus::Pending);
    assert_eq!(
        map_tool.resolved_target_id.as_deref(),
        Some("map-analysis-v2")
    );
    let pp_overlay = find("overlay:18-lab-dev/vosu-overlays:pp-counter");
    assert_eq!(pp_overlay.status, IntentStatus::Pending);
    assert_eq!(
        pp_overlay.resolved_target_id.as_deref(),
        Some("pp-counter-v2")
    );
    assert!(!data.join("catalog/catalog-installed.json").exists());
    assert!(!config.join("catalog-installed.json").exists());

    // >1 same-kind/source claims -> Conflict with sorted candidates.
    assert_eq!(
        find("tool:18-lab-dev/vosu-tools:practice").status,
        IntentStatus::Conflict {
            candidates: vec!["practice-alt".to_owned(), "practice-v2".to_owned()],
        }
    );
    assert_eq!(
        find("overlay:18-lab-dev/vosu-overlays:ambiguous-overlay").status,
        IntentStatus::Conflict {
            candidates: vec!["ambiguous-a".to_owned(), "ambiguous-b".to_owned()],
        }
    );

    // 0 matching same-kind claims -> Unmanaged, and 1.x directory left untouched on disk.
    assert_eq!(
        find("tool:18-lab-dev/vosu-tools:unclaimed-official").status,
        IntentStatus::Unmanaged
    );
    assert_eq!(
        find("overlay:18-lab-dev/vosu-overlays:wrong-kind-overlay").status,
        IntentStatus::Unmanaged
    );
    assert!(data.join("tools/unclaimed-official/tool.toml").is_file());

    // Lookup error -> RetryableError, zero cross-source fallback.
    assert!(matches!(
        find("tool:18-lab-dev/vosulay:vosulay").status,
        IntentStatus::RetryableError { .. }
    ));
    assert!(matches!(
        find("af2:vosulay:vosu.vosulay").status,
        IntentStatus::RetryableError { .. }
    ));
}

#[test]
fn af2_transfer_is_copy_only_isolates_secrets_and_never_reruns_after_completed() {
    assert_eq!(AF2_TRANSFER_MAP.len(), 9);
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
    let data_bytes = b"1x-downloader-queue-payload";
    fs::write(
        data.join("tool-data/downloader/nested/queue.db"),
        data_bytes,
    )
    .unwrap();
    let secret = "oauth-secret-isolated-to-plugin-store";
    let settings_bytes = serde_json::to_vec_pretty(&json!({
        "concurrency": 5,
        "max_file_mb": 200,
        "no_video": true,
        "client_id": "cid-42",
        "client_secret": secret,
        "hide_owned": true,
        "follow_current_map": false,
        "unlisted_extra": "must-not-migrate"
    }))
    .unwrap();
    fs::write(
        config.join("tools/settings/downloader.json"),
        &settings_bytes,
    )
    .unwrap();

    let schemas = bundled_migration_schemas();
    let store = PluginSettingsStore::open(&config);
    let MigrationOutcome::Completed(mut journal) =
        run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
    else {
        panic!("expected Completed");
    };

    let intent_id = "af2:downloader:vosu.tools.downloader";
    let dl_schema = downloader_schema();
    let outcome = execute_af2_transfer(
        &config,
        &data,
        &mut journal,
        intent_id,
        &store,
        Some(&dl_schema),
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
                "max_file_mb".to_owned(),
                "no_video".to_owned(),
            ],
            imported_web_tool_keys: vec!["follow_current_map".to_owned(), "hide_owned".to_owned(),],
        }
    );

    // Copy-only: 1.x source files remain byte-identical and untouched.
    assert_eq!(
        fs::read(data.join("tool-data/downloader/nested/queue.db")).unwrap(),
        data_bytes
    );
    assert_eq!(
        fs::read(config.join("tools/settings/downloader.json")).unwrap(),
        settings_bytes
    );

    // Durable data copied to `<data>/plugins/vosu.tools.downloader/`.
    assert_eq!(
        fs::read(data.join("plugins/vosu.tools.downloader/nested/queue.db")).unwrap(),
        data_bytes
    );

    // Plugin settings store holds allowlisted plugin keys including `client_secret`,
    // never web-tool keys or unlisted keys.
    let plugin_vals = store.load("vosu.tools.downloader").unwrap();
    assert_eq!(plugin_vals.get("client_secret"), Some(&json!(secret)));
    assert_eq!(plugin_vals.get("concurrency"), Some(&json!(5)));
    assert!(!plugin_vals.contains_key("hide_owned"));
    assert!(!plugin_vals.contains_key("unlisted_extra"));

    // Web tool settings hold ONLY web-tool keys — NEVER `client_secret` or unlisted keys.
    let web_raw = fs::read_to_string(config.join("tools/settings/downloader-v2.json")).unwrap();
    assert!(
        !web_raw.contains(secret),
        "secret leaked into web tool settings!"
    );
    let web_vals: Value = serde_json::from_str(&web_raw).unwrap();
    assert_eq!(web_vals["hide_owned"], json!(true));
    assert_eq!(web_vals["follow_current_map"], json!(false));
    assert!(web_vals.get("client_secret").is_none());
    assert!(web_vals.get("unlisted_extra").is_none());

    // Modify 2.0 destination after completion; re-running MUST return AlreadyCompleted
    // and never overwrite post-transfer edits!
    fs::write(
        data.join("plugins/vosu.tools.downloader/nested/queue.db"),
        b"2.0-user-updated",
    )
    .unwrap();
    let rerun = execute_af2_transfer(
        &config,
        &data,
        &mut journal,
        intent_id,
        &store,
        Some(&dl_schema),
    )
    .unwrap();
    assert_eq!(rerun, Af2TransferOutcome::AlreadyCompleted);
    assert_eq!(
        fs::read(data.join("plugins/vosu.tools.downloader/nested/queue.db")).unwrap(),
        b"2.0-user-updated"
    );
}

#[test]
fn af2_transfer_detects_no_overwrite_collisions_across_all_destinations() {
    let schemas = bundled_migration_schemas();
    let dl_schema = downloader_schema();
    let intent_id = "af2:downloader:vosu.tools.downloader";

    for kind in ["data_dir", "plugin_settings", "web_tool_settings"] {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        fs::create_dir_all(config.join("tools/settings")).unwrap();
        fs::create_dir_all(data.join("tool-data/downloader")).unwrap();

        fs::write(
            config.join("tools/state.json"),
            serde_json::to_vec_pretty(&json!({
                "downloader": {"official": true, "catalog_source": "18-lab-dev/vosu-tools"}
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(data.join("tool-data/downloader/item.dat"), b"1x-source").unwrap();
        fs::write(
            config.join("tools/settings/downloader.json"),
            serde_json::to_vec_pretty(&json!({
                "concurrency": 4,
                "hide_owned": true
            }))
            .unwrap(),
        )
        .unwrap();

        let store = PluginSettingsStore::open(&config);
        match kind {
            "data_dir" => {
                fs::create_dir_all(data.join("plugins/vosu.tools.downloader")).unwrap();
                fs::write(
                    data.join("plugins/vosu.tools.downloader/existing.dat"),
                    b"keep-2x-data",
                )
                .unwrap();
            }
            "plugin_settings" => {
                store
                    .store(
                        "vosu.tools.downloader",
                        &Map::from_iter([("concurrency".to_owned(), json!(9))]),
                    )
                    .unwrap();
            }
            "web_tool_settings" => {
                fs::write(
                    config.join("tools/settings/downloader-v2.json"),
                    r#"{"hide_owned":false}"#,
                )
                .unwrap();
            }
            _ => unreachable!(),
        }

        let MigrationOutcome::Completed(mut journal) =
            run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
        else {
            panic!("expected Completed");
        };

        let outcome = execute_af2_transfer(
            &config,
            &data,
            &mut journal,
            intent_id,
            &store,
            Some(&dl_schema),
        )
        .unwrap();
        assert!(
            matches!(outcome, Af2TransferOutcome::Collision { .. }),
            "collision kind {kind} expected Collision, got {outcome:?}"
        );

        // Journal persisted Collision status.
        let reloaded = load_journal(&config).unwrap().unwrap();
        let intent = reloaded
            .pending_intents
            .iter()
            .find(|i| i.intent_id == intent_id)
            .unwrap();
        assert!(matches!(intent.status, IntentStatus::Collision { .. }));

        // Both sides untouched & no partial writes to other destinations!
        assert_eq!(
            fs::read(data.join("tool-data/downloader/item.dat")).unwrap(),
            b"1x-source"
        );
        match kind {
            "data_dir" => {
                assert_eq!(
                    fs::read(data.join("plugins/vosu.tools.downloader/existing.dat")).unwrap(),
                    b"keep-2x-data"
                );
                assert!(!data.join("plugins/vosu.tools.downloader/item.dat").exists());
                assert!(store.load("vosu.tools.downloader").unwrap().is_empty());
                assert!(!config.join("tools/settings/downloader-v2.json").exists());
            }
            "plugin_settings" => {
                assert_eq!(
                    store
                        .load("vosu.tools.downloader")
                        .unwrap()
                        .get("concurrency"),
                    Some(&json!(9))
                );
                assert!(!data.join("plugins/vosu.tools.downloader").exists());
                assert!(!config.join("tools/settings/downloader-v2.json").exists());
            }
            "web_tool_settings" => {
                assert_eq!(
                    fs::read_to_string(config.join("tools/settings/downloader-v2.json")).unwrap(),
                    r#"{"hide_owned":false}"#
                );
                assert!(!data.join("plugins/vosu.tools.downloader").exists());
                assert!(store.load("vosu.tools.downloader").unwrap().is_empty());
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn af2_transfer_resumes_cleanly_after_mid_transfer_interruption() {
    let schemas = bundled_migration_schemas();
    let dl_schema = downloader_schema();
    let intent_id = "af2:downloader:vosu.tools.downloader";

    for stage in ["after_data", "after_plugin_settings", "after_web_settings"] {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config");
        let data = tmp.path().join("data");
        fs::create_dir_all(config.join("tools/settings")).unwrap();
        fs::create_dir_all(data.join("tool-data/downloader")).unwrap();

        fs::write(
            config.join("tools/state.json"),
            serde_json::to_vec_pretty(&json!({
                "downloader": {"official": true, "catalog_source": "18-lab-dev/vosu-tools"}
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(data.join("tool-data/downloader/queue.db"), b"1x-queue").unwrap();
        fs::write(
            config.join("tools/settings/downloader.json"),
            serde_json::to_vec_pretty(&json!({
                "concurrency": 3,
                "client_secret": "resume-secret",
                "hide_owned": true
            }))
            .unwrap(),
        )
        .unwrap();

        let store = PluginSettingsStore::open(&config);
        let MigrationOutcome::Completed(mut journal) =
            run_local_migration(&config, &data, BuildChannel::Release, &schemas, &store).unwrap()
        else {
            panic!("expected Completed");
        };

        // Simulate committed substep outputs + durable journal receipt before crash.
        let intent = journal
            .pending_intents
            .iter_mut()
            .find(|i| i.intent_id == intent_id)
            .unwrap();
        let spec = intent.af2_transfer.as_mut().unwrap();

        // Substep 1 committed: data copied.
        fs::create_dir_all(data.join("plugins/vosu.tools.downloader")).unwrap();
        fs::write(
            data.join("plugins/vosu.tools.downloader/queue.db"),
            b"1x-queue",
        )
        .unwrap();
        spec.data_copied = true;

        if matches!(stage, "after_plugin_settings" | "after_web_settings") {
            store
                .store(
                    "vosu.tools.downloader",
                    &Map::from_iter([
                        ("concurrency".to_owned(), json!(3)),
                        ("client_secret".to_owned(), json!("resume-secret")),
                    ]),
                )
                .unwrap();
            spec.plugin_settings_imported = true;
        }
        if stage == "after_web_settings" {
            fs::write(
                config.join("tools/settings/downloader-v2.json"),
                "{\n  \"hide_owned\": true\n}\n",
            )
            .unwrap();
            spec.web_settings_copied = true;
        }
        save_journal(&config, &mut journal).unwrap();

        // Reload journal & store fresh (simulating process restart) and resume.
        let mut reloaded = load_journal(&config).unwrap().unwrap();
        let fresh_store = PluginSettingsStore::open(&config);
        let outcome = execute_af2_transfer(
            &config,
            &data,
            &mut reloaded,
            intent_id,
            &fresh_store,
            Some(&dl_schema),
        )
        .unwrap();
        assert!(
            matches!(outcome, Af2TransferOutcome::Completed { .. }),
            "stage {stage} failed to resume cleanly: {outcome:?}"
        );

        let final_intent = reloaded
            .pending_intents
            .iter()
            .find(|i| i.intent_id == intent_id)
            .unwrap();
        assert_eq!(final_intent.status, IntentStatus::Completed);
        assert_eq!(
            fs::read(data.join("plugins/vosu.tools.downloader/queue.db")).unwrap(),
            b"1x-queue"
        );
        assert_eq!(
            fresh_store
                .load("vosu.tools.downloader")
                .unwrap()
                .get("client_secret"),
            Some(&json!("resume-secret"))
        );
        let web_vals = read_json_value(&config.join("tools/settings/downloader-v2.json"));
        assert_eq!(web_vals["hide_owned"], json!(true));
    }
}
