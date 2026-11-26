

#[test]
fn production_page_adapter_substitutes_template_and_renders_html_contracts() {
    let page = render_homepage(
        PageService.dashboard_template(),
        DynamicPage::Dashboard,
        Some(2),
        None,
        "",
        r#"<article class="counter-card">counter</article>"#,
        None,
    );
    let html = String::from_utf8(page.body.to_vec()).unwrap();
    assert!(!html.contains("{{"), "every placeholder is substituted");
    assert!(html.contains(r#"data-count="installed">2</span>"#));
    // The first paint already carries the page's title, tab and search.
    assert!(html.contains(r#"<body data-page="installed">"#));
    assert!(html.contains(r#"<h1 id="page-title" tabindex="-1">Overlays</h1>"#));
    assert!(html.contains(r#"data-page="installed" href="/" aria-current="page""#));
    assert!(html.contains(r#"placeholder="Search overlays""#));

    let tools = render_homepage(
        PageService.dashboard_template(),
        DynamicPage::Tools,
        None,
        None,
        "",
        "",
        None,
    );
    let tools = String::from_utf8(tools.body.to_vec()).unwrap();
    assert!(tools.contains(r#"data-page="tools" href="/tools" aria-current="page""#));
    assert!(!tools.contains(r#"href="/" aria-current"#));
    let overlay_settings = render_homepage(
        PageService.dashboard_template(),
        DynamicPage::OverlaySettings,
        None,
        None,
        "",
        "",
        None,
    );
    let overlay_settings = String::from_utf8(overlay_settings.body.to_vec()).unwrap();
    assert!(!overlay_settings.contains("search-bar"));

    let hostile = render_homepage(
        PageService.dashboard_template(),
        DynamicPage::Dashboard,
        Some(1),
        None,
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
    assert!(html.contains(r#"data-action="show-update" hidden"#));

    let updated = render_homepage(
        PageService.dashboard_template(),
        DynamicPage::Dashboard,
        Some(0),
        Some(0),
        "",
        "",
        Some(&UpdateNotice::Minor("99.0.0".to_owned())),
    );
    let updated = String::from_utf8(updated.body.to_vec()).unwrap();
    assert!(updated.contains(r#"id="update-notice""#));
    assert!(!updated.contains(r#"data-action="show-update" hidden"#));
    assert!(!updated.contains("{{NEW_VERSION}}"));
    assert!(updated.contains("99.0.0 available"));

    let settings = render_runtime_settings(&Config::default());
    for binding in [
        "ENABLE_AUTOUPDATE",
        "OPEN_DASHBOARD_ON_STARTUP",
        #[cfg(target_os = "linux")]
        "START_AT_LOGIN",
        "CALCULATE_PP",
        "READ_MANIA_SCROLL_SPEED",
        "POLL_RATE",
        "PRECISE_DATA_POLL_RATE",
        "API_V1_SUBSCRIPTION_DEBOUNCE",
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
        assert!(settings.contains(
            r#"data-id="START_AT_LOGIN" type="checkbox" role="switch" aria-describedby="setting-START_AT_LOGIN-description">"#
        ));
        let enabled = render_runtime_settings(&Config {
            start_at_login: true,
            ..Config::default()
        });
        assert!(enabled.contains(
            r#"data-id="START_AT_LOGIN" type="checkbox" role="switch" aria-describedby="setting-START_AT_LOGIN-description" checked>"#
        ));
    }
    #[cfg(windows)]
    assert!(!settings.contains("START_AT_LOGIN"));
    if vosu_core::desktop::externally_managed() {
        assert!(settings.contains(
            r#"data-id="ENABLE_AUTOUPDATE" type="checkbox" role="switch" aria-describedby="setting-ENABLE_AUTOUPDATE-description" checked disabled>"#
        ));
        assert!(settings.contains("Updates are managed by your Linux package manager"));
    }
    for key in ["ENABLE_KEY_OVERLAY", "ENABLE_MANIA_KEY_HIT_ERRORS", "ENABLE_HIT_OFFSETS", "V2_SUBSCRIPTION_DEBOUNCE"] {
        assert!(!settings.contains(key), "{key}");
    }
    assert!(settings.contains(
        r#"data-id="API_V1_SUBSCRIPTION_DEBOUNCE" type="number" value="1" min="1" max="60000" step="any" required"#
    ));
    assert!(settings.contains(
        r#"data-id="PRECISE_DATA_POLL_RATE" type="number" value="10" min="1" max="60000" step="any" required"#
    ));
    let distinct = render_runtime_settings(&Config {
        api_v1_subscription_debounce: 7.375,
        ..Config::default()
    });
    assert!(distinct.contains(
        r#"data-id="API_V1_SUBSCRIPTION_DEBOUNCE" type="number" value="7.375" min="1" max="60000" step="any" required"#
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
        bounded!(ConfigStore::initialize(root.path().join("vosu.toml")))
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
        update: None,
    };

    let dashboard = bounded!(pages
        .render(DynamicPage::Dashboard, None))
        .unwrap();
    let dashboard = String::from_utf8(dashboard.body.to_vec()).unwrap();
    assert!(dashboard.contains(r#"data-count="installed">2</span>"#));
    assert!(!dashboard.contains(r#"class="catalog-notice""#));

    let unavailable = bounded!(pages
        .render(DynamicPage::Available, None))
        .unwrap();
    let unavailable = String::from_utf8(unavailable.body.to_vec()).unwrap();
    assert!(unavailable.contains(r#"data-count="installed">2</span>"#));
    assert!(unavailable.contains("<h2>Couldn't reach the community catalog</h2>"));
    // The third-party disclosure must not depend on a reachable catalog.
    assert!(unavailable.contains(r#"class="catalog-notice""#));

    bounded!(counters.shutdown());
    bounded!(config.shutdown()).unwrap();
}

#[tokio::test]
async fn one_unreadable_overlay_is_an_error_card_not_an_empty_page() {
    let root = tempfile::tempdir().unwrap();
    let static_root = root.path().join("static");
    let good = static_root.join("good");
    std::fs::create_dir_all(&good).unwrap();
    std::fs::write(good.join("index.html"), "").unwrap();
    let bad = static_root.join("bad &one");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("metadata.txt"), vec![b'x'; 64 * 1024 + 1]).unwrap();

    let counters = Arc::new(ReloadableCounterService::production(
        static_root,
        root.path().join("settings"),
        root.path().join("cache"),
    ));
    let config = Arc::new(
        bounded!(ConfigStore::initialize(root.path().join("vosu.toml")))
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
        update: None,
    };
    let dashboard = bounded!(pages
        .render(DynamicPage::Dashboard, None))
        .unwrap();
    let dashboard = String::from_utf8(dashboard.body.to_vec()).unwrap();
    // The readable overlay is listed; the broken folder is an error card
    // naming the file, with the actions that fix it. The count is the cards.
    assert!(dashboard.contains(r#"data-count="installed">2</span>"#));
    assert!(dashboard.contains(r#"data-overlay-id="good""#));
    assert!(dashboard.contains(r#"<article class="counter-card is-broken" data-overlay-id="bad &amp;one""#));
    assert!(dashboard.contains("<code>metadata.txt: "));
    assert!(dashboard.contains(r#"data-action="delete-counter" data-folder="bad &amp;one""#));
    assert!(!dashboard.contains("No overlays yet"));

    bounded!(counters.shutdown());
    bounded!(config.shutdown()).unwrap();
}

#[tokio::test]
async fn settings_save_persists_browser_numeric_strings_for_runtime_consumers() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("vosu.toml");
    let store = Arc::new(bounded!(ConfigStore::initialize(config_path.clone())).unwrap());
    let service = RuntimeConfig::new(Arc::clone(&store));

    bounded!(service
        .save(serde_json::json!({
            "POLL_RATE": " 175.5 ",
            "PRECISE_DATA_POLL_RATE": "\t12.25\n",
            "V2_SUBSCRIPTION_DEBOUNCE": " 1.125 ",
            "API_V1_SUBSCRIPTION_DEBOUNCE": " 7.375 ",
            "SERVER_PORT": " 24890 ",
            "DASHBOARD_PORT": " 24891 ", "_network_action": "apply"
        })))
        .unwrap();
    bounded!(service.save(serde_json::json!({"_network_action":"confirm", "_request_origin":"http://127.0.0.1:24891"}))).unwrap();

    let current = store.current();
    assert_eq!(current.poll_rate, 175.5);
    assert_eq!(current.precise_data_poll_rate, 12.25);
    assert_eq!(current.api_v1_subscription_debounce, 7.375);
    assert_eq!(
        listen_target(&current).unwrap(),
        ListenTarget::new("127.0.0.1", 24_890)
    );

    drop(service);
    bounded!(store.shutdown()).unwrap();
    drop(store);

    let reloaded = bounded!(ConfigStore::initialize(config_path)).unwrap();
    let current = reloaded.current();
    assert_eq!(current.poll_rate, 175.5);
    assert_eq!(current.precise_data_poll_rate, 12.25);
    assert_eq!(current.api_v1_subscription_debounce, 7.375);
    assert_eq!(current.server_port, 24_890.0);
    bounded!(reloaded.shutdown()).unwrap();
}

#[tokio::test]
async fn settings_reject_a_missing_overlays_folder_and_revert_pending_network_changes() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(
        bounded!(ConfigStore::initialize(root.path().join("vosu.toml")))
            .unwrap(),
    );
    let service = RuntimeConfig::new(Arc::clone(&store));

    // A typo in an absolute overlays folder is rejected, not created empty.
    let missing = root.path().join("no such folder");
    let error = bounded!(service
        .save(serde_json::json!({"STATIC_FOLDER_PATH": missing.to_str().unwrap()})))
        .unwrap_err();
    assert_eq!(error, format!("Overlays folder not found: {}", missing.display()));
    bounded!(service
        .save(serde_json::json!({"STATIC_FOLDER_PATH": root.path().to_str().unwrap()})))
        .unwrap();
    assert_eq!(store.current().static_folder_path, root.path().to_str().unwrap());

    // "Revert now" restores the previous listeners at once.
    let before = listen_target(&store.current()).unwrap();
    bounded!(service
        .save(serde_json::json!({"SERVER_PORT": 24_890, "_network_action": "apply"})))
        .unwrap();
    assert_eq!(store.current().server_port, 24_890.0);
    bounded!(service
        .save(serde_json::json!({"_network_action": "revert"})))
        .unwrap();
    assert_eq!(listen_target(&store.current()).unwrap(), before);
    assert_eq!(
        bounded!(service.save(serde_json::json!({"_network_action": "revert"}))).unwrap_err(),
        "No pending network change"
    );

    drop(service);
    bounded!(store.shutdown()).unwrap();
}

#[tokio::test]
async fn out_of_range_or_unparseable_save_preserves_runtime_and_disk_configuration() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vosu.toml");
    let store = Arc::new(bounded!(ConfigStore::initialize(path.clone())).unwrap());
    bounded!(store.flush()).unwrap();
    let original = store.current();
    let bytes = std::fs::read(&path).unwrap();
    let service = RuntimeConfig::new(Arc::clone(&store));
    let cases = [
        ("POLL_RATE", serde_json::json!(99)),
        ("PRECISE_DATA_POLL_RATE", serde_json::json!(0.5)),
        ("V2_SUBSCRIPTION_DEBOUNCE", serde_json::json!(0)),
        ("V2_SUBSCRIPTION_DEBOUNCE", serde_json::json!(0.5)),
        ("API_V1_SUBSCRIPTION_DEBOUNCE", serde_json::json!(0)),
        ("API_V1_SUBSCRIPTION_DEBOUNCE", serde_json::json!(0.5)),
        ("SERVER_PORT", serde_json::json!(0)),
        ("SERVER_PORT", serde_json::json!(65_536)),
        ("SERVER_PORT", serde_json::json!("24050.5")),
        ("DASHBOARD_PORT", serde_json::json!("24051.5")),
    ];
    let unbounded = ["POLL_RATE", "PRECISE_DATA_POLL_RATE", "V2_SUBSCRIPTION_DEBOUNCE", "API_V1_SUBSCRIPTION_DEBOUNCE", "SERVER_PORT", "DASHBOARD_PORT"]
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
        assert!(bounded!(service.save(object.into())).is_err(), "{key}={value}");
        assert_eq!(*store.current(), *original);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    assert_eq!(poll_duration(f64::MAX), Duration::from_secs(60));
    assert!(std::time::Instant::now().checked_add(poll_duration(f64::MAX)).is_some());
    bounded!(store.shutdown()).unwrap();
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
    assert_eq!(config.api_v1_subscription_debounce, 1.0);
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
    let result = bounded!(updater.run_update(false));
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        result,
        Err(error) if error == vosu_core::desktop::MANAGED_UPDATE_ERROR
    ));
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
    let store = Arc::new(bounded!(ConfigStore::initialize(root.path().join("vosu.toml"))).unwrap());
    let service = RuntimeConfig::new(Arc::clone(&store));
    assert!(bounded!(service.save(serde_json::json!({"SERVER_PORT":2405}))).is_err());
    bounded!(service.save(serde_json::json!({"SERVER_PORT":2405,"DASHBOARD_PORT":24892,"_network_action":"apply"}))).unwrap();
    assert_eq!(store.current().server_port, 2405.0);
    assert!(bounded!(service.save(serde_json::json!({"SERVER_PORT":24893,"_network_action":"apply"}))).is_err());
    assert!(bounded!(service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://127.0.0.1:24051"}))).is_err());
    bounded!(service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://127.0.0.1:24892"}))).unwrap();
    assert!(bounded!(service.network.lock()).is_none());
    assert_eq!(store.current().server_port, 2405.0);
    bounded!(store.shutdown()).unwrap();
}

#[tokio::test]
async fn network_timer_reverts_port_typo_without_a_browser_and_preserves_other_autosaves() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("vosu.toml");
    let store = Arc::new(bounded!(ConfigStore::initialize(path.clone())).unwrap());
    let service = RuntimeConfig::new(Arc::clone(&store));
    bounded!(service.save(serde_json::json!({"SERVER_PORT":2405,"DASHBOARD_PORT":24891,"ALLOWED_IPS":"100.64.0.0/10","_network_action":"apply"}))).unwrap();
    bounded!(service.save(serde_json::json!({"DEBUG_LOG":true}))).unwrap();
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
    bounded!(store.shutdown()).unwrap();
}

#[tokio::test]
async fn network_confirmation_waits_for_the_new_effective_dashboard_target() {
    use crate::server::runtime::{EffectiveListener, ListenerState};
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(bounded!(ConfigStore::initialize(root.path().join("vosu.toml"))).unwrap());
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
    bounded!(service.save(serde_json::json!({"DASHBOARD_IP":"0.0.0.0","SERVER_PORT":24893,"_network_action":"apply","_request_origin":"http://127.0.0.1:24051"}))).unwrap();
    assert!(bounded!(service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://127.0.0.1:24051"}))).is_err(), "retained old listener cannot confirm a failed replacement");
    dashboard_tx.send(state("0.0.0.0", 24051)).unwrap();
    assert!(bounded!(service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://192.168.1.7:24051"}))).is_err(), "overlay replacement must also be effective");
    overlay_tx.send(state("127.0.0.1", 24893)).unwrap();
    bounded!(service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://192.168.1.7:24051"}))).unwrap();
    bounded!(store.shutdown()).unwrap();
}

#[tokio::test]
async fn failed_network_rollback_keeps_guard_and_retries_until_persisted() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct FailingReplace(Arc<AtomicBool>);
    impl vosu_core::config::AtomicReplace for FailingReplace {
        fn replace(&self, temp: &Path, target: &Path) -> io::Result<()> {
            if self.0.load(Ordering::SeqCst) {
                Err(io::Error::other("injected persistence failure"))
            } else {
                vosu_sys::platform::atomic_replace(temp, target)
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let store = Arc::new(bounded!(ConfigStore::initialize_with_options(root.path().join("vosu.toml"),
        vosu_core::config::ConfigStoreOptions { atomic_replace: Arc::new(FailingReplace(fail.clone())),
            ..vosu_core::config::ConfigStoreOptions::default() })).unwrap());
    let service = RuntimeConfig::new(store.clone());
    bounded!(service.save(serde_json::json!({"SERVER_PORT":2405,"_network_action":"apply"}))).unwrap();
    fail.store(true, Ordering::SeqCst);
    tokio::time::pause();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(5), async {
        while store.current().server_port != 24050.0 { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert!(bounded!(service.network.lock()).is_some(), "failed flush cannot clear pending guard");
    assert!(bounded!(service.save(serde_json::json!({"_network_action":"confirm","_request_origin":"http://127.0.0.1:24051"}))).is_err());
    fail.store(false, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(5), async {
        while service.network.lock().await.is_some() { tokio::task::yield_now().await; }
    }).await.unwrap();
    bounded!(store.shutdown()).unwrap();
}

#[tokio::test]
async fn wildcard_network_apply_retains_the_verified_remote_return_origin() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(bounded!(ConfigStore::initialize(root.path().join("vosu.toml"))).unwrap());
    store.update(|config| config.dashboard_ip = "0.0.0.0".into());
    bounded!(store.flush()).unwrap();
    let origins = Arc::new(crate::server::origins::Origins::new(
        "http://127.0.0.1:24050".into(), "http://127.0.0.1:24051".into()).unwrap());
    let mut service = RuntimeConfig::new(store.clone());
    service.origins = Some(origins.clone());
    bounded!(service.save(serde_json::json!({
        "DASHBOARD_PORT":24895, "_network_action":"apply",
        "_request_origin":"http://192.168.1.7:24051",
    }))).unwrap();
    assert_eq!(origins.previous_dashboard.read().as_deref(), Some("http://192.168.1.7:24051"));
    bounded!(store.shutdown()).unwrap();
}
