use super::*;
use crate::bridge_b1::{B1Event, B1Inputs, B1Phase, B1Sink};
use vosu_osu_reader::process::{BatchScanResult, Pattern, ScanBackend};

#[derive(Default)]
struct ReadyScans {
    started: Option<Arc<tokio::sync::Notify>>,
}
impl ScanBackend for ReadyScans {
    fn scan_all(&self, _: &Pattern) -> Result<Vec<usize>, MemoryError> {
        if let Some(started) = &self.started {
            started.notify_one();
        }
        Ok(vec![0x4000])
    }
    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        Ok(patterns
            .iter()
            .enumerate()
            .map(|(index, _)| BatchScanResult {
                index,
                address: 0x4000 + index * 0x100,
            })
            .collect())
    }
}
struct Sink {
    events: tokio::sync::mpsc::Sender<B1Event>,
    inputs: tokio::sync::watch::Sender<B1Inputs>,
}
impl vosu_surface::LegacySink<B1Event, B1Inputs> for Sink {
    fn send(&self, event: B1Event) {
        self.events
            .try_send(event)
            .expect("bounded test sink overflow");
    }
    fn inputs(&self) -> tokio::sync::watch::Receiver<B1Inputs> {
        self.inputs.subscribe()
    }
}
fn sink() -> (Arc<Sink>, tokio::sync::mpsc::Receiver<B1Event>) {
    let (events, receiver) = tokio::sync::mpsc::channel(32);
    (
        Arc::new(Sink {
            events,
            inputs: tokio::sync::watch::channel(B1Inputs::default()).0,
        }),
        receiver,
    )
}
async fn event(events: &mut tokio::sync::mpsc::Receiver<B1Event>) -> B1Event {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("capture event deadline")
        .expect("capture task closed")
}
fn capture_session(pid: u32, client: ClientType) -> Arc<OsuSession> {
    capture_session_with_scans(pid, client, Arc::new(ReadyScans::default()))
}
fn capture_session_with_scans(
    pid: u32,
    client: ClientType,
    scans: Arc<dyn ScanBackend>,
) -> Arc<OsuSession> {
    OsuSession::new(
        u64::from(pid),
        pid,
        37,
        client,
        ProcessHandle::with_scan_backend(FakeMemory::new(true), ProcessBitness::X86, scans),
        PathBuf::new(),
        PathBuf::new(),
        InstanceSettings {
            tournament_spectator: true,
            tournament_ipc_id: Some(3.0),
            devserver_endpoint: Some("test.invalid".into()),
        },
        None,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stable_capture_attaches_scans_reads_and_detaches_after_join() {
    let session = capture_session(61051, ClientType::Stable);
    let (sink, mut events) = sink();
    let target: Arc<B1Sink> = sink;
    session.start_capture(target.clone(), noop_warn()).await;
    session.start_capture(target, noop_warn()).await;
    match event(&mut events).await {
        B1Event::Attached {
            key,
            client,
            settings,
            is_tournament_manager,
        } => {
            assert_eq!(key.pid, 61051);
            assert_eq!(key.generation, 37);
            assert_eq!(client, ClientType::Stable);
            assert_eq!(settings.tournament_ipc_id, Some(3.0));
            assert!(settings.tournament_spectator);
            assert_eq!(
                settings.devserver_endpoint.as_deref(),
                Some(std::ffi::OsStr::new("test.invalid"))
            );
            assert!(!is_tournament_manager);
        }
        _ => panic!("attach must be first"),
    }
    assert!(matches!(
        event(&mut events).await,
        B1Event::Phase {
            phase: B1Phase::ResolvingOffsets,
            ..
        }
    ));
    assert!(matches!(event(&mut events).await, B1Event::Publish { .. }));
    assert!(matches!(
        event(&mut events).await,
        B1Event::Phase {
            phase: B1Phase::Reading,
            ..
        }
    ));
    session.cancel();
    tokio::time::timeout(Duration::from_secs(5), session.join_resolver())
        .await
        .unwrap();
    let mut detached = false;
    while let Ok(event) = events.try_recv() {
        assert!(!detached, "nothing may be published after Detached");
        match event {
            B1Event::Detached { key } => {
                assert_eq!(key.pid, 61051);
                detached = true;
            }
            B1Event::Publish { .. } => {}
            _ => panic!("capture must start only once"),
        }
    }
    assert!(detached);
    assert!(session.capture.lock().await.is_none());
}

#[tokio::test]
async fn lazer_capture_waits_for_offsets_and_cancel_releases_the_session() {
    let session = capture_session(61052, ClientType::Lazer);
    let (sink, mut events) = sink();
    session.start_capture(sink, noop_warn()).await;
    assert!(matches!(event(&mut events).await, B1Event::Attached { .. }));
    assert!(matches!(
        event(&mut events).await,
        B1Event::Phase {
            phase: B1Phase::ResolvingOffsets,
            ..
        }
    ));
    session.cancel();
    tokio::time::timeout(Duration::from_secs(5), session.join_resolver())
        .await
        .unwrap();
    assert!(matches!(event(&mut events).await, B1Event::Detached { .. }));
    assert!(events.try_recv().is_err());
    let weak = Arc::downgrade(&session);
    drop(session);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn cancelled_session_never_attaches_capture() {
    let session = capture_session(61053, ClientType::Stable);
    let (sink, mut events) = sink();
    session.cancel();
    session.start_capture(sink, noop_warn()).await;
    assert!(session.capture.lock().await.is_none());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn installing_offsets_wakes_capture_without_waiting_for_retry_delay() {
    use vosu_osu_reader::memory::lazer_offsets::LazerOffsets;
    let preparing = Arc::new(tokio::sync::Notify::new());
    let session = capture_session_with_scans(
        61101,
        ClientType::Lazer,
        Arc::new(ReadyScans {
            started: Some(preparing.clone()),
        }),
    );
    let offsets = Arc::new(LazerOffsets::embedded().unwrap());
    let (sink, mut events) = sink();
    session.start_capture(sink, noop_warn()).await;
    assert!(matches!(event(&mut events).await, B1Event::Attached { .. }));
    assert!(matches!(
        event(&mut events).await,
        B1Event::Phase {
            phase: B1Phase::ResolvingOffsets,
            ..
        }
    ));
    // The per-session signal proves capture is waiting for offsets before
    // installation; racing installation against task startup would not test
    // the former one-second retry delay.
    tokio::time::timeout(
        Duration::from_millis(500),
        session.offset_waiting.notified(),
    )
    .await
    .unwrap();
    assert!(session.install_lazer_offsets(offsets));
    let next = tokio::time::timeout(Duration::from_millis(500), events.recv())
        .await
        .expect("offset installation must bypass the one-second retry delay")
        .unwrap();
    assert!(matches!(
        next,
        B1Event::Phase {
            phase: B1Phase::StartingReader,
            ..
        }
    ));
    tokio::time::timeout(Duration::from_millis(500), preparing.notified())
        .await
        .expect("reader preparation must run promptly, not just its phase event");
    session.cancel();
    tokio::time::timeout(Duration::from_secs(5), session.join_resolver())
        .await
        .unwrap();
    assert!(matches!(event(&mut events).await, B1Event::Detached { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_capture_forwards_changed_b1_inputs_and_b2_settings() {
    use crate::settings::OsuSettings;
    use vosu_osu_state::{ReaderSettings, SubscriptionDemand};
    let session = capture_session(61102, ClientType::Stable);
    let (sink, mut events) = sink();
    session.start_capture(sink.clone(), noop_warn()).await;
    assert!(matches!(event(&mut events).await, B1Event::Attached { .. }));
    assert!(matches!(
        event(&mut events).await,
        B1Event::Phase {
            phase: B1Phase::ResolvingOffsets,
            ..
        }
    ));
    assert!(matches!(event(&mut events).await, B1Event::Publish { .. }));
    assert!(matches!(
        event(&mut events).await,
        B1Event::Phase {
            phase: B1Phase::Reading,
            ..
        }
    ));
    // These are clones of the exact receivers supplied to the worker, not
    // copies of the input literals or a second settings conversion.
    let (mut reader, mut demand) = session.capture_controls.borrow().as_ref().unwrap().clone();
    reader.borrow_and_update();
    demand.borrow_and_update();
    let next_demand = SubscriptionDemand {
        regular: true,
        precise: true,
        gameplay_regular: true,
        gameplay_precise: true,
    };
    sink.inputs.send_replace(B1Inputs {
        enable_key_overlay: false,
        enable_mania_key_hit_errors: true,
        enable_hit_offsets: true,
        v2_subscription_debounce: 43.0,
        demand: next_demand,
    });
    tokio::time::timeout(Duration::from_millis(500), reader.changed())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_millis(500), demand.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        *reader.borrow_and_update(),
        ReaderSettings {
            enable_key_overlay: false,
            enable_mania_key_hit_errors: true,
            enable_hit_offsets: true,
            v2_subscription_debounce: 43.0,
            ..ReaderSettings::default()
        }
    );
    assert_eq!(*demand.borrow_and_update(), next_demand);
    session.osu_settings.send_replace(OsuSettings {
        calculate_pp: false,
        poll_rate: 731.0,
        precise_data_poll_rate: 37.0,
        show_mp_commands: true,
        read_mania_scroll_speed: false,
    });
    tokio::time::timeout(Duration::from_millis(500), reader.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        *reader.borrow_and_update(),
        ReaderSettings {
            calculate_pp: false,
            poll_rate: 731.0,
            precise_data_poll_rate: 37.0,
            show_mp_commands: true,
            read_mania_scroll_speed: false,
            enable_key_overlay: false,
            enable_mania_key_hit_errors: true,
            enable_hit_offsets: true,
            v2_subscription_debounce: 43.0,
        }
    );
    assert_eq!(*demand.borrow(), next_demand);
    let final_demand = SubscriptionDemand {
        gameplay_regular: true,
        ..SubscriptionDemand::default()
    };
    sink.inputs.send_modify(|inputs| {
        inputs.demand = final_demand;
        inputs.enable_key_overlay = true;
    });
    tokio::time::timeout(Duration::from_millis(500), demand.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*demand.borrow_and_update(), final_demand);
    assert!(reader.borrow().enable_key_overlay);
    assert_eq!(reader.borrow().poll_rate, 731.0);
    session.cancel();
    tokio::time::timeout(Duration::from_secs(5), session.join_resolver())
        .await
        .unwrap();
    let remaining = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
    assert!(matches!(remaining.last(), Some(B1Event::Detached { .. })));
    assert!(
        remaining[..remaining.len() - 1]
            .iter()
            .all(|event| matches!(event, B1Event::Publish { .. }))
    );
}
