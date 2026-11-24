//! The process-global B1 slot is exercised in its own test binary so native
//! cargo test's concurrently running library tests cannot borrow this sink.
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use vosu_osu::{
    bridge_b1::{B1Event, B1Inputs, B1Phase, B1Sink},
    classify::Platform,
    plugin::OsuPlugin,
};
use vosu_osu_reader::memory::lazer_loader::{GenerationFuture, OffsetGenerator};
use vosu_plugin_api::{
    Bitness, DetachReason, HostServices, Message, SessionAttached, SessionDetached, vosu_status,
};

#[derive(Clone, Default)]
struct Host(Arc<Mutex<Vec<Message>>>);
impl HostServices for Host {
    fn send(&self, message: &Message, _: u64) -> vosu_status {
        self.0.lock().unwrap().push(message.clone());
        vosu_status::VOSU_OK
    }
}
struct OfflineGenerator;
impl OffsetGenerator for OfflineGenerator {
    fn generate<'a>(
        &'a self,
        _: &'a Path,
        _: &'a str,
        _: &'a CancellationToken,
    ) -> GenerationFuture<'a> {
        Box::pin(async { Err(io::Error::new(io::ErrorKind::NotFound, "offline")) })
    }
}
struct Sink {
    events: std::sync::mpsc::SyncSender<B1Event>,
    inputs: watch::Sender<B1Inputs>,
}
impl vosu_surface::LegacySink<B1Event, B1Inputs> for Sink {
    fn send(&self, event: B1Event) {
        self.events
            .try_send(event)
            .expect("finite lifecycle test events");
    }
    fn inputs(&self) -> watch::Receiver<B1Inputs> {
        self.inputs.subscribe()
    }
}
fn attached(handle: u64) -> Message {
    Message::SessionAttached(SessionAttached {
        session: "vosu.osu".into(),
        handle,
        generation: 1,
        pid: handle as u32,
        bitness: Bitness::X86,
        name: Some("osu!.exe".into()),
        path: None,
        command_line: None,
        argv: Some(Vec::new()),
    })
}
fn root(_: u32, path: Option<&Path>, _: Option<&str>) -> PathBuf {
    path.and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_default()
}
#[test]
fn capture_resolves_b1_at_session_start_and_logs_missing_sink_only_once() {
    let data = tempfile::tempdir().unwrap();
    let host = Host::default();
    let plugin = OsuPlugin::with_generator(
        host.clone(),
        Platform::Windows,
        root,
        data.path().to_owned(),
        OfflineGenerator,
    );
    plugin.dispatch(attached(61056), 0).unwrap();
    plugin.dispatch(attached(61058), 0).unwrap();
    let count = host.0.lock().unwrap().iter().filter(|event| matches!(event, Message::LogRecord(record) if record.message.contains("Bridge B1 is not installed"))).count();
    assert_eq!(count, 1);
    let (events, receiver) = std::sync::mpsc::sync_channel(8);
    let sink: Arc<B1Sink> = Arc::new(Sink {
        events,
        inputs: watch::channel(B1Inputs::default()).0,
    });
    let _installed = vosu_surface::install(sink).unwrap();
    plugin.dispatch(attached(61057), 0).unwrap();
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
        B1Event::Attached { .. }
    ));
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
        B1Event::Phase {
            phase: B1Phase::StartingReader,
            ..
        }
    ));
    plugin
        .dispatch(
            Message::SessionDetached(SessionDetached {
                handle: 61057,
                reason: DetachReason::Exited,
            }),
            0,
        )
        .unwrap();
    assert!(matches!(
        receiver.recv_timeout(Duration::from_millis(500)).unwrap(),
        B1Event::Detached { .. }
    ));
    assert!(receiver.try_recv().is_err());
    assert!(!host.0.lock().unwrap().iter().any(|event| matches!(event, Message::ScanRequest(scan) if scan.session == 61056 || scan.session == 61058)));
}
