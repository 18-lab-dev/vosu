#[tokio::test]
async fn dashboard_attachment_status_uses_existing_roster_and_published_version() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(ConfigStore::initialize(root.path().join("vosu.toml")).await.unwrap());
    let snapshots = Arc::new(SnapshotManager::new(Arc::new(ManagerSnapshot::not_ready(0, ApiRuntimeSettings::default()))));
    let provider = RuntimeReportInput {
        config: config.clone(), snapshots: snapshots.clone(),
        counters: Arc::new(ReloadableCounterService::production(root.path().join("static"), root.path().join("settings"), root.path().join("cache"))),
        logs: root.path().join("logs"), lifecycle: Arc::default(),
    };
    assert_eq!(provider.dashboard_status()["instances"], serde_json::json!([]));
    let mut state = (*state_bundle(3, 0)).clone();
    Arc::make_mut(&mut state.regular).settings.values.insert("client".into(), serde_json::json!({"version": "2026.1005.0"}));
    snapshots.publish(Arc::new(ManagerSnapshot::new(1, Some(42), false, ApiRuntimeSettings::default(), [
        ManagerInstanceSnapshot::new(42, crate::instance::InstanceSettings { tournament_spectator: false, tournament_ipc_id: None, devserver_endpoint: None }, false, Arc::new(state))
    ])));
    let status = provider.dashboard_status();
    assert_eq!(status["instances"][0]["pid"], 42);
    assert_eq!(status["instances"][0]["client"], "stable");
    assert_eq!(status["instances"][0]["version"], "2026.1005.0");
    assert_eq!(status["instances"][0]["state"], "reading");
    let (_, lifecycle) = tokio::sync::watch::channel(Arc::new(crate::instance::InstanceLifecycleSnapshot {
        revision: 2, instances: Arc::from([]), focus: Default::default(),
    }));
    provider.lifecycle.store(Some(Arc::new(lifecycle)));
    assert_eq!(provider.dashboard_status()["instances"], serde_json::json!([]), "exited instances must not linger through a stale published state");
    config.shutdown().await.unwrap();
}

#[test]
fn tool_context_exposes_only_selected_instance_game_and_skin_paths() {
    let root = tempfile::tempdir().unwrap();
    let snapshots = Arc::new(SnapshotManager::new(Arc::new(ManagerSnapshot::not_ready(0, ApiRuntimeSettings::default()))));
    let provider = RuntimeBeatmap {
        snapshots: snapshots.clone(),
        counters: Arc::new(ReloadableCounterService::production(root.path().join("static"), root.path().join("settings"), root.path().join("cache"))),
    };
    assert!(provider.tool_context()["osu_dir"].is_null());
    assert!(provider.tool_context()["skin"].is_null());
    let game = root.path().join("selected-game");
    for source in [ClientType::Stable, ClientType::Lazer] {
        let mut state = (*state_bundle(1, 0)).clone();
        state.source = source;
        let regular = Arc::make_mut(&mut state.regular);
        regular.global.game_folder = game.to_string_lossy().into_owned();
        regular.global.skin_folder = "Selected Skin".into();
        snapshots.publish(Arc::new(ManagerSnapshot::new(1, Some(42), false, ApiRuntimeSettings::default(), [
            ManagerInstanceSnapshot::new(42, crate::instance::InstanceSettings { tournament_spectator: false, tournament_ipc_id: None, devserver_endpoint: None }, false, Arc::new(state))
        ])));
        let context = provider.tool_context();
        assert_eq!(context["skin"]["name"], "Selected Skin");
        if source == ClientType::Stable {
            assert_eq!(context["osu_dir"], serde_json::json!(game));
            assert_eq!(context["skin"]["folder"], serde_json::json!(game.join("Skins/Selected Skin")));
        } else {
            assert!(context["osu_dir"].is_null());
            assert!(context["skin"]["folder"].is_null());
        }
    }
}

