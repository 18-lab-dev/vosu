use super::*;
use vosu_osu_reader::process::ScanBackend;
use vosu_plugin_api::memory::{BatchScanResult, Pattern};

struct PublicationHost;
impl vosu_plugin_api::HostServices for PublicationHost {
    fn send(&self, _: &vosu_plugin_api::Message, _: u64) -> vosu_plugin_api::vosu_status {
        vosu_plugin_api::vosu_status::VOSU_OK
    }
}
#[derive(Default)]
struct ReadyScans {
    started: Option<Arc<tokio::sync::Notify>>,
}
impl ScanBackend for ReadyScans {
    fn scan_all(&self, _: &Pattern) -> Result<Vec<usize>, MemoryError> {
        Ok(vec![0x4000])
    }
    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        if let Some(started) = &self.started {
            started.notify_one();
        }
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
// Lazer prepares scalar patterns after offsets arrive; stable prepares batches.
// Keep their readiness observations separate so neither test accepts the wrong path.
struct LazerPreparationScans(Arc<tokio::sync::Notify>);
impl ScanBackend for LazerPreparationScans {
    fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        self.0.notify_one();
        ReadyScans::default().scan_all(pattern)
    }
    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        ReadyScans::default().scan_batch(patterns)
    }
}
fn capture_session(pid: u32, client: ClientType) -> Arc<OsuSession> {
    capture_session_with_scans(pid, client, Arc::new(ReadyScans::default()))
}
fn capture_session_with_scans(
    pid: u32,
    client: ClientType,
    scans: Arc<dyn ScanBackend>,
) -> Arc<OsuSession> {
    let session = OsuSession::new(
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
    );
    session.configure_publication(Arc::new(PublicationHost));
    session
}

async fn controls(session: &OsuSession) -> super::super::capture::CaptureControls {
    let mut controls = session.capture_controls.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(value) = controls.borrow_and_update().clone() {
                return value;
            }
            controls.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stable_capture_starts_once_and_joins_without_root_controls() {
    let preparing = Arc::new(tokio::sync::Notify::new());
    let session = capture_session_with_scans(
        61051,
        ClientType::Stable,
        Arc::new(ReadyScans {
            started: Some(preparing.clone()),
        }),
    );
    session.start_capture(noop_warn(), noop_warn()).await;
    session.start_capture(noop_warn(), noop_warn()).await;
    tokio::time::timeout(Duration::from_secs(5), preparing.notified())
        .await
        .unwrap();
    let (reader, _) = controls(&session).await;
    assert_eq!(*reader.borrow(), vosu_osu_state::ReaderSettings::default());
    session.cancel();
    tokio::time::timeout(Duration::from_secs(5), session.join_resolver())
        .await
        .unwrap();
    assert!(session.capture.lock().await.is_none());
    let weak = Arc::downgrade(&session);
    drop(session);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn lazer_capture_waits_for_offsets_and_cancel_releases_the_session() {
    let session = capture_session(61052, ClientType::Lazer);
    session.start_capture(noop_warn(), noop_warn()).await;
    tokio::time::timeout(Duration::from_secs(5), session.offset_waiting.notified())
        .await
        .unwrap();
    session.cancel();
    tokio::time::timeout(Duration::from_secs(5), session.join_resolver())
        .await
        .unwrap();
    assert!(session.capture.lock().await.is_none());
    let weak = Arc::downgrade(&session);
    drop(session);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn cancelled_session_never_starts_capture() {
    let session = capture_session(61053, ClientType::Stable);
    session.cancel();
    session.start_capture(noop_warn(), noop_warn()).await;
    assert!(session.capture.lock().await.is_none());
}

#[tokio::test]
async fn installing_offsets_wakes_capture_without_waiting_for_retry_delay() {
    use vosu_osu_reader::memory::lazer_offsets::LazerOffsets;
    let preparing = Arc::new(tokio::sync::Notify::new());
    let session = capture_session_with_scans(
        61101,
        ClientType::Lazer,
        Arc::new(LazerPreparationScans(preparing.clone())),
    );
    let offsets = Arc::new(LazerOffsets::embedded().unwrap());
    session.start_capture(noop_warn(), noop_warn()).await;
    tokio::time::timeout(
        Duration::from_millis(500),
        session.offset_waiting.notified(),
    )
    .await
    .unwrap();
    assert!(session.install_lazer_offsets(offsets));
    tokio::time::timeout(Duration::from_millis(500), preparing.notified())
        .await
        .expect("offset installation must bypass the one-second retry delay");
    session.cancel();
    tokio::time::timeout(Duration::from_secs(5), session.join_resolver())
        .await
        .unwrap();
    assert!(session.capture.lock().await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn running_capture_forwards_bus_demand_and_osu_settings() {
    use crate::settings::OsuSettings;
    use vosu_osu_state::{ReaderSettings, SubscriptionDemand};
    let session = capture_session(61102, ClientType::Stable);
    session.start_capture(noop_warn(), noop_warn()).await;
    let (mut reader, mut demand) = controls(&session).await;
    reader.borrow_and_update();
    demand.borrow_and_update();
    let next_demand = SubscriptionDemand {
        regular: true,
        precise: true,
        ..SubscriptionDemand::default()
    };
    session
        .publication
        .get()
        .unwrap()
        .demand
        .set_bus(true, true);
    tokio::time::timeout(Duration::from_millis(500), demand.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*reader.borrow_and_update(), ReaderSettings::default());
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
            collect_hit_errors: false,
        }
    );
    assert_eq!(*demand.borrow(), next_demand);
    let final_demand = SubscriptionDemand::default();
    session
        .publication
        .get()
        .unwrap()
        .demand
        .set_bus(false, false);
    tokio::time::timeout(Duration::from_millis(500), demand.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*demand.borrow_and_update(), final_demand);
    assert!(!reader.borrow().collect_hit_errors);
    assert_eq!(reader.borrow().poll_rate, 731.0);
    session.cancel();
    tokio::time::timeout(Duration::from_secs(5), session.join_resolver())
        .await
        .unwrap();
    assert!(session.capture.lock().await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn extended_error_collection_follows_bus_demand_and_releases_on_unsubscribe() {
    let session = capture_session(61103, ClientType::Stable);
    session.start_capture(noop_warn(), noop_warn()).await;
    let (mut reader, _) = controls(&session).await;
    assert!(!reader.borrow_and_update().collect_hit_errors);
    let publication = session.publication.get().unwrap();
    publication.demand.set_bus_collection(false, true, true);
    tokio::time::timeout(Duration::from_secs(5), reader.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(reader.borrow_and_update().collect_hit_errors);
    publication.demand.set_bus(false, false);
    tokio::time::timeout(Duration::from_secs(5), reader.changed())
        .await
        .unwrap()
        .unwrap();
    assert!(!reader.borrow_and_update().collect_hit_errors);
    session.cancel();
    tokio::time::timeout(Duration::from_secs(5), session.join_resolver())
        .await
        .unwrap();
}
