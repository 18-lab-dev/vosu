
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
        &vosu_core::updater::UpdateError::Quarantined("unknown target".into())
    ));
    assert!(!recovery_error_is_nonfatal(
        &vosu_core::updater::UpdateError::LiveOwner
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

struct RuntimeTestPathResolver {
    executable: PathBuf,
    launch: PathBuf,
    root: PathBuf,
}

impl PathResolver for RuntimeTestPathResolver {
    fn current_exe(&self) -> io::Result<PathBuf> {
        Ok(self.executable.clone())
    }

    fn launch_dir(&self) -> io::Result<PathBuf> {
        Ok(self.launch.clone())
    }

    fn env_var(&self, name: &str) -> Option<OsString> {
        let directory = match name {
            "HOME" => "home",
            "XDG_CONFIG_HOME" => "config",
            "XDG_DATA_HOME" => "data",
            "XDG_CACHE_HOME" => "cache",
            _ => return None,
        };
        Some(self.root.join(directory).into_os_string())
    }
}

pub(super) fn runtime_test_paths(root: &Path) -> Paths {
    let program = root.join("program");
    let launch = root.join("launch");
    fs::create_dir_all(&program).unwrap();
    fs::create_dir_all(&launch).unwrap();
    let paths = Paths::resolve(&RuntimeTestPathResolver {
        executable: program.join("vosu"),
        launch,
        root: root.to_owned(),
    }, vosu_sys::platform::BuildChannel::Release)
    .unwrap();
    paths.initialize().unwrap();
    paths
}

#[test]
fn runtime_static_root_resolution_matches_paths_static_path_semantics() {
    let root = tempfile::tempdir().unwrap();
    let paths = runtime_test_paths(root.path());
    let mut config = Config::default();

    for configured in ["", "./static", "./StAtIc", ".\\STATIC"] {
        config.static_folder_path = configured.to_owned();
        let resolved = resolved_static_root(&paths, &config).unwrap();
        assert_eq!(resolved, paths.data().join("static"), "{configured:?}");
        assert!(resolved.is_dir(), "{configured:?}");
    }

    config.static_folder_path = "custom/counters".to_owned();
    let resolved = resolved_static_root(&paths, &config).unwrap();
    assert_eq!(resolved, paths.launch().join("custom/counters"));
    assert!(resolved.is_dir());
}

#[test]
fn static_root_reload_creates_target_and_retains_previous_root_on_failure() {
    let root = tempfile::tempdir().unwrap();
    let paths = runtime_test_paths(root.path());
    let initial = paths.launch().join("initial");
    fs::create_dir_all(&initial).unwrap();
    let counters = ReloadableCounterService::production(
        initial,
        paths.config().join("settings"),
        paths.cache().join("downloads"),
    );
    let config = Config {
        static_folder_path: "next/counters".to_owned(),
        ..Config::default()
    };
    reload_counter_static_root(&paths, &config, &counters).unwrap();
    let active = paths.launch().join("next/counters");
    assert_eq!(counters.current_root(), active);
    assert!(active.is_dir());
    fs::write(paths.launch().join("blocked"), "not a directory").unwrap();
    let blocked_config = Config {
        static_folder_path: "blocked/counters".to_owned(),
        ..Config::default()
    };
    assert!(reload_counter_static_root(&paths, &blocked_config, &counters).is_err());
    assert_eq!(counters.current_root(), active);
}

#[tokio::test]
async fn prerelease_startup_and_restart_import_only_once_into_isolated_roots() {
    use vosu_sys::platform::BuildChannel;

    let root = tempfile::tempdir().unwrap();
    let resolver = RuntimeTestPathResolver {
        executable: root.path().join("program/vosu"),
        launch: root.path().to_owned(),
        root: root.path().to_owned(),
    };
    let channel = BuildChannel::from_version(env!("CARGO_PKG_VERSION")).unwrap();
    assert_eq!(channel, BuildChannel::Prerelease);
    let normal = Paths::resolve(&resolver, BuildChannel::Release).unwrap();
    fs::create_dir_all(normal.config()).unwrap();
    let source = normal.config().join("vosu.toml");
    let original = "POLL_RATE = 643.0\nSTATIC_FOLDER_PATH = 'normal-write-sentinel'\nUNKNOWN_SENTINEL = 97\n";
    fs::write(&source, original).unwrap();
    let alpha = Paths::resolve(&resolver, channel).unwrap();
    alpha.initialize().unwrap();
    let store = bounded!(super::config::initialize_startup_config(&alpha, channel)).unwrap();
    assert_eq!(store.current().poll_rate, 643.0);
    let static_root = resolved_static_root(&alpha, &store.current()).unwrap();
    assert_eq!(static_root, alpha.data().join("static"));
    assert_ne!(static_root, normal.data().join("static"));
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    bounded!(store.shutdown()).unwrap();
    fs::write(&source, "POLL_RATE = [malformed after import").unwrap();
    let restarted = bounded!(super::config::initialize_startup_config(&alpha, channel)).unwrap();
    assert_eq!(restarted.current().poll_rate, 643.0);
    assert_eq!(
        fs::read_to_string(&source).unwrap(),
        "POLL_RATE = [malformed after import"
    );
    bounded!(restarted.shutdown()).unwrap();
}
#[test]
fn prerelease_bundled_manifest_accepts_the_executable_version() {
    let manifest = vosu_plugin_api::manifest::PackageManifest::parse(include_str!(
        "../../../crates/vosu-osu/manifest.toml"
    )).and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
    .unwrap();
    let host = semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    let requirement = semver::VersionReq::parse(&manifest.host).unwrap();
    assert!(requirement.matches(&host));
    assert!(
        !semver::VersionReq::parse(">=1.2.0, <3.0.0")
            .unwrap()
            .matches(&host)
    );
}
