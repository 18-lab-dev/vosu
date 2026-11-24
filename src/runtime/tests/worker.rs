fn state_bundle(generation: u64, reset_epoch: u64) -> Arc<StateBundleSnapshot> {
    use vosu_osu_state::{
        BassDensityState, BeatmapPPState, GameplayState, GlobalState,
        LazerMultiSpectatingState, MenuState, PreciseState, RankedPlayState, RegularState,
        ResultScreenState, SettingsState, TourneyManagerState, UserState,
    };
    use vosu_osu_model::states::HitErrorArray;

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
        [ManagerInstanceSnapshot::new(7, vosu_osu_model::launch::InstanceSettings {
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
            bounded!(cancel.cancelled());
            exits.fetch_add(1, Ordering::AcqRel);
        })
    };

    bounded!(registry
        .reconcile(&BTreeMap::from([(7, 1), (9, 1)]), spawn));
    bounded!(registry
        .reconcile(&BTreeMap::from([(7, 1), (9, 1)]), spawn));
    assert_eq!(
        *starts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![(7, 1), (9, 1)],
        "unchanged generations must not create duplicate workers"
    );

    bounded!(registry.reconcile(&BTreeMap::from([(7, 2)]), spawn));
    assert_eq!(
        *starts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        vec![(7, 1), (9, 1), (7, 2)]
    );
    assert_eq!(exits.load(Ordering::Acquire), 2);

    bounded!(registry.shutdown());
    assert!(registry.workers.is_empty());
    assert_eq!(exits.load(Ordering::Acquire), 3);
}

#[tokio::test]
async fn supervisor_shutdown_aborts_a_worker_tree_after_its_deadline() {
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        bounded!(task_cancel.cancelled());
        bounded!(std::future::pending::<()>());
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
    bounded!(hubs.shutdown());
}


