use super::*;
use crate::bridge_b1::{B1Event, B1Inputs, B1Phase, B1Sink};
use vosu_osu_reader::process::{BatchScanResult, Pattern, ScanBackend};

struct ReadyScans;
impl ScanBackend for ReadyScans {
    fn scan_all(&self, _: &Pattern) -> Result<Vec<usize>, MemoryError> {
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
    OsuSession::new(
        u64::from(pid),
        pid,
        37,
        client,
        ProcessHandle::with_scan_backend(
            FakeMemory::new(true),
            ProcessBitness::X86,
            Arc::new(ReadyScans),
        ),
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
            phase: B1Phase::StartingReader,
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
