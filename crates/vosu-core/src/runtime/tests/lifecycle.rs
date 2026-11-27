
#[tokio::test]
async fn signal_completion_cancels_the_shared_runtime_token() {
    let shutdown = CancellationToken::new();
    let observed = shutdown.clone();
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(cancel_on_signal(shutdown, async move {
        bounded!(receive)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "signal test closed"))
    }));

    send.send(()).expect("deliver synthetic signal");
    bounded!(task)
        .expect("join signal task")
        .expect("signal task result");
    assert!(observed.is_cancelled());
}

#[test]
fn candidate_and_rollback_roles_skip_release_update_transactions() {
    let readiness = StartupReadiness {
        record_path: PathBuf::from(".vosu-update.json"),
        transaction_token: "transaction".to_owned(),
        address: "127.0.0.1:1".parse().expect("readiness address"),
    };

    assert!(startup_role_allows_update_check(None));
    assert!(
        !startup_role_allows_update_check(Some(&readiness)),
        "candidate role must not collide with the helper-owned install record"
    );
    assert!(
        !startup_role_allows_update_check(Some(&readiness)),
        "rollback role must not collide with the helper-owned install record"
    );
}

#[test]
fn quarantined_recovery_is_nonfatal() {
    assert!(recovery_error_is_nonfatal(
        &crate::updater::UpdateError::Quarantined("unknown target".into())
    ));
    assert!(!recovery_error_is_nonfatal(
        &crate::updater::UpdateError::LiveOwner
    ));
}

#[tokio::test]
async fn run_error_still_invokes_shutdown() {
    let shutdown_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let called = shutdown_called.clone();
    let result = bounded!(finish_runtime_run(Err(io::Error::other("run failed")), async move {
        called.store(true, Ordering::Release);
        Ok(())
    }));
    assert_eq!(result.unwrap_err().to_string(), "run failed");
    assert!(shutdown_called.load(Ordering::Acquire));
}

#[tokio::test]
async fn failed_rebind_then_config_rollback_clears_stale_retry_target() {
    let old_target = ListenTarget::new("127.0.0.1", 24050);
    let replacement = ListenTarget::new("invalid.test", 24051);
    let (desired, mut desired_rx) = watch::channel(old_target.clone());
    let (state_tx, state) = watch::channel(ListenerState {
        desired: Some(old_target.clone()),
        effective: Some(crate::server::runtime::EffectiveListener {
            target: old_target.clone(),
            address: "127.0.0.1:24050".parse().unwrap(),
        }),
        last_error: None,
    });
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let retained = old_target.clone();
    let task = tokio::spawn(async move {
        bounded!(desired_rx.changed()).unwrap();
        let requested = desired_rx.borrow().clone();
        state_tx.send_replace(ListenerState {
            desired: Some(requested),
            effective: Some(crate::server::runtime::EffectiveListener {
                target: retained,
                address: "127.0.0.1:24050".parse().unwrap(),
            }),
            last_error: Some("bind failed".into()),
        });
        bounded!(task_cancel.cancelled());
    });
    let mut owner = ListenerOwner {
        desired,
        state,
        cancel,
        task,
        broker: None,
        broker_logger: None,
        cli_task: None,
    };
    assert!(bounded!(owner.stage(replacement.clone())).is_err());
    let state = owner.state();
    assert_eq!(state.desired, Some(replacement));
    assert_eq!(state.effective.unwrap().target, old_target);
    bounded!(owner
        .stage(old_target.clone()))
        .expect("rollback to effective target");
    assert_eq!(
        owner.desired.borrow().clone(),
        old_target,
        "rollback must replace the failed desired target even when it is already effective"
    );
    bounded!(owner.shutdown());
}

