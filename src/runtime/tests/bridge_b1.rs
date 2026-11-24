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


#[tokio::test]
async fn live_selection_promotes_file_and_checksum_before_api_cadence() {
    use crate::server::service::CurrentBeatmapProvider;
    use crate::server::v2_subscription::V2SubscriptionProvider;
    use md5::{Digest, Md5};
    let md5 = |bytes: &[u8]| Md5::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>();

    let root = tempfile::tempdir().unwrap();
    use vosu_osu::bridge_b1::{B1Event, B1Publisher, B1SessionKey};
    use vosu_surface::LegacySink;
    let logger = Logger::new(root.path(), LoggerOptions::default(), Arc::new(StdoutConsole)).unwrap();
    let (_config, config) = watch::channel(Arc::new(Config::default()));
    let (_demand, demand) = watch::channel(Default::default());
    let providers = Arc::new(SnapshotProviders::new(
        Arc::new(ManagerSnapshot::not_ready(0, ApiRuntimeSettings::default())),
        Arc::new(ProductionSnapshotEncoder),
    ));
    let files = RuntimeBeatmap {
        snapshots: providers.snapshot_manager(),
        counters: Arc::new(ReloadableCounterService::production(
            root.path().join("static"), root.path().join("settings"), root.path().join("cache"),
        )),
    };
    let bridge = LegacyBridge::new(config, demand, providers.clone(), logger.clone());
    let key = B1SessionKey { pid: 7, generation: 1 };
    bridge.send(B1Event::Attached {
        key, client: ClientType::Lazer,
        settings: vosu_osu_model::launch::InstanceSettings {
            tournament_spectator: false, tournament_ipc_id: None, devserver_endpoint: None,
        },
        is_tournament_manager: false,
    });
    let sink = B1Publisher::new(key, bridge.clone());
    let (_foreground, foreground_rx) = watch::channel(None);
    let task = bridge.start(foreground_rx);
    let mut previous = None;
    for (_revision, bytes, delivery) in [
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
        let state = Arc::new(state);
        sink.publish(state.clone(), delivery).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let api = providers.load();
                let subscription = V2SubscriptionProvider::capture(providers.as_ref()).document(&[&[]]).unwrap();
                if api.focused().is_some_and(|s| s.regular.menu.checksum.as_deref() == Some(hash.as_str()))
                    && subscription.as_ref().is_some_and(|s| s["beatmap"]["checksum"] == hash) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert!(Arc::ptr_eq(providers.load().focused().unwrap(), &state));
        let subscription = V2SubscriptionProvider::capture(providers.as_ref()).document(&[&[]]).unwrap().unwrap();
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
    tokio::time::timeout(Duration::from_secs(2), task.shutdown()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), files.counters.shutdown()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), logger.shutdown()).await.unwrap().unwrap();
}
#[tokio::test]
async fn dashboard_attachment_status_uses_existing_roster_and_published_version() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(ConfigStore::initialize(root.path().join("vosu.toml")).await.unwrap());
    let snapshots = Arc::new(SnapshotManager::new(Arc::new(ManagerSnapshot::not_ready(0, ApiRuntimeSettings::default()))));
    let provider = RuntimeReportInput {
        config: config.clone(), snapshots: snapshots.clone(),
        counters: Arc::new(ReloadableCounterService::production(root.path().join("static"), root.path().join("settings"), root.path().join("cache"))),
        logs: root.path().join("logs"), lifecycle: Arc::default(),
    };
    assert_eq!(provider.dashboard_status()["instances"], serde_json::json!([]));
    let mut state = (*state_bundle(3, 0)).clone();
    Arc::make_mut(&mut Arc::make_mut(&mut state.regular).settings.values).insert("client".into(), serde_json::json!({"version": "2026.1005.0"}));
    snapshots.publish(Arc::new(ManagerSnapshot::new(1, Some(42), false, ApiRuntimeSettings::default(), [
        ManagerInstanceSnapshot::new(42, vosu_osu_model::launch::InstanceSettings { tournament_spectator: false, tournament_ipc_id: None, devserver_endpoint: None }, false, Arc::new(state))
    ])));
    let status = provider.dashboard_status();
    assert_eq!(status["instances"][0]["pid"], 42);
    assert_eq!(status["instances"][0]["client"], "stable");
    assert_eq!(status["instances"][0]["version"], "2026.1005.0");
    assert_eq!(status["instances"][0]["state"], "reading");
    for (phase, expected) in [
        (vosu_osu::bridge_b1::B1Phase::ResolvingOffsets, "resolving offsets"),
        (vosu_osu::bridge_b1::B1Phase::StartingReader, "starting reader"),
        (vosu_osu::bridge_b1::B1Phase::Reading, "reading"),
    ] {
        let mut session = b1_session(21301, ClientType::Lazer, false);
        session.phase = phase;
        let (_, receiver) = watch::channel(Arc::new(B1Lifecycle { revision: 1, sessions: [session].into(), focus: Default::default() }));
        provider.lifecycle.store(Some(Arc::new(receiver)));
        assert_eq!(provider.dashboard_status()["instances"][0]["state"], expected);
    }
    let (_, lifecycle) = tokio::sync::watch::channel(Arc::new(B1Lifecycle {
        revision: 2, sessions: Arc::from([]), focus: Default::default(),
    }));
    provider.lifecycle.store(Some(Arc::new(lifecycle)));
    assert_eq!(provider.dashboard_status()["instances"], serde_json::json!([]), "exited instances must not linger through a stale published state");
    tokio::time::timeout(Duration::from_secs(2), config.shutdown()).await.unwrap().unwrap();
}
#[test]
fn non_focused_instance_generation_invalidates_manager_revision() {
    let mut revisions = SemanticRevision::default();
    let initial = ManagerSemantics {
        focused_pid: Some(7),
        game_focused: true,
        enable_key_overlay: true,
        enable_mania_key_hit_errors: false,
        enable_hit_offsets: false,
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

fn b1_session(pid: u32, client: ClientType, manager: bool) -> B1SessionView {
    B1SessionView { pid, generation: 1, client, settings: vosu_osu_model::launch::InstanceSettings { tournament_spectator: true, tournament_ipc_id: Some(12.0), devserver_endpoint: None }, phase: vosu_osu::bridge_b1::B1Phase::Reading, is_tournament_manager: manager }
}
#[test]
fn b1_tournament_precedence_lowest_pid_and_absent_client() {
    let mut lifecycle = B1Lifecycle { revision: 1, sessions: [b1_session(21001, ClientType::Stable, false), b1_session(21002, ClientType::Stable, false), b1_session(21003, ClientType::Lazer, true)].into(), focus: FocusState { focused_client: ClientType::Stable, game_focused: true } };
    let config = Config::default();
    let states = lifecycle.sessions.iter().map(|s| (s.pid, state_bundle(1, 0))).collect();
    let revisions = BTreeMap::new();
    let select = |life: &B1Lifecycle| build_manager_snapshot(&mut SemanticRevision::default(), life, &config, &states, &revisions).unwrap().focused_pid();
    assert_eq!(select(&lifecycle), Some(21003));
    Arc::make_mut(&mut lifecycle.sessions)[2].is_tournament_manager = false;
    assert_eq!(select(&lifecycle), Some(21001));
    lifecycle.sessions = lifecycle.sessions[..2].into();
    lifecycle.focus.focused_client = ClientType::Lazer;
    assert_eq!(select(&lifecycle), None);
}
#[test]
fn b1_foreground_exact_and_unavailable_focus_fallback() {
    let sessions = [(21101, ClientType::Stable, false), (21102, ClientType::Lazer, false)];
    assert_eq!(reduce_foreground(sessions, Some(21102)), FocusState { focused_client: ClientType::Lazer, game_focused: true });
    assert_eq!(reduce_foreground(sessions, None), FocusState { focused_client: ClientType::Stable, game_focused: false });
    assert_eq!(reduce_foreground(sessions, Some(99999)), reduce_foreground(sessions, None));
    assert_eq!(reduce_foreground([(21101, ClientType::Stable, false), (21102, ClientType::Lazer, true)], None).focused_client, ClientType::Lazer);
}
#[tokio::test]
async fn b1_lifecycle_current_order_semantics_inputs_and_joined_shutdown() {
    use vosu_osu::bridge_b1::{B1Event, B1SessionKey, B1Phase};
    use vosu_surface::LegacySink;
    let root = tempfile::tempdir().unwrap();
    let logger = Logger::new(root.path(), LoggerOptions::default(), Arc::new(StdoutConsole)).unwrap();
    let (config, config_rx) = watch::channel(Arc::new(Config::default()));
    let (demand, demand_rx) = watch::channel(Default::default());
    let snapshots = Arc::new(SnapshotProviders::new(Arc::new(ManagerSnapshot::not_ready(0, ApiRuntimeSettings::default())), Arc::new(ProductionSnapshotEncoder)));
    let bridge = LegacyBridge::new(config_rx, demand_rx, snapshots.clone(), logger);
    let attach = |pid, generation| B1Event::Attached { key: B1SessionKey { pid, generation }, client: ClientType::Stable, settings: b1_session(pid, ClientType::Stable, false).settings, is_tournament_manager: false };
    bridge.send(attach(21202, 1)); bridge.send(attach(21201, 1));
    let lifecycle = bridge.lifecycle();
    assert_eq!(lifecycle.borrow().sessions.iter().map(|s| s.pid).collect::<Vec<_>>(), [21201, 21202]);
    assert_eq!(lifecycle.borrow().revision, 2);
    bridge.send(attach(21201, 1));
    bridge.send(B1Event::Phase { key: B1SessionKey { pid: 21201, generation: 1 }, phase: B1Phase::ResolvingOffsets });
    assert_eq!(lifecycle.borrow().revision, 2);
    // Leave old work queued before PID reuse; the manager must drain it
    // against the new attachment rather than resurrect the old generation.
    bridge.send(B1Event::Publish { key: B1SessionKey { pid: 21201, generation: 1 }, api: state_bundle(1, 99), live: state_bundle(1, 99) });
    bridge.send(attach(21201, 2));
    bridge.send(B1Event::Detached { key: B1SessionKey { pid: 21201, generation: 1 } });
    assert_eq!(lifecycle.borrow().revision, 3);
    assert_eq!(lifecycle.borrow().sessions[0].generation, 2);
    let mut inputs = bridge.inputs();
    let (_foreground, foreground_rx) = watch::channel(None);
    let task = bridge.start(foreground_rx);
    let mut next = Config::default(); next.enable_key_overlay = !next.enable_key_overlay;
    next.enable_mania_key_hit_errors = !next.enable_mania_key_hit_errors;
    next.enable_hit_offsets = !next.enable_hit_offsets;
    next.v2_subscription_debounce = 73.0;
    next.api_v1_subscription_debounce = 31.25;
    config.send(Arc::new(next.clone())).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while inputs.borrow_and_update().enable_key_overlay != next.enable_key_overlay { inputs.changed().await.unwrap(); }
    }).await.unwrap();
    assert_eq!(inputs.borrow().enable_mania_key_hit_errors, next.enable_mania_key_hit_errors);
    assert_eq!(inputs.borrow().enable_hit_offsets, next.enable_hit_offsets);
    assert_eq!(inputs.borrow().v2_subscription_debounce, 73.0);
    demand.send(vosu_osu_state::SubscriptionDemand { precise: true, ..Default::default() }).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !inputs.borrow_and_update().demand.precise { inputs.changed().await.unwrap(); }
    }).await.unwrap();
    assert!(!snapshots.load().manager().instances().contains_key(&21201), "queued old-generation work must not become visible after PID reuse");
    let state = state_bundle(2, 12);
    bridge.send(B1Event::Publish { key: B1SessionKey { pid: 21201, generation: 2 }, api: state.clone(), live: state });
    tokio::time::timeout(Duration::from_secs(2), async {
        while snapshots.load().manager().instances().get(&21201).is_none_or(|entry| entry.state().generation != 2 || entry.state().reset_epoch != 12) { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(snapshots.load().manager().instances()[&21201].state().generation, 2, "valid new-generation work must be observed before shutdown");
    tokio::time::timeout(Duration::from_secs(2), task.shutdown()).await.unwrap();
    assert!(lifecycle.borrow().sessions.is_empty());
    assert_eq!(snapshots.load().manager().instances()[&21201].state().generation, 2);
    bridge.send(attach(21203, 3));
    assert!(lifecycle.borrow().sessions.is_empty(), "closed bridge must reject later work");
    let mut lifecycle = lifecycle;
    lifecycle.borrow_and_update();
    drop(bridge);
    assert!(tokio::time::timeout(Duration::from_secs(2), lifecycle.changed()).await.unwrap().is_err());
}

#[tokio::test]
async fn b1_shared_foreground_and_detach_update_lifecycle_and_manager() {
    use vosu_osu::bridge_b1::{B1Event, B1SessionKey};
    use vosu_surface::LegacySink;
    let root = tempfile::tempdir().unwrap();
    let logger = Logger::new(root.path(), LoggerOptions::default(), Arc::new(StdoutConsole)).unwrap();
    let (_config, config_rx) = watch::channel(Arc::new(Config::default()));
    let (_demand, demand_rx) = watch::channel(Default::default());
    let snapshots = Arc::new(SnapshotProviders::new(Arc::new(ManagerSnapshot::not_ready(0, ApiRuntimeSettings::default())), Arc::new(ProductionSnapshotEncoder)));
    let bridge = LegacyBridge::new(config_rx, demand_rx, snapshots.clone(), logger);
    for (pid, client) in [(21401, ClientType::Lazer), (21402, ClientType::Stable)] {
        let key = B1SessionKey { pid, generation: 1 };
        bridge.send(B1Event::Attached { key, client, settings: b1_session(pid, client, false).settings, is_tournament_manager: false });
        let mut state = (*state_bundle(1, 0)).clone();
        state.source = client;
        let state = Arc::new(state);
        bridge.send(B1Event::Publish { key, api: state.clone(), live: state });
    }
    let (foreground, foreground_rx) = watch::channel(None);
    let lifecycle = bridge.lifecycle();
    let task = bridge.start(foreground_rx);
    tokio::time::timeout(Duration::from_secs(2), async {
        while snapshots.load().manager().focused_pid() != Some(21401) { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(lifecycle.borrow().focus, FocusState { focused_client: ClientType::Lazer, game_focused: false });
    let (detached, remaining, fallback_client) = {
        foreground.send(Some(21402)).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while lifecycle.borrow().focus != (FocusState { focused_client: ClientType::Stable, game_focused: true })
                || snapshots.load().manager().focused_pid() != Some(21402) {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        assert_eq!(lifecycle.borrow().focus, FocusState { focused_client: ClientType::Stable, game_focused: true });
        assert_eq!(snapshots.load().manager().focused_pid(), Some(21402));
        (21402, 21401, ClientType::Lazer)
    };
    bridge.send(B1Event::Detached { key: B1SessionKey { pid: detached, generation: 1 } });
    tokio::time::timeout(Duration::from_secs(2), async {
        while lifecycle.borrow().focus != (FocusState { focused_client: fallback_client, game_focused: false })
            || snapshots.load().manager().focused_pid() != Some(remaining) {
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(lifecycle.borrow().focus, FocusState { focused_client: fallback_client, game_focused: false });
    assert_eq!(snapshots.load().manager().focused_pid(), Some(remaining));
    assert!(!snapshots.load().manager().instances().contains_key(&detached));
    // The OS owner is unchanged: a new attachment must consume the cached
    // sample without another poll or watch notification.
    let key = B1SessionKey { pid: detached, generation: 2 };
    bridge.send(B1Event::Attached { key, client: ClientType::Stable, settings: b1_session(detached, ClientType::Stable, false).settings, is_tournament_manager: false });
    let mut state = (*state_bundle(2, 0)).clone();
    state.source = ClientType::Stable;
    let state = Arc::new(state);
    bridge.send(B1Event::Publish { key, api: state.clone(), live: state });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !lifecycle.borrow().focus.game_focused || snapshots.load().manager().focused_pid() != Some(detached) {
            tokio::task::yield_now().await;
        }
    }).await.unwrap();
    assert_eq!(lifecycle.borrow().focus, FocusState { focused_client: ClientType::Stable, game_focused: true });
    assert_eq!(snapshots.load().manager().focused_pid(), Some(detached));
    tokio::time::timeout(Duration::from_secs(2), task.shutdown()).await.unwrap();
}
