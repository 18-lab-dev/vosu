fn state_bundle(generation: u64, reset_epoch: u64) -> Arc<StateBundleSnapshot> {
    use crate::{
        model::states::HitErrorArray,
        state::{
            BassDensityState, BeatmapPPState, GameplayState, GlobalState,
            LazerMultiSpectatingState, MenuState, PreciseState, RankedPlayState, RegularState,
            ResultScreenState, SettingsState, TourneyManagerState, UserState,
        },
    };

    Arc::new(StateBundleSnapshot {
        source: ClientType::Stable,
        generation,
        reset_epoch,
        regular: Arc::new(RegularState {
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
        }),
        precise: Arc::new(PreciseState { catcher: None, status: 0,
        play_time: 0.0,
        mode: 0,
        key_overlay: Arc::new(Vec::new()),
        keypresses: Arc::new(Vec::new()),
        hit_errors: Arc::new(HitErrorArray::default()),
        total_hit_errors: 0.0,
        committed_hit_error_index: 0,
        unstable_rate: 0.0, }),
    })
}

fn state_publication(pid: u32, generation: u64, reset_epoch: u64) -> StatePublication {
    let state = state_bundle(generation, reset_epoch);
    StatePublication {
        pid,
        generation,
        api: Arc::clone(&state),
        live: state,
    }
}

#[test]
fn publication_queue_coalesces_each_pid_to_its_latest_snapshot() {
    let queue = StatePublicationQueue::default();

    queue.publish(state_publication(7, 1, 10)).unwrap();
    queue.publish(state_publication(7, 1, 11)).unwrap();
    queue.publish(state_publication(7, 1, 12)).unwrap();
    queue.publish(state_publication(9, 4, 20)).unwrap();

    let drained = queue.drain();
    assert_eq!(drained.len(), 2);
    assert_eq!(drained.keys().copied().collect::<Vec<_>>(), vec![7, 9]);
    assert_eq!(drained[&7].live.reset_epoch, 12);
    assert_eq!(drained[&9].live.reset_epoch, 20);
    assert!(queue.drain().is_empty());
}

#[test]
fn publication_queue_never_replaces_a_new_generation_with_late_stale_work() {
    let queue = StatePublicationQueue::default();

    queue.publish(state_publication(7, 3, 30)).unwrap();
    queue.publish(state_publication(7, 4, 40)).unwrap();
    queue.publish(state_publication(7, 3, 31)).unwrap();

    let drained = queue.drain();
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[&7].generation, 4);
    assert_eq!(drained[&7].live.reset_epoch, 40);
}

#[test]
fn equivalent_state_publication_preserves_revision_and_snapshot() {
    let first = state_bundle(1, 10);
    let equivalent = Arc::new(StateBundleSnapshot::clone(&first));
    let mut states = BTreeMap::new();
    let mut revisions = BTreeMap::new();

    update_state_publication(&mut states, &mut revisions, 7, Arc::clone(&first));
    update_state_publication(&mut states, &mut revisions, 7, equivalent);
    assert_eq!(revisions[&7], 1);
    assert!(Arc::ptr_eq(&states[&7], &first));

    update_state_publication(&mut states, &mut revisions, 7, state_bundle(1, 10));
    assert_eq!(revisions[&7], 2);
}

#[test]
fn publication_sink_suppresses_equivalent_snapshots() {
    let queue = Arc::new(StatePublicationQueue::default());
    let sink = StatePublicationSink::new(7, 1, Arc::clone(&queue));
    let first = state_bundle(1, 10);
    let equivalent = Arc::new(StateBundleSnapshot::clone(&first));

    sink.publish(first, Delivery::All).unwrap();
    assert_eq!(queue.drain().len(), 1);
    sink.publish(equivalent, Delivery::All).unwrap();
    assert!(queue.drain().is_empty());

    sink.publish(state_bundle(1, 10), Delivery::All).unwrap();
    assert_eq!(queue.drain().len(), 1);
}

#[test]
fn subscriber_samples_reach_live_state_before_api_cadence() {
    let queue = Arc::new(StatePublicationQueue::default());
    let sink = StatePublicationSink::new(7, 1, Arc::clone(&queue));
    let cadence = state_bundle(1, 10);
    sink.publish(Arc::clone(&cadence), Delivery::All).unwrap();
    queue.drain();

    // A subscriber-driven regular sample is live-only.
    let fast = Arc::new(StateBundleSnapshot {
        regular: Arc::clone(&state_bundle(1, 10).regular),
        ..StateBundleSnapshot::clone(&cadence)
    });
    sink.publish(Arc::clone(&fast), Delivery::Live).unwrap();
    let drained = queue.drain();
    assert!(Arc::ptr_eq(&drained[&7].live, &fast));
    assert!(Arc::ptr_eq(&drained[&7].api, &cadence));

    // A due precise sample advances only the API's precise component.
    let precise = Arc::new(StateBundleSnapshot {
        precise: Arc::clone(&state_bundle(1, 10).precise),
        ..StateBundleSnapshot::clone(&fast)
    });
    sink.publish(Arc::clone(&precise), Delivery::Precise)
        .unwrap();
    let api = Arc::clone(&queue.drain()[&7].api);
    assert!(Arc::ptr_eq(&api.regular, &cadence.regular));
    assert!(Arc::ptr_eq(&api.precise, &precise.precise));

    // A reset epoch reaches API readers at once, even between cadence points.
    let reset = state_bundle(1, 11);
    sink.publish(Arc::clone(&reset), Delivery::Live).unwrap();
    let drained = queue.drain();
    assert!(Arc::ptr_eq(&drained[&7].api, &reset));
    assert!(Arc::ptr_eq(&drained[&7].live, &reset));

    // The next API cadence capture catches API readers up.
    let caught_up = state_bundle(1, 11);
    sink.publish(Arc::clone(&caught_up), Delivery::All).unwrap();
    assert!(Arc::ptr_eq(&queue.drain()[&7].api, &caught_up));
}