#[tokio::test]
async fn broker_publication_failure_keeps_effective_rebind_and_disables_writes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("broker.json");
    let broker = Arc::new(crate::server::broker::BrokerToken::new(path.clone()).unwrap());
    broker.publish("127.0.0.1:24050".parse().unwrap()).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::AUTHORIZATION,
        format!("Bearer {}", value["token"].as_str().unwrap()).parse().unwrap());
    let peer = Some("127.0.0.1".parse().unwrap());
    assert_eq!(broker.authorize(&headers, peer), Ok(true));
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let target = ListenTarget::new("127.0.0.1", 24051);
    let (desired, _desired_rx) = watch::channel(ListenTarget::new("127.0.0.1", 24050));
    let (_state_tx, state) = watch::channel(ListenerState {
        desired: Some(target.clone()),
        effective: Some(crate::server::runtime::EffectiveListener {
            target: target.clone(), address: "127.0.0.1:24051".parse().unwrap(),
        }),
        last_error: None,
    });
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move { bounded!(task_cancel.cancelled()); });
    let mut owner = ListenerOwner {
        desired, state, cancel, task, broker: Some(broker.clone()), broker_logger: None,
        cli_task: None,
    };
    bounded!(owner.stage(target.clone())).expect("live rebind must remain successful");
    assert_eq!(owner.state().effective.unwrap().target, target);
    assert_eq!(broker.authorize(&headers, peer), Err(()));
    bounded!(owner.shutdown());
}

#[tokio::test]
async fn listener_accept_stop_is_separate_from_forced_drain_join() {
    let target = ListenTarget::new("127.0.0.1", 24050);
    let (desired, _desired_rx) = watch::channel(target.clone());
    let (_state_tx, state) = watch::channel(ListenerState::default());
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let task_release = release.clone();
    let (events, mut event_rx) = mpsc::unbounded_channel();
    let task_events = events.clone();
    let task = tokio::spawn(async move {
        bounded!(task_cancel.cancelled());
        task_events.send("accept-stopped").unwrap();
        bounded!(task_release.notified());
        task_events.send("listener-joined").unwrap();
    });
    let owner = ListenerOwner {
        desired,
        state,
        cancel,
        task,
        broker: None,
        broker_logger: None,
        cli_task: None,
    };
    owner.stop_accepting();
    assert_eq!(bounded!(event_rx.recv()), Some("accept-stopped"));
    events.send("hubs-closed-1001").unwrap();
    release.notify_one();
    bounded!(owner.join());
    assert_eq!(bounded!(event_rx.recv()), Some("hubs-closed-1001"));
    assert_eq!(bounded!(event_rx.recv()), Some("listener-joined"));
}

#[test]
fn runtime_static_root_resolution_matches_paths_static_path_semantics() {
    let root = tempfile::tempdir().unwrap();
    let paths = runtime_test_paths(root.path());
    let store = Arc::new(PluginSettingsStore::open(paths.config()));
    let settings = PluginSettingsService::new(store.clone());
    let overlay = OverlayRootSetting {
        plugin: crate::migration::OVERLAY_HOST_PLUGIN_ID,
        key: crate::migration::OVERLAY_HOST_SETTING_KEY,
    };
    let schema = vosu_plugin_api::manifest::settings::SettingsSchema::parse(
        r#"[{"uniqueID":"static_folder_path","type":"text","title":"Overlay folder","value":""}]"#,
    )
    .unwrap();

    for configured in ["", "./static", "./StAtIc", ".\\STATIC"] {
        settings
            .submit(
                overlay.plugin,
                &schema,
                vec![(overlay.key.to_owned(), serde_json::json!(configured))],
            )
            .unwrap();
        let resolved = resolved_overlay_root(&paths, &settings, overlay).unwrap();
        assert_eq!(resolved, paths.data().join("static"), "{configured:?}");
        assert!(resolved.is_dir(), "{configured:?}");
    }

    settings
        .submit(
            overlay.plugin,
            &schema,
            vec![(overlay.key.to_owned(), serde_json::json!("custom/counters"))],
        )
        .unwrap();
    let resolved = resolved_overlay_root(&paths, &settings, overlay).unwrap();
    assert_eq!(resolved, paths.launch().join("custom/counters"));
    assert!(resolved.is_dir());
}