#[test]
fn non_focused_instance_generation_invalidates_manager_revision() {
    let mut revisions = SemanticRevision::default();
    let initial = ManagerSemantics {
        focused_pid: Some(7),
        game_focused: true,
        enable_key_overlay: true,
        enable_mania_key_hit_errors: false,
        enable_hit_offsets: false,
        instances: BTreeMap::from([(7, 10), (9, 20)]),
    };
    assert_eq!(revisions.observe(initial.clone()), 1);
    assert_eq!(revisions.observe(initial.clone()), 1);

    let mut non_focused_changed = initial.clone();
    non_focused_changed.instances.insert(9, 21);
    assert_eq!(revisions.observe(non_focused_changed), 2);

    let mut settings_changed = initial;
    settings_changed.enable_key_overlay = false;
    assert_eq!(revisions.observe(settings_changed), 3);
}
#[test]
fn production_page_adapter_substitutes_template_and_renders_html_contracts() {
    let page = render_homepage(
        PageService.dashboard_template(),
        Some(2),
        None,
        true,
        "",
        r#"<article class="counter-card">counter</article>"#,
        None,
    );
    let html = String::from_utf8(page.body.to_vec()).unwrap();
    for placeholder in [
        "{{LOCAL_AMOUNT}}",
        "{{AVAILABLE_AMOUNT}}",
        "{{SEARCH}}",
        "{{LIST}}",
        "{{NOTICE}}",
        "{{UPDATE_HIDDEN}}",
        "{{CURRENT_VERSION}}",
        "{{NEW_VERSION}}",
    ] {
        assert!(!html.contains(placeholder));
    }
    assert!(html.contains(r#"data-count="installed">2</span>"#));

    let hostile = render_homepage(
        PageService.dashboard_template(),
        Some(1),
        None,
        true,
        "",
        "<p>{{CURRENT_VERSION}} {{LIST}} {{UNKNOWN</p>",
        None,
    );
    let hostile = String::from_utf8(hostile.body.to_vec()).unwrap();
    assert!(
        hostile.contains("<p>{{CURRENT_VERSION}} {{LIST}} {{UNKNOWN</p>"),
        "substituted content must not be rescanned for placeholders"
    );
    assert!(html.contains("search-bar"));
    assert!(html.contains(r#"class="counter-card""#));

    let updated = render_homepage(
        PageService.dashboard_template(),
        Some(0),
        Some(0),
        true,
        "",
        "",
        Some("99.0.0"),
    );
    let updated = String::from_utf8(updated.body.to_vec()).unwrap();
    assert!(updated.contains(r#"id="update-notice""#));
    assert!(!updated.contains(r#"class="update-notice" hidden"#));
    assert!(!updated.contains("{{NEW_VERSION}}"));
    assert!(updated.contains("99.0.0"));

    let settings = render_runtime_settings(&Config::default());
    for binding in [
        "ENABLE_AUTOUPDATE",
        "OPEN_DASHBOARD_ON_STARTUP",
        #[cfg(target_os = "linux")]
        "START_AT_LOGIN",
        "CALCULATE_PP",
        "READ_MANIA_SCROLL_SPEED",
        "ENABLE_KEY_OVERLAY",
        "POLL_RATE",
        "PRECISE_DATA_POLL_RATE",
        "V2_SUBSCRIPTION_DEBOUNCE",
        "SERVER_IP",
        "SERVER_PORT",
        "DASHBOARD_IP",
        "DASHBOARD_PORT",
        "ALLOWED_IPS",
        "DEBUG_LOG",
        "SHOW_MP_COMMANDS",
        "STATIC_FOLDER_PATH",
    ] {
        assert!(
            settings.contains(&format!(r#"data-id="{binding}""#)),
            "{binding}"
        );
    }
    #[cfg(target_os = "linux")]
    {
        assert!(settings.contains("Run vosu as a systemd user service when you log in."));
        assert!(settings.contains(r#"data-id="START_AT_LOGIN" type="checkbox" >"#));
        let enabled = render_runtime_settings(&Config {
            start_at_login: true,
            ..Config::default()
        });
        assert!(enabled.contains(r#"data-id="START_AT_LOGIN" type="checkbox" checked>"#));
    }
    #[cfg(windows)]
    assert!(!settings.contains("START_AT_LOGIN"));
    if crate::desktop::externally_managed() {
        assert!(settings.contains(r#"data-id="ENABLE_AUTOUPDATE" type="checkbox" checked disabled>"#));
        assert!(settings.contains("Updates are managed by your Linux package manager"));
    }
    assert!(settings.contains("V2 subscription debounce"));
    assert!(settings.contains(
        "Wait at least this long between updates sent to overlays that request selected V2 fields."
    ));
    assert!(settings.contains(
        r#"data-id="V2_SUBSCRIPTION_DEBOUNCE" type="number" value="1" min="1" max="60000" step="any" required"#
    ));
    assert!(settings.contains(
        r#"data-id="PRECISE_DATA_POLL_RATE" type="number" value="10" min="1" max="60000" step="any" required"#
    ));
}

#[tokio::test]
async fn dynamic_pages_keep_discovered_installed_count_when_catalog_fetch_fails() {
    let root = tempfile::tempdir().unwrap();
    let static_root = root.path().join("static");
    for name in ["first", "second"] {
        let counter = static_root.join(name);
        std::fs::create_dir_all(&counter).unwrap();
        std::fs::write(counter.join("index.html"), "").unwrap();
    }

    let counters = Arc::new(ReloadableCounterService::production(
        static_root,
        root.path().join("settings"),
        root.path().join("cache"),
    ));
    let config = Arc::new(
        ConfigStore::initialize(root.path().join("vosu.toml"))
            .await
            .unwrap(),
    );
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    let pages = RuntimePages {
        pages: PageService,
        counters: Arc::clone(&counters),
        catalog: CatalogService::production("test"),
        config: Arc::clone(&config),
        shutdown,
        update_version: None,
    };

    let dashboard = pages
        .render(DynamicPage::Dashboard, None)
        .await
        .unwrap();
    let dashboard = String::from_utf8(dashboard.body.to_vec()).unwrap();
    assert!(dashboard.contains(r#"data-count="installed">2</span>"#));
    assert!(!dashboard.contains(r#"class="catalog-notice""#));

    let unavailable = pages
        .render(DynamicPage::Available, None)
        .await
        .unwrap();
    let unavailable = String::from_utf8(unavailable.body.to_vec()).unwrap();
    assert!(unavailable.contains(r#"data-count="installed">2</span>"#));
    assert!(unavailable.contains("Error:"));
    // The third-party disclosure must not depend on a reachable catalog.
    assert!(unavailable.contains(r#"class="catalog-notice""#));

    counters.shutdown().await;
    config.shutdown().await.unwrap();
}

#[tokio::test]
async fn settings_save_persists_browser_numeric_strings_for_runtime_consumers() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("vosu.toml");
    let store = Arc::new(ConfigStore::initialize(config_path.clone()).await.unwrap());
    let service = RuntimeConfig::new(Arc::clone(&store));

    service
        .save(serde_json::json!({
            "POLL_RATE": " 175.5 ",
            "PRECISE_DATA_POLL_RATE": "\t12.25\n",
            "V2_SUBSCRIPTION_DEBOUNCE": " 1.125 ",
            "SERVER_PORT": " 24890 ",
            "DASHBOARD_PORT": " 24891 ", "_network_action": "apply"
        }))
        .await
        .unwrap();
    service.save(serde_json::json!({"_network_action":"confirm", "_request_origin":"http://127.0.0.1:24891"})).await.unwrap();

    let current = store.current();
    assert_eq!(current.poll_rate, 175.5);
    assert_eq!(current.precise_data_poll_rate, 12.25);
    assert_eq!(current.v2_subscription_debounce, 1.125);
    assert_eq!(
        listen_target(&current).unwrap(),
        ListenTarget::new("127.0.0.1", 24_890)
    );

    drop(service);
    store.shutdown().await.unwrap();
    drop(store);

    let reloaded = ConfigStore::initialize(config_path).await.unwrap();
    let current = reloaded.current();
    assert_eq!(current.poll_rate, 175.5);
    assert_eq!(current.precise_data_poll_rate, 12.25);
    assert_eq!(current.v2_subscription_debounce, 1.125);
    assert_eq!(current.server_port, 24_890.0);
    reloaded.shutdown().await.unwrap();
}

#[tokio::test]
async fn out_of_range_or_unparseable_save_preserves_runtime_and_disk_configuration() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vosu.toml");
    let store = Arc::new(ConfigStore::initialize(path.clone()).await.unwrap());
    store.flush().await.unwrap();
    let original = store.current();
    let bytes = std::fs::read(&path).unwrap();
    let service = RuntimeConfig::new(Arc::clone(&store));
    let cases = [
        ("POLL_RATE", serde_json::json!(99)),
        ("PRECISE_DATA_POLL_RATE", serde_json::json!(0.5)),
        ("V2_SUBSCRIPTION_DEBOUNCE", serde_json::json!(0)),
        ("V2_SUBSCRIPTION_DEBOUNCE", serde_json::json!(0.5)),
        ("SERVER_PORT", serde_json::json!(0)),
        ("SERVER_PORT", serde_json::json!(65_536)),
        ("SERVER_PORT", serde_json::json!("24050.5")),
        ("DASHBOARD_PORT", serde_json::json!("24051.5")),
    ];
    let unbounded = ["POLL_RATE", "PRECISE_DATA_POLL_RATE", "V2_SUBSCRIPTION_DEBOUNCE", "SERVER_PORT", "DASHBOARD_PORT"]
        .into_iter()
        .flat_map(|key| {
            [
                serde_json::json!(1e100),
                serde_json::json!("1e300"),
                serde_json::json!(""),
                serde_json::json!("abc"),
            ]
            .map(|value| (key, value))
        });
    for (key, value) in cases.into_iter().chain(unbounded) {
        let mut object = serde_json::Map::new();
        object.insert(key.to_owned(), value.clone());
        object.insert("_network_action".into(), serde_json::json!("apply"));
        object.insert("STATIC_FOLDER_PATH".to_owned(), serde_json::json!("/"));
        assert!(service.save(object.into()).await.is_err(), "{key}={value}");
        assert_eq!(*store.current(), *original);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    assert_eq!(poll_duration(f64::MAX), Duration::from_secs(60));
    assert!(std::time::Instant::now().checked_add(poll_duration(f64::MAX)).is_some());
    store.shutdown().await.unwrap();
}

#[test]
fn settings_save_accepts_finite_json_numbers_and_ignores_invalid_or_nonfinite_values() {
    let mut config = Config::default();
    apply_config_values(
        &mut config,
        serde_json::json!({
            "POLL_RATE": 144.5,
            "PRECISE_DATA_POLL_RATE": 8,
            "V2_SUBSCRIPTION_DEBOUNCE": 0.25,
            "SERVER_PORT": 24060
        })
        .as_object()
        .unwrap(),
    );

    assert_eq!(config.poll_rate, 144.5);
    assert_eq!(config.precise_data_poll_rate, 8.0);
    assert_eq!(config.v2_subscription_debounce, 0.25);
    assert_eq!(config.server_port, 24_060.0);

    let unchanged = config.clone();
    apply_config_values(
        &mut config,
        serde_json::json!({
            "POLL_RATE": "not-a-number",
            "PRECISE_DATA_POLL_RATE": "NaN",
            "V2_SUBSCRIPTION_DEBOUNCE": "inf",
            "SERVER_PORT": "   "
        })
        .as_object()
        .unwrap(),
    );
    assert_eq!(config, unchanged);
}

#[test]
fn settings_save_parses_start_at_login_without_running_user_manager() {
    let mut config = Config::default();
    for value in [serde_json::json!(true), serde_json::json!(false)] {
        apply_config_values(
            &mut config,
            serde_json::json!({ "START_AT_LOGIN": value }).as_object().unwrap(),
        );
        assert_eq!(config.start_at_login, value.as_bool().unwrap());
    }
}


#[cfg(target_os = "linux")]
#[tokio::test]
async fn updater_rejects_readonly_package_directory_before_network_access() {
    use std::os::unix::fs::PermissionsExt;

    // Root bypasses directory mode permissions, so this contract needs a user.
    // SAFETY: geteuid reads process credentials and has no pointer preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let updater = RuntimeUpdater::production(
        root.path().join("vosu"),
        root.path().to_path_buf(),
        CancellationToken::new(),
    );
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = updater.run_update().await;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        result,
        Err(error) if error == crate::desktop::MANAGED_UPDATE_ERROR
    ));
}
#[test]
fn counter_cards_preview_at_native_resolution_from_encoded_relative_paths() {
    let fixed = render_counter_card(
        &CounterCard {
            name: "Counter",
            folder: "accuracy by sxlvia",
            resolution: &[serde_json::json!(640), serde_json::json!(480)],
            ..CounterCard::default()
        },
        "localhost:24050",
    );
    assert!(fixed.contains(r#"src="http://localhost:24050/accuracy%20by%20sxlvia/""#));
    assert!(fixed.contains(r#"data-preview-width="640" data-preview-height="480""#));
    assert!(fixed.contains(r#"width="640" height="480" scrolling="no""#));
    assert!(fixed.contains(r#"data-copy-value="http://localhost:24050/accuracy%20by%20sxlvia/""#));
    assert!(fixed.contains(r#"<code>/accuracy by sxlvia/</code>"#));
    assert!(fixed.contains(r#"data-copy-value="640 × 480""#));

    let nested = render_counter_card(
        &CounterCard {
            folder: "parent/50% #1?",
            ..CounterCard::default()
        },
        "127.0.0.1:24050",
    );
    assert!(nested.contains(r#"src="http://127.0.0.1:24050/parent/50%25%20%231%3F/""#));

    let any = render_counter_card(
        &CounterCard {
            folder: "folder",
            resolution: &[serde_json::json!(-1), serde_json::json!(-2)],
            ..CounterCard::default()
        },
        "127.0.0.1:24050",
    );
    assert!(any.contains(r#"width="500" height="300""#));
    assert!(any.contains(r#"data-copy-value="ANY × ANY""#));

    let flexible = render_counter_card(
        &CounterCard {
            folder: "folder",
            resolution: &[serde_json::json!(-2), serde_json::json!(-1)],
            ..CounterCard::default()
        },
        "127.0.0.1:24050",
    );
    assert!(flexible.contains(r#"class="preview-stage is-fluid""#));
    assert!(flexible.contains(r#"width="100%" height="500""#));
}

#[test]
fn installed_updates_use_catalog_metadata_and_actual_folder_target() {
    let mut local: Vec<crate::counters::Counter> = serde_json::from_value(serde_json::json!([{
        "folderName": "custom-folder", "name": "OffsetSuggester", "author": "Pinossaur",
        "version": "0.1", "resolution": [350, 230], "authorlinks": [], "settings": []
    }])).unwrap();
    let remote = crate::server::catalog::decode_response(200, br#"[{"name":"OffsetSuggester","author":"Pinossaur","version":"1.0","downloadLink":"https://example.test/counter.zip"}]"#).unwrap();
    let installed = local.iter().map(local_overlay).collect::<Vec<_>>();
    let remote = crate::server::catalog::merge_local(remote, &installed);
    annotate_installed_updates(&mut local, &remote);
    let html = render_local_counters(&local, None, "localhost:24050");
    assert!(html.contains("Update available</button>"));
    assert!(html.contains(r#"class="badge badge-warning""#));
    assert!(html.contains(r#"data-action="update-counter""#));
    assert!(html.contains(r#"data-install-name="custom-folder""#));
    assert!(html.contains(r#"data-download-url="https://example.test/counter.zip""#));
}

#[test]
fn card_meta_omits_empty_or_unknown_author_segments() {
    for external in [false, true] {
        for author in ["", "  ", "unknown", "Unknown"] {
            let html = render_counter_card(&CounterCard {
                name: "Native small", version: "1.0.0", author, external,
                resolution: &[serde_json::json!(180), serde_json::json!(80)],
                ..CounterCard::default()
            }, "localhost:24050");
            let meta = html.split(r#"<p class="counter-card__meta">"#).nth(1).unwrap().split("</p>").next().unwrap();
            assert!(!meta.contains("<span></span>"), "{meta}");
            assert!(!meta.contains("unknown"), "{meta}");
            assert!(!meta.contains("by "), "{meta}");
            if !external {
                assert!(meta.starts_with("<span>v1.0</span><button"), "{meta}");
            }
        }
    }
}

#[test]
fn installed_counter_cards_emit_escaped_identity_pin_and_management_menu() {
    let card = render_counter_card(
        &CounterCard {
            name: "Counter",
            author: "Author",
            version: "1.0",
            folder: r#"folder<&""#,
            settings: true,
            ..CounterCard::default()
        },
        "localhost:24050",
    );

    assert!(card.starts_with(
        r#"<article class="counter-card" data-overlay-id="folder&lt;&amp;&quot;""#
    ));
    assert!(card.contains(r#"<h2 title="Counter">Counter</h2>"#));
    assert!(card.contains(r#"data-action="toggle-pin" aria-pressed="false""#));
    let header_actions = &card[..card.find(r#"<div class="card-menu""#).unwrap()];
    assert!(header_actions.contains(r#"data-action="counter-settings""#));
    assert!(!header_actions.contains(r#"data-action="delete-counter""#));
    let menu = &card[card.find(r#"<div class="card-menu""#).unwrap()..];
    assert!(menu.starts_with(r#"<div class="card-menu" id="card-menu-folder%3C&amp;%22" popover>"#));
    let delete = menu.find(r#"data-action="delete-counter""#).unwrap();
    for earlier in ["open-counter", "pin-earlier", "pin-later"] {
        assert!(menu.find(&format!(r#"data-action="{earlier}""#)).unwrap() < delete);
    }
    assert!(card.contains(r#"popovertarget="card-menu-folder%3C&amp;%22""#));
}

#[test]
fn duplicate_installed_titles_show_their_folder() {
    let counter = |folder: &str, name: &str| -> crate::counters::Counter {
        serde_json::from_value(serde_json::json!({
            "folderName": folder,
            "name": name,
            "author": "sxlvia",
            "version": "1.0.0",
            "resolution": [],
            "authorlinks": [],
            "settings": []
        }))
        .unwrap()
    };
    let html = render_local_counters(
        &[
            counter("accuracy", "accuracy"),
            counter("accuracy by sxlvia", "Accuracy"),
            counter("other", "other"),
        ],
        None,
        "localhost:24050",
    );
    assert_eq!(html.matches(r#"class="counter-card__folder""#).count(), 2);
    assert!(html.contains(r#"<span class="visually-hidden">Folder: </span>accuracy by sxlvia</p>"#));
    assert!(!html.contains(r#"</span>other</p>"#));
}

#[test]
fn catalog_counter_cards_do_not_emit_identity_or_pin_control() {
    let card = render_counter_card(
        &CounterCard {
            name: "Counter",
            author: "Author",
            folder: "catalog-folder",
            external: true,
            download_link: Some("https://example.com/counter.zip"),
            author_links: &["https://github.com/Author".to_owned()],
            ..CounterCard::default()
        },
        "localhost:24050",
    );

    assert!(!card.contains("data-overlay-id"));
    assert!(!card.contains(r#"data-action="toggle-pin""#));
    assert!(!card.contains("card-menu"));
    assert!(card.contains(r#">GitHub</a>"#));
    assert!(!card.contains("<img"));
}

#[test]
fn counter_card_links_require_absolute_http_or_https_urls() {
    let author_links = [
        "javascript:alert(1)".to_owned(),
        "https://authors.example/profile".to_owned(),
        "data:text/plain,unsafe".to_owned(),
        "/same-origin/profile".to_owned(),
        "http://authors.example/profile".to_owned(),
    ];
    let assets = [
        "/same-origin/preview.png",
        "https://cdn.example/preview.png",
        "javascript:alert(1)",
        "data:text/plain,unsafe",
        "http://cdn.example/preview.png",
    ];
    let card = render_counter_card(
        &CounterCard {
            name: "Counter",
            author: "Author",
            folder: "catalog-folder",
            external: true,
            author_links: &author_links,
            assets: &assets,
            ..CounterCard::default()
        },
        "localhost:24050",
    );

    assert!(card.contains(r#"href="https://authors.example/profile""#));
    assert!(card.contains(r#"href="http://authors.example/profile""#));
    assert!(card.contains(r#"src="https://cdn.example/preview.png""#));
    assert!(card.contains(r#"src="http://cdn.example/preview.png""#));
    assert_eq!(card.matches(r#"class="author-link""#).count(), 2);
    assert_eq!(card.matches(r#"class="gallery__slide""#).count(), 2);
    // Only the first preview is shown; the gallery browses the rest.
    assert_eq!(card.matches(r#" hidden>"#).count(), 1);
    assert!(card.contains(r#"<span class="gallery__count" aria-live="polite">1 / 2</span>"#));
    for rejected in [
        "javascript:alert(1)",
        "data:text/plain,unsafe",
        "/same-origin/profile",
        "/same-origin/preview.png",
    ] {
        assert!(!card.contains(rejected), "{rejected} must be omitted");
    }
}

#[test]
fn production_catalog_query_matches_source_token_rules() {
    assert_eq!(normalize_catalog_query("!HD SC"), "hd sc");
    assert_eq!(normalize_catalog_query("HD SC"), "hd sc");
    assert_eq!(normalize_catalog_query("  my  overlay! "), "my overlay");
    assert!(matches_catalog_query(
        "Counter",
        "Author",
        &["streamcompanion".into()],
        &normalize_catalog_query("HD SC")
    ));
    assert!(matches_catalog_query(
        "My-Overlay",
        "Author",
        &[],
        &normalize_catalog_query("my overlay")
    ));
    assert!(!matches_catalog_query(
        "Counter",
        "Author",
        &[],
        &normalize_catalog_query("my overlay")
    ));
    assert!(matches_catalog_query(
        "Counter",
        "Author",
        &["streamcompanion".into()],
        "sc"
    ));
    assert!(!matches_catalog_query(
        "Counter",
        "Author",
        &["vosu".into()],
        "vosu"
    ));
    assert!(matches_catalog_query("vosu counter", "Author", &[], "vosu"));
    assert!(!matches_catalog_query(
        "Counter",
        "Author",
        &["vosu".into()],
        "missing vosu"
    ));
}

#[test]
fn report_adapter_reads_real_instance_client_version() {
    let mut settings = crate::model::states::SettingsObject::new();
    settings.insert("client".into(), serde_json::json!({"version":"2026.714.0"}));
    assert_eq!(report_instance_version(&settings), "2026.714.0");
    settings.insert("client".into(), serde_json::json!({"version":42}));
    assert_eq!(report_instance_version(&settings), "");
}

#[test]
fn production_adapter_keeps_current_files_relative_to_songs() {
    assert_eq!(
        current_file_relative(Some("123 Artist - Title"), Some("map.osu")),
        Some(PathBuf::from("123 Artist - Title").join("map.osu"))
    );
    assert_eq!(
        current_file_relative(Some("../outside"), Some("map.osu")),
        Some(PathBuf::from("../outside").join("map.osu")),
        "the provider must not turn a request-derived path into a trust root"
    );
}

#[test]
fn production_adapter_builds_only_trusted_stable_skin_roots() {
    assert_eq!(
        stable_skin_root("/games/osu", "Default"),
        Some(PathBuf::from("/games/osu/Skins/Default"))
    );
    assert_eq!(stable_skin_root("/games/osu", ""), None);
    assert_eq!(stable_skin_root("/games/osu", "../outside"), None);
    assert_eq!(stable_skin_root("/games/osu", "nested/skin"), None);
    let absolute = format!("{}outside", std::path::MAIN_SEPARATOR);
    assert_eq!(stable_skin_root("/games/osu", &absolute), None);
}
#[test]
fn fractional_server_port_is_rejected() {
    let config = Config {
        server_port: 24_050.5,
        ..Config::default()
    };
    let error = listen_target(&config).expect_err("fractional ports must not be truncated");
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn authorization_policy_reloads_without_listener_rebuild() {
    let authorizer = ReloadableAuthorizer::new("127.0.0.1,localhost", "localhost");
    let peer = "127.0.0.1".parse().unwrap();
    assert!(
        authorizer
            .authorize(
                peer,
                RequestHeaders {
                    origin: Some("http://localhost:24050"),
                    referer: None,
                },
            )
            .allowed()
    );
    authorizer.reload("127.0.0.1,example.test", "example.test");
    assert!(
        !authorizer
            .authorize(
                peer,
                RequestHeaders {
                    origin: Some("http://localhost:24050"),
                    referer: None,
                },
            )
            .allowed()
    );
    assert!(
        authorizer
            .authorize(
                peer,
                RequestHeaders {
                    origin: Some("https://example.test"),
                    referer: None,
                },
            )
            .allowed()
    );
}

#[test]
fn startup_owner_conflict_remains_typed_for_exit_75_mapping() {
    let error = io::Error::new(io::ErrorKind::ResourceBusy, StartupOwnerConflict);
    assert!(is_startup_owner_conflict(&error));
    assert!(!is_startup_owner_conflict(&io::Error::other("ordinary")));
}


#[tokio::test]
async fn network_apply_requires_confirmation_on_new_origin_and_allows_only_one_pending_change() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(ConfigStore::initialize(root.path().join("vosu.toml")).await.unwrap());
    let service = RuntimeConfig::new(Arc::clone(&store));
    assert!(service.save(serde_json::json!({"SERVER_PORT":2405})).await.is_err());
    service.save(serde_json::json!({"SERVER_PORT":2405,"DASHBOARD_PORT":24892,"_network_action":"apply"})).await.unwrap();
    assert_eq!(store.current().server_port, 2405.0);
    assert!(service.save(serde_json::json!({"SERVER_PORT":24893,"_network_action":"apply"})).await.is_err());
    assert!(service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://127.0.0.1:24051"})).await.is_err());
    service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://127.0.0.1:24892"})).await.unwrap();
    assert!(service.network.lock().await.is_none());
    assert_eq!(store.current().server_port, 2405.0);
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn network_timer_reverts_port_typo_without_a_browser_and_preserves_other_autosaves() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vosu.toml");
    let store = Arc::new(ConfigStore::initialize(path.clone()).await.unwrap());
    let service = RuntimeConfig::new(Arc::clone(&store));
    service.save(serde_json::json!({"SERVER_PORT":2405,"DASHBOARD_PORT":24891,"ALLOWED_IPS":"100.64.0.0/10","_network_action":"apply"})).await.unwrap();
    service.save(serde_json::json!({"DEBUG_LOG":true})).await.unwrap();
    tokio::time::pause();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.network.lock().await.is_some() { tokio::task::yield_now().await; }
    }).await.unwrap();
    let current = store.current();
    assert_eq!(current.server_port, 24050.0);
    assert_eq!(current.dashboard_port, 24051.0);
    assert_eq!(current.allowed_ips, "127.0.0.1,localhost,absolute");
    assert!(current.debug_log);
    let persisted: toml::Value = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(persisted["SERVER_PORT"].as_float(), Some(24050.0));
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn network_confirmation_waits_for_the_new_effective_dashboard_target() {
    use crate::server::runtime::{EffectiveListener, ListenerState};
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(ConfigStore::initialize(root.path().join("vosu.toml")).await.unwrap());
    let origins = Arc::new(crate::server::origins::Origins::new("http://127.0.0.1:24050".into(), "http://127.0.0.1:24051".into()).unwrap());
    let state = |host: &str, port| ListenerState {
        effective: Some(EffectiveListener { target: ListenTarget::new(host, port), address: format!("{host}:{port}").parse().unwrap() }),
        ..ListenerState::default()
    };
    let (overlay_tx, overlay) = watch::channel(state("127.0.0.1", 24050));
    let (dashboard_tx, dashboard) = watch::channel(state("127.0.0.1", 24051));
    origins.observe_listeners(overlay, dashboard);
    let mut service = RuntimeConfig::new(Arc::clone(&store));
    service.origins = Some(origins);
    service.save(serde_json::json!({"DASHBOARD_IP":"0.0.0.0","SERVER_PORT":24893,"_network_action":"apply","_request_origin":"http://127.0.0.1:24051"})).await.unwrap();
    assert!(service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://127.0.0.1:24051"})).await.is_err(), "retained old listener cannot confirm a failed replacement");
    dashboard_tx.send(state("0.0.0.0", 24051)).unwrap();
    assert!(service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://192.168.1.7:24051"})).await.is_err(), "overlay replacement must also be effective");
    overlay_tx.send(state("127.0.0.1", 24893)).unwrap();
    service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://192.168.1.7:24051"})).await.unwrap();
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_network_rollback_keeps_guard_and_retries_until_persisted() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct FailingReplace(Arc<AtomicBool>);
    impl crate::config::AtomicReplace for FailingReplace {
        fn replace(&self, temp: &Path, target: &Path) -> io::Result<()> {
            if self.0.load(Ordering::SeqCst) {
                Err(io::Error::other("injected persistence failure"))
            } else {
                crate::platform::atomic_replace(temp, target)
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let store = Arc::new(ConfigStore::initialize_with_options(root.path().join("vosu.toml"),
        crate::config::ConfigStoreOptions { atomic_replace: Arc::new(FailingReplace(fail.clone())),
            ..crate::config::ConfigStoreOptions::default() }).await.unwrap());
    let service = RuntimeConfig::new(store.clone());
    service.save(serde_json::json!({"SERVER_PORT":2405,"_network_action":"apply"})).await.unwrap();
    fail.store(true, Ordering::SeqCst);
    tokio::time::pause();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(5), async {
        while store.current().server_port != 24050.0 { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert!(service.network.lock().await.is_some(), "failed flush cannot clear pending guard");
    assert!(service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://127.0.0.1:24051"})).await.is_err());
    fail.store(false, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.network.lock().await.is_some() { tokio::task::yield_now().await; }
    }).await.unwrap();
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn wildcard_network_apply_retains_the_verified_remote_return_origin() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(ConfigStore::initialize(root.path().join("vosu.toml")).await.unwrap());
    store.update(|config| config.dashboard_ip = "0.0.0.0".into());
    store.flush().await.unwrap();
    let origins = Arc::new(crate::server::origins::Origins::new(
        "http://127.0.0.1:24050".into(), "http://127.0.0.1:24051".into()).unwrap());
    let mut service = RuntimeConfig::new(store.clone());
    service.origins = Some(origins.clone());
    service.save(serde_json::json!({
        "DASHBOARD_PORT":24895, "_network_action":"apply",
        "_request_origin":"http://192.168.1.7:24051",
    })).await.unwrap();
    assert_eq!(origins.previous_dashboard.read().as_deref(), Some("http://192.168.1.7:24051"));
    store.shutdown().await.unwrap();
}
