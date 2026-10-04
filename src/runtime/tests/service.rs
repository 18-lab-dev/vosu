#[test]
fn non_focused_instance_generation_invalidates_manager_revision() {
    let mut revisions = SemanticRevision::default();
    let initial = ManagerSemantics {
        focused_pid: Some(7),
        game_focused: true,
        enable_key_overlay: true,
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
        r#"<article class="counter-card">counter</article>"#,
        None,
    );
    let html = String::from_utf8(page.body.to_vec()).unwrap();
    for placeholder in [
        "{{LOCAL_AMOUNT}}",
        "{{AVAILABLE_AMOUNT}}",
        "{{SEARCH}}",
        "{{LIST}}",
        "{{UPDATE_HIDDEN}}",
        "{{CURRENT_VERSION}}",
        "{{NEW_VERSION}}",
    ] {
        assert!(!html.contains(placeholder));
    }
    assert!(html.contains(r#"data-count="installed">(2)</span>"#));
    assert!(html.contains("search-bar"));
    assert!(html.contains(r#"class="counter-card""#));

    let updated = render_homepage(
        PageService.dashboard_template(),
        Some(0),
        Some(0),
        true,
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
        "CALCULATE_PP",
        "READ_MANIA_SCROLL_SPEED",
        "ENABLE_KEY_OVERLAY",
        "POLL_RATE",
        "PRECISE_DATA_POLL_RATE",
        "V2_SUBSCRIPTION_DEBOUNCE",
        "SERVER_IP",
        "SERVER_PORT",
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
    assert!(settings.contains("V2 subscription debounce"));
    assert!(settings.contains(
        "Minimum milliseconds between selected-field WebSocket frames."
    ));
    assert!(settings.contains(
        r#"data-id="V2_SUBSCRIPTION_DEBOUNCE" type="number" value="0" min="0" step="any""#
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
    assert!(dashboard.contains(r#"data-count="installed">(2)</span>"#));

    let unavailable = pages
        .render(DynamicPage::Available, None)
        .await
        .unwrap();
    let unavailable = String::from_utf8(unavailable.body.to_vec()).unwrap();
    assert!(unavailable.contains(r#"data-count="installed">(2)</span>"#));
    assert!(unavailable.contains("Error:"));

    counters.shutdown().await;
    config.shutdown().await.unwrap();
}

#[tokio::test]
async fn settings_save_persists_browser_numeric_strings_for_runtime_consumers() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("vosu.toml");
    let store = Arc::new(ConfigStore::initialize(config_path.clone()).await.unwrap());
    let service = RuntimeConfig {
        config: Arc::clone(&store),
    };

    service
        .save(serde_json::json!({
            "POLL_RATE": " 175.5 ",
            "PRECISE_DATA_POLL_RATE": "\t12.25\n",
            "V2_SUBSCRIPTION_DEBOUNCE": " 0.125 ",
            "SERVER_PORT": " 24051 "
        }))
        .await
        .unwrap();

    let current = store.current();
    assert_eq!(current.poll_rate, 175.5);
    assert_eq!(current.precise_data_poll_rate, 12.25);
    assert_eq!(current.v2_subscription_debounce, 0.125);
    assert_eq!(
        listen_target(&current).unwrap(),
        ListenTarget::new("127.0.0.1", 24_051)
    );

    drop(service);
    store.shutdown().await.unwrap();
    drop(store);

    let reloaded = ConfigStore::initialize(config_path).await.unwrap();
    let current = reloaded.current();
    assert_eq!(current.poll_rate, 175.5);
    assert_eq!(current.precise_data_poll_rate, 12.25);
    assert_eq!(current.v2_subscription_debounce, 0.125);
    assert_eq!(current.server_port, 24_051.0);
    reloaded.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_interval_save_preserves_runtime_and_disk_configuration() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vosu.toml");
    let store = Arc::new(ConfigStore::initialize(path.clone()).await.unwrap());
    store.flush().await.unwrap();
    let original = store.current();
    let bytes = std::fs::read(&path).unwrap();
    let service = RuntimeConfig { config: Arc::clone(&store) };
    for key in ["POLL_RATE", "PRECISE_DATA_POLL_RATE", "V2_SUBSCRIPTION_DEBOUNCE"] {
        for value in [serde_json::json!(1e100), serde_json::json!("1e300")] {
            let mut object = serde_json::Map::new();
            object.insert(key.to_owned(), value);
            object.insert("STATIC_FOLDER_PATH".to_owned(), serde_json::json!("/"));
            assert!(service.save(object.into()).await.is_err());
            assert_eq!(*store.current(), *original);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
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
fn counter_cards_use_absolute_obs_url_and_source_resolution_rules() {
    let fixed = render_counter_card(
        "Counter",
        "Author",
        "1.0",
        "folder",
        false,
        false,
        false,
        None,
        &[],
        false,
        false,
        &[],
        &[serde_json::json!(640), serde_json::json!(480)],
        "localhost:24050",
    );
    assert!(fixed.contains(r#"src="http://localhost:24050/folder/""#));
    assert!(fixed.contains(r#"width="640px" height="480px""#));
    assert!(fixed.contains(r#"data-copy-value="640 × 480""#));
    assert!(fixed.contains(r#"<code>640 × 480</code>"#));

    let any = render_counter_card(
        "Counter",
        "Author",
        "1.0",
        "folder",
        false,
        false,
        false,
        None,
        &[],
        false,
        false,
        &[],
        &[serde_json::json!(-1), serde_json::json!(-2)],
        "127.0.0.1:24050",
    );
    assert!(any.contains(r#"width="500px" height="300px""#));
    assert!(any.contains(r#"data-copy-value="ANY × ANY""#));
    assert!(any.contains(r#"<code>ANY × ANY</code>"#));

    let flexible = render_counter_card(
        "Counter",
        "Author",
        "1.0",
        "folder",
        false,
        false,
        false,
        None,
        &[],
        false,
        false,
        &[],
        &[serde_json::json!(-2), serde_json::json!(-1)],
        "127.0.0.1:24050",
    );
    assert!(flexible.contains(r#"width="100%" height="500px""#));
}

#[test]
fn installed_counter_cards_emit_escaped_identity_and_pin_control() {
    let card = render_counter_card(
        "Counter",
        "Author",
        "1.0",
        r#"folder<&""#,
        false,
        false,
        false,
        None,
        &[],
        false,
        false,
        &[],
        &[],
        "localhost:24050",
    );

    assert!(card.starts_with(
        r#"<article class="counter-card" data-overlay-id="folder&lt;&amp;&quot;""#
    ));
    assert!(card.contains(
        r#"<button class="button" type="button" data-action="toggle-pin" aria-pressed="false">Pin</button>"#
    ));
}

#[test]
fn catalog_counter_cards_do_not_emit_identity_or_pin_control() {
    let card = render_counter_card(
        "Counter",
        "Author",
        "1.0",
        "catalog-folder",
        true,
        false,
        false,
        Some("https://example.com/counter.zip"),
        &["https://github.com/Author".to_owned()],
        false,
        false,
        &[],
        &[],
        "localhost:24050",
    );

    assert!(!card.contains("data-overlay-id"));
    assert!(!card.contains(r#"data-action="toggle-pin""#));
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
        "Counter",
        "Author",
        "1.0",
        "catalog-folder",
        true,
        false,
        false,
        None,
        &author_links,
        false,
        false,
        &assets,
        &[],
        "localhost:24050",
    );

    assert!(card.contains(r#"href="https://authors.example/profile""#));
    assert!(card.contains(r#"href="http://authors.example/profile""#));
    assert!(card.contains(r#"href="https://cdn.example/preview.png""#));
    assert!(card.contains(r#"href="http://cdn.example/preview.png""#));
    assert_eq!(card.matches(r#"class="author-link""#).count(), 2);
    assert_eq!(card.matches(r#"<li><a href=""#).count(), 2);
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