#[tokio::test]
async fn prerelease_startup_and_restart_import_only_once_into_isolated_roots() {
    use vosu_sys::platform::BuildChannel;

    const OSU_SCHEMA: &str = r#"[
        {"uniqueID":"CALCULATE_PP","type":"checkbox","title":"Calculate pp","value":true},
        {"uniqueID":"POLL_RATE","type":"number","title":"Poll rate","value":150.0},
        {"uniqueID":"PRECISE_DATA_POLL_RATE","type":"number","title":"Precise data poll rate","value":10.0},
        {"uniqueID":"SHOW_MP_COMMANDS","type":"checkbox","title":"Show multiplayer commands","value":false},
        {"uniqueID":"READ_MANIA_SCROLL_SPEED","type":"checkbox","title":"Read mania scroll speed","value":true}
    ]"#;
    const OVERLAY_SCHEMA: &str =
        r#"[{"uniqueID":"static_folder_path","type":"text","title":"Overlay folder","value":""}]"#;
    static TEST_PLUGINS: &[crate::plugin::BundledPlugin<'static>] = &[
        crate::plugin::BundledPlugin {
            manifest: "",
            settings_schema: Some(OSU_SCHEMA),
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: dummy_vtable,
        },
        crate::plugin::BundledPlugin {
            manifest: "",
            settings_schema: Some(OVERLAY_SCHEMA),
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: dummy_vtable,
        },
    ];
    fn dummy_vtable() -> vosu_plugin_api::vosu_plugin_vtable {
        unreachable!("vtable is never invoked by migration schema extraction")
    }
    static TEST_BUNDLE: Bundle = Bundle {
        version: "2.0.0-alpha.1",
        plugin_host_identity: "vosu/2.0.0-alpha.1/plugin-host-1",
        plugins: TEST_PLUGINS,
        plugin_ids: &["vosu.osu", "vosu.overlayhost"],
        dashboard: "vosu.dashboard",
        tool_host: "vosu.toolhost",
        overlay_root: OverlayRootSetting {
            plugin: "vosu.overlayhost",
            key: "static_folder_path",
        },
        hosts: &FixtureHosts,
    };

    let root = tempfile::tempdir().unwrap();
    let resolver = RuntimeTestPathResolver {
        executable: root.path().join("program/vosu"),
        launch: root.path().to_owned(),
        root: root.path().to_owned(),
    };
    let channel = BuildChannel::from_version("2.0.0-alpha.1").unwrap();
    assert_eq!(channel, BuildChannel::Prerelease);
    let normal = Paths::resolve(&resolver, BuildChannel::Release).unwrap();
    fs::create_dir_all(normal.config()).unwrap();
    let source = normal.config().join("vosu.toml");
    let original = "SERVER_PORT = 24643.0\nPOLL_RATE = 643.0\nSTATIC_FOLDER_PATH = 'normal-write-sentinel'\nUNKNOWN_SENTINEL = 97\n";
    fs::write(&source, original).unwrap();
    let alpha = Paths::resolve(&resolver, channel).unwrap();
    alpha.initialize().unwrap();
    let (store, settings_store) =
        bounded!(super::config::initialize_startup_storage(&TEST_BUNDLE, &alpha, channel)).unwrap();
    assert_eq!(store.current().server_port, 24643.0);
    let settings_service = PluginSettingsService::new(settings_store.clone());
    assert_eq!(effective_osu_poll_rates(&settings_service), (643.0, 10.0));
    let static_root =
        resolved_overlay_root(&alpha, &settings_service, TEST_BUNDLE.overlay_root).unwrap();
    assert_eq!(static_root, alpha.data().join("static"));
    assert_ne!(static_root, normal.data().join("static"));
    assert!(!normal.config().join(crate::migration::JOURNAL_FILENAME).exists());
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    bounded!(store.shutdown()).unwrap();
    fs::write(&source, "SERVER_PORT = [malformed after import").unwrap();
    let (restarted, restarted_settings) =
        bounded!(super::config::initialize_startup_storage(&TEST_BUNDLE, &alpha, channel)).unwrap();
    assert_eq!(restarted.current().server_port, 24643.0);
    assert_eq!(
        effective_osu_poll_rates(&PluginSettingsService::new(restarted_settings)),
        (643.0, 10.0)
    );
    assert_eq!(
        fs::read_to_string(&source).unwrap(),
        "SERVER_PORT = [malformed after import"
    );
    bounded!(restarted.shutdown()).unwrap();
}