#[tokio::test]
async fn registry_add_remove_restart_and_complete_join() {
    let starts = Arc::new(Mutex::new(Vec::new()));
    let exits = Arc::new(AtomicUsize::new(0));
    let mut registry = PidWorkerRegistry::default();
    let spawn = |pid, generation, cancel: CancellationToken| {
        starts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((pid, generation));
        let exits = Arc::clone(&exits);
        tokio::spawn(async move {
            cancel.cancelled().await;
            exits.fetch_add(1, Ordering::AcqRel);
        })
    };

    registry
        .reconcile(&BTreeMap::from([(7, 1), (9, 1)]), spawn)
        .await;
    registry
        .reconcile(&BTreeMap::from([(7, 1), (9, 1)]), spawn)
        .await;
    assert_eq!(
        *starts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![(7, 1), (9, 1)],
        "unchanged generations must not create duplicate workers"
    );

    registry.reconcile(&BTreeMap::from([(7, 2)]), spawn).await;
    assert_eq!(
        *starts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![(7, 1), (9, 1), (7, 2)]
    );
    assert_eq!(exits.load(Ordering::Acquire), 2);

    registry.shutdown().await;
    assert!(registry.workers.is_empty());
    assert_eq!(exits.load(Ordering::Acquire), 3);
}

#[tokio::test]
async fn supervisor_shutdown_aborts_a_worker_tree_after_its_deadline() {
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        task_cancel.cancelled().await;
        std::future::pending::<()>().await;
    });
    let supervisor = ProductionStateSupervisor { cancel, task };

    tokio::time::timeout(
        Duration::from_millis(50),
        supervisor.shutdown_with_timeout(Duration::from_millis(5)),
    )
    .await
    .expect("supervisor shutdown deadline must abort a stuck worker tree");
}

#[tokio::test]
async fn five_hub_tasks_join_without_lingering() {
    let provider = Arc::new(SnapshotProviders::new(
        Arc::new(ManagerSnapshot::not_ready(
            0,
            crate::api::ApiRuntimeSettings::default(),
        )),
        Arc::new(ProductionSnapshotEncoder),
    ));
    let hubs = WebSocketHubs::start(provider.clone(), provider, Duration::ZERO);
    hubs.shutdown().await;
}


#[test]
fn memory_permission_denial_keeps_typed_identity_in_state_worker_errors() {
    let denied = StateWorkerError::from(crate::process::MemoryError::PermissionDenied {
        address: 0x1234,
        len: 16,
        message: "operation not permitted".into(),
    });
    assert!(matches!(
        denied,
        StateWorkerError::MemoryPermissionDenied {
            address: 0x1234,
            len: 16,
            ..
        }
    ));

    let fault = StateWorkerError::from(crate::process::MemoryError::Read {
        address: 0x1234,
        len: 16,
        message: "bad address".into(),
    });
    assert!(matches!(fault, StateWorkerError::Capture(_)));
}

#[tokio::test]
async fn permission_denial_logs_once_and_stops_initialization_without_retrying() {
    let cancel = CancellationToken::new();
    let attempts = AtomicUsize::new(0);
    let errors = AtomicUsize::new(0);

    let worker = tokio::time::timeout(
        Duration::from_millis(50),
        initialize_state_worker::<(), _, _, _>(
            &cancel,
            Duration::from_secs(60),
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err(StateWorkerError::MemoryPermissionDenied {
                    address: 0x4000,
                    len: 8,
                    message: "permission denied".into(),
                }))
            },
            |_| {
                errors.fetch_add(1, Ordering::SeqCst);
            },
        ),
    )
    .await
    .expect("permission denial must terminate immediately");

    assert!(worker.is_none());
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(errors.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn ordinary_initialization_errors_keep_retrying_until_cancelled() {
    let cancel = CancellationToken::new();
    let attempts = AtomicUsize::new(0);
    let errors = AtomicUsize::new(0);

    let worker = initialize_state_worker::<(), _, _, _>(
        &cancel,
        Duration::ZERO,
        || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt == 2 {
                cancel.cancel();
            }
            std::future::ready(Err(StateWorkerError::Capture("transient".into())))
        },
        |_| {
            errors.fetch_add(1, Ordering::SeqCst);
        },
    )
    .await;

    assert!(worker.is_none());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(errors.load(Ordering::SeqCst), 0);
}
