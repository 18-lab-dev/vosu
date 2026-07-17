#[test]
fn precise_only_publication_does_not_wake_regular_subscribers() {
    use crate::{
        instance::InstanceSettings,
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
    });
    let precise = |play_time| {
        Arc::new(PreciseState {
            status: 2,
            play_time,
            key_overlay: Arc::new(Vec::new()),
            hit_errors: Arc::new(Vec::new()),
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 0.0,
        })
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
    let precise_rx =
        WebSocketStateProvider::subscribe(&providers, WebSocketSchema::V2Precise);
    providers.publish(manager(2, precise(2.0)));
    assert!(!regular_rx.has_changed().unwrap());
    assert!(precise_rx.has_changed().unwrap());
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
        readiness_token: "readiness".to_owned(),
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
    };
    owner.stop_accepting();
    assert_eq!(event_rx.recv().await, Some("accept-stopped"));
    events.send("hubs-closed-1001").unwrap();
    release.notify_one();
    owner.join().await;
    assert_eq!(event_rx.recv().await, Some("hubs-closed-1001"));
    assert_eq!(event_rx.recv().await, Some("listener-joined"));
}
