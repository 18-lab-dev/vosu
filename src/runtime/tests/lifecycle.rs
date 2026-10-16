#[test]
fn precise_publication_wakes_native_subscription_without_waking_regular_stream() {
    use crate::{
        instance::InstanceSettings,
        model::states::{HitErrorArray, KeypressButton},
        state::{
            BassDensityState, BeatmapPPState, GameplayState, GlobalState,
            LazerMultiSpectatingState, MenuState, PreciseState, RankedPlayState, RegularState,
            ResultScreenState, SettingsState, TourneyManagerState, UserState,
        },
    };

    let regular = Arc::new(RegularState {
        global: GlobalState::default(),
        settings: SettingsState::default(),
        beatmap: BeatmapPPState::default(),
        menu: MenuState::default(),
        bass_density: BassDensityState::default(),
        gameplay: GameplayState::default(),
        result_screen: ResultScreenState::default(),
        tourney: TourneyManagerState::default(),
        user: UserState::default(),
        lazer_multi_spectating: LazerMultiSpectatingState::default(),
        ranked_play: RankedPlayState::default(),
        room: Default::default(),
    });
    let precise = |play_time| {
        Arc::new(PreciseState { catcher: None, status: 2,
        play_time,
        mode: 0,
        key_overlay: Arc::new(Vec::new()),
        keypresses: Arc::new(vec![KeypressButton {
            name: "K1".into(),
            is_pressed: true,
            count: 7,
            pressed_at: Some(play_time),
            released_at: None,
        }]),
        hit_errors: Arc::new(HitErrorArray::default()),
        total_hit_errors: 0.0,
        committed_hit_error_index: 0,
        unstable_rate: 0.0, })
    };
    let manager = |revision, precise| {
        let state = Arc::new(StateBundleSnapshot {
            source: ClientType::Stable,
            generation: 1,
            reset_epoch: 0,
            regular: regular.clone(),
            precise,
        });
        Arc::new(ManagerSnapshot::new(
            revision,
            Some(7),
            true,
            ApiRuntimeSettings::default(),
            [ManagerInstanceSnapshot::new(
                7,
                InstanceSettings {
                    tournament_spectator: false,
                    tournament_ipc_id: None,
                    devserver_endpoint: None,
                },
                false,
                state,
            )],
        ))
    };
    let providers = SnapshotProviders::new(
        manager(1, precise(1.0)),
        Arc::new(ProductionSnapshotEncoder),
    );
    let regular_rx = WebSocketStateProvider::subscribe(&providers, WebSocketSchema::V2);
    let precise_rx = WebSocketStateProvider::subscribe(&providers, WebSocketSchema::V2Precise);
    providers.publish(manager(2, precise(2.0)));
    assert!(!regular_rx.has_changed().unwrap());
    assert!(precise_rx.has_changed().unwrap());
    let native = V2SubscriptionProvider::capture(&providers)
        .document()
        .unwrap()
        .unwrap();
    assert_eq!(
        native["keys"]["all"],
        serde_json::json!([{
            "name": "K1",
            "isPressed": true,
            "count": 7,
            "pressedAt": 2,
            "releasedAt": null
        }])
    );
    assert_eq!(
        native["play"]["mode"],
        serde_json::json!({"number": 0, "name": "osu"})
    );
}

#[test]
fn v2_subscription_capture_keeps_not_ready_revision_and_missing_document() {
    let providers = SnapshotProviders::new(
        Arc::new(ManagerSnapshot::not_ready(
            17,
            ApiRuntimeSettings::default(),
        )),
        Arc::new(ProductionSnapshotEncoder),
    );
    let snapshot = V2SubscriptionProvider::capture(&providers);
    assert_eq!(snapshot.revision(), 17);
    assert_eq!(snapshot.document().unwrap(), None);
}
#[tokio::test]
async fn signal_completion_cancels_the_shared_runtime_token() {
    let shutdown = CancellationToken::new();
    let observed = shutdown.clone();
    let (send, receive) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(cancel_on_signal(shutdown, async move {
        receive
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "signal test closed"))
    }));

    send.send(()).expect("deliver synthetic signal");
    task.await
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
    let result = finish_runtime_run(Err(io::Error::other("run failed")), async move {
        called.store(true, Ordering::Release);
        Ok(())
    })
    .await;
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
        desired_rx.changed().await.unwrap();
        let requested = desired_rx.borrow().clone();
        state_tx.send_replace(ListenerState {
            desired: Some(requested),
            effective: Some(crate::server::runtime::EffectiveListener {
                target: retained,
                address: "127.0.0.1:24050".parse().unwrap(),
            }),
            last_error: Some("bind failed".into()),
        });
        task_cancel.cancelled().await;
    });
    let mut owner = ListenerOwner {
        desired,
        state,
        cancel,
        task,
        broker: None,
        broker_logger: None,
    };
    assert!(owner.stage(replacement.clone()).await.is_err());
    let state = owner.state();
    assert_eq!(state.desired, Some(replacement));
    assert_eq!(state.effective.unwrap().target, old_target);
    owner
        .stage(old_target.clone())
        .await
        .expect("rollback to effective target");
    assert_eq!(
        owner.desired.borrow().clone(),
        old_target,
        "rollback must replace the failed desired target even when it is already effective"
    );
    owner.shutdown().await;
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
    let task = tokio::spawn(async move { task_cancel.cancelled().await; });
    let mut owner = ListenerOwner {
        desired, state, cancel, task, broker: Some(broker.clone()), broker_logger: None,
    };
    owner.stage(target.clone()).await.expect("live rebind must remain successful");
    assert_eq!(owner.state().effective.unwrap().target, target);
    assert_eq!(broker.authorize(&headers, peer), Err(()));
    owner.shutdown().await;
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
        task_cancel.cancelled().await;
        task_events.send("accept-stopped").unwrap();
        task_release.notified().await;
        task_events.send("listener-joined").unwrap();
    });
    let owner = ListenerOwner {
        desired,
        state,
        cancel,
        task,
        broker: None,
        broker_logger: None,
    };
    owner.stop_accepting();
    assert_eq!(event_rx.recv().await, Some("accept-stopped"));
    events.send("hubs-closed-1001").unwrap();
    release.notify_one();
    owner.join().await;
    assert_eq!(event_rx.recv().await, Some("hubs-closed-1001"));
    assert_eq!(event_rx.recv().await, Some("listener-joined"));
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

fn runtime_test_paths(root: &Path) -> Paths {
    let program = root.join("program");
    let launch = root.join("launch");
    fs::create_dir_all(&program).unwrap();
    fs::create_dir_all(&launch).unwrap();
    Paths::resolve(&RuntimeTestPathResolver {
        executable: program.join("vosu"),
        launch,
        root: root.to_owned(),
    })
    .unwrap()
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
