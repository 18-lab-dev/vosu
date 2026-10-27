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

#[test]
fn live_selection_promotes_file_and_checksum_before_api_cadence() {
    use crate::server::service::CurrentBeatmapProvider;
    use crate::server::v2_subscription::V2SubscriptionProvider;
    use md5::{Digest, Md5};
    let md5 = |bytes: &[u8]| Md5::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>();

    let root = tempfile::tempdir().unwrap();
    let queue = Arc::new(StatePublicationQueue::default());
    let sink = StatePublicationSink::new(7, 1, Arc::clone(&queue));
    let providers = SnapshotProviders::new(
        Arc::new(ManagerSnapshot::not_ready(0, ApiRuntimeSettings::default())),
        Arc::new(ProductionSnapshotEncoder),
    );
    let files = RuntimeBeatmap {
        snapshots: providers.snapshot_manager(),
        counters: Arc::new(ReloadableCounterService::production(
            root.path().join("static"), root.path().join("settings"), root.path().join("cache"),
        )),
    };
    let manager = |revision, state| Arc::new(ManagerSnapshot::new(
        revision, Some(7), false, ApiRuntimeSettings::default(),
        [ManagerInstanceSnapshot::new(7, crate::instance::InstanceSettings {
            tournament_spectator: false, tournament_ipc_id: None, devserver_endpoint: None,
        }, false, state)],
    ));
    let mut previous = None;
    for (revision, bytes, delivery) in [
        (1, b"osu file format v14\n[Metadata]\nTitle:Old\n".as_slice(), Delivery::All),
        (2, b"osu file format v14\n[Metadata]\nTitle:New\n".as_slice(), Delivery::Live),
    ] {
        let hash = md5(bytes);
        let relative = PathBuf::from(&hash[..1]).join(&hash[..2]).join(&hash);
        let path = root.path().join(&relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        let mut state = (*state_bundle(1, 10)).clone();
        state.source = ClientType::Lazer;
        let regular = Arc::make_mut(&mut state.regular);
        regular.global.songs_folder = root.path().to_string_lossy().into_owned();
        regular.menu.folder = Some(String::new());
        regular.menu.filename = Some(relative.to_string_lossy().into_owned());
        regular.menu.checksum = Some(hash.clone());
        sink.publish(Arc::new(state), delivery).unwrap();
        let publication = queue.drain().remove(&7).unwrap();
        assert!(Arc::ptr_eq(&publication.api, &publication.live));
        // Production publishes API before live, and only then wakes subscribers.
        providers.publish_api(manager(revision, publication.api));
        providers.publish_live(manager(revision, publication.live));
        let subscription = V2SubscriptionProvider::capture(&providers).document(&[&[]]).unwrap().unwrap();
        assert_eq!(subscription["beatmap"]["checksum"], hash);
        assert_eq!(files.tool_context()["beatmap"]["checksum"], hash);
        let actual = files.current_beatmap_bytes().unwrap().unwrap();
        assert_eq!(actual.as_ref(), bytes);
        assert_eq!(md5(&actual), hash);
        assert_eq!(providers.load().focused().unwrap().regular.menu.checksum.as_deref(), Some(hash.as_str()));
        if let Some(old) = previous {
            assert_ne!(old, hash);
        }
        previous = Some(hash);
    }
}

#[test]
fn selection_resource_changes_bypass_both_live_and_precise_api_cadences() {
    let previous = state_bundle(1, 10);
    for field in 0..6 {
        let mut next = (*previous).clone();
        let regular = Arc::make_mut(&mut next.regular);
        match field {
            0 => regular.menu.checksum = Some("new checksum".into()),
            1 => regular.menu.filename = Some("new file".into()),
            2 => regular.menu.folder = Some("new folder".into()),
            3 => regular.menu.audio_filename = Some("new audio".into()),
            4 => regular.menu.background_filename = Some("new background".into()),
            5 => regular.global.songs_folder = "new songs root".into(),
            _ => unreachable!(),
        }
        let next = Arc::new(next);
        for delivery in [Delivery::Live, Delivery::Precise] {
            assert!(Arc::ptr_eq(&api_state(Some(&previous), &next, delivery), &next));
        }
    }
    let mut ordinary = (*previous).clone();
    Arc::make_mut(&mut ordinary.regular).menu.title = Some("new title only".into());
    assert!(Arc::ptr_eq(&api_state(Some(&previous), &Arc::new(ordinary), Delivery::Live), &previous));
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
