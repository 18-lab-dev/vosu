fn state_publication(pid: u32, generation: u64, reset_epoch: u64) -> StatePublication {
    use crate::{
        model::states::HitErrorArray,
        state::{
            BassDensityState, BeatmapPPState, GameplayState, GlobalState,
            LazerMultiSpectatingState, MenuState, PreciseState, RankedPlayState, RegularState,
            ResultScreenState, SettingsState, TourneyManagerState, UserState,
        },
    };

    StatePublication {
        pid,
        generation,
        snapshot: Arc::new(StateBundleSnapshot {
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
            }),
            precise: Arc::new(PreciseState {
                status: 0,
                play_time: 0.0,
                mode: 0,
                key_overlay: Arc::new(Vec::new()),
                keypresses: Arc::new(Vec::new()),
                hit_errors: Arc::new(HitErrorArray::default()),
                total_hit_errors: 0.0,
                committed_hit_error_index: 0,
                unstable_rate: 0.0,
            }),
        }),
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
    assert_eq!(drained[&7].snapshot.reset_epoch, 12);
    assert_eq!(drained[&9].snapshot.reset_epoch, 20);
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
    assert_eq!(drained[&7].snapshot.reset_epoch, 40);
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