#[tokio::test]
async fn release_startup_runs_local_migration_before_config_store_initialize() {
    use vosu_sys::platform::BuildChannel;

    const OSU_SCHEMA: &str = r#"[
        {"uniqueID":"CALCULATE_PP","type":"checkbox","title":"Calculate pp","value":true},
        {"uniqueID":"POLL_RATE","type":"number","title":"Poll rate","value":150.0},
        {"uniqueID":"PRECISE_DATA_POLL_RATE","type":"number","title":"Precise data poll rate","value":10.0},
        {"uniqueID":"SHOW_MP_COMMANDS","type":"checkbox","title":"Show multiplayer commands","value":false},
        {"uniqueID":"READ_MANIA_SCROLL_SPEED","type":"checkbox","title":"Read mania scroll speed","value":true}
    ]"#;
    const OVERLAY_SCHEMA: &str =
        r#"[{"uniqueID":"static_folder_path","type":"text","title":"Overlay folder","value":""}]"#;
    fn dummy_vtable() -> vosu_plugin_api::vosu_plugin_vtable {
        unreachable!("vtable is never invoked by migration schema extraction")
    }
    static TEST_PLUGINS: &[crate::plugin::BundledPlugin<'static>] = &[
        crate::plugin::BundledPlugin {
            manifest: "",
            settings_schema: Some(OSU_SCHEMA),
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: dummy_vtable,
        },
        crate::plugin::BundledPlugin {
            manifest: "",
            settings_schema: Some(OVERLAY_SCHEMA),
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: dummy_vtable,
        },
    ];
    static RELEASE_BUNDLE: Bundle = Bundle {
        version: "2.0.0",
        plugin_host_identity: "vosu/2.0.0/plugin-host-1",
        plugins: TEST_PLUGINS,
        plugin_ids: &["vosu.osu", "vosu.overlayhost"],
        dashboard: "vosu.dashboard",
        tool_host: "vosu.toolhost",
        overlay_root: OverlayRootSetting {
            plugin: "vosu.overlayhost",
            key: "static_folder_path",
        },
        hosts: &FixtureHosts,
    };

    let root = tempfile::tempdir().unwrap();
    let resolver = RuntimeTestPathResolver {
        executable: root.path().join("program/vosu"),
        launch: root.path().to_owned(),
        root: root.path().to_owned(),
    };
    let channel = BuildChannel::from_version("2.0.0").unwrap();
    assert_eq!(channel, BuildChannel::Release);
    let paths = Paths::resolve(&resolver, channel).unwrap();
    paths.initialize().unwrap();
    fs::write(
        paths.config().join("vosu.toml"),
        "SERVER_PORT = 24999.0\nPOLL_RATE = 321.0\nCALCULATE_PP = false\nSTATIC_FOLDER_PATH = \"custom/overlays\"\nV2_SUBSCRIPTION_DEBOUNCE = 9.0\n",
    )
    .unwrap();
    fs::write(paths.config().join("vosu.env"), "SHOW_MP_COMMANDS=true\n").unwrap();

    let (store, settings_store) = bounded!(super::config::initialize_startup_storage(
        &RELEASE_BUNDLE,
        &paths,
        channel
    ))
    .unwrap();
    assert_eq!(store.current().server_port, 24_999.0);
    assert!(!paths.config().join("vosu.env").exists());
    let persisted = fs::read_to_string(paths.config().join("vosu.toml")).unwrap();
    for stripped in [
        "POLL_RATE",
        "CALCULATE_PP",
        "SHOW_MP_COMMANDS",
        "STATIC_FOLDER_PATH",
        "V2_SUBSCRIPTION_DEBOUNCE",
    ] {
        assert!(!persisted.contains(stripped), "{stripped} leaked into vosu.toml");
    }
    let journal = crate::migration::load_journal(paths.config())
        .unwrap()
        .expect("release migration journal written");
    assert!(journal.local_conversion_complete);
    let settings_service = PluginSettingsService::new(settings_store.clone());
    assert_eq!(effective_osu_poll_rates(&settings_service), (321.0, 10.0));
    let osu_overrides = settings_service
        .stored_overrides(crate::migration::OSU_PLUGIN_ID)
        .unwrap();
    assert_eq!(osu_overrides["CALCULATE_PP"], serde_json::json!(false));
    assert_eq!(osu_overrides["SHOW_MP_COMMANDS"], serde_json::json!(true));
    let resolved =
        resolved_overlay_root(&paths, &settings_service, RELEASE_BUNDLE.overlay_root).unwrap();
    assert_eq!(resolved, paths.launch().join("custom/overlays"));
    assert!(resolved.is_dir());
    bounded!(store.shutdown()).unwrap();
}
