use super::*;
use crate::project::{OsuPointer, ProjectionSink};
use std::sync::{atomic::AtomicUsize, mpsc};
use std::time::Duration;
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings, states::HitErrorArray};
use vosu_osu_state::*;
use vosu_plugin_api::{Message, PublishValueRef, decode_publish_batch_ref};
use vosu_proptest::wait::JoinBounded;

#[derive(Default)]
struct Host {
    calls: AtomicUsize,
    logs: AtomicUsize,
    last: AtomicUsize,
    last_batch_len: AtomicUsize,
    foreground: AtomicU8,
    status: AtomicUsize,
    entered: Option<mpsc::SyncSender<()>>,
    release: Option<Mutex<mpsc::Receiver<()>>>,
}
struct TestHost(Arc<Host>);
impl std::ops::Deref for TestHost {
    type Target = Host;
    fn deref(&self) -> &Host {
        &self.0
    }
}
impl HostServices for TestHost {
    fn send(&self, message: &Message, _: u64) -> vosu_status {
        assert!(matches!(message, Message::LogRecord(_)));
        self.logs.fetch_add(1, Ordering::Relaxed);
        vosu_status::VOSU_OK
    }
    fn send_flat(&self, opcode: vosu_opcode, _: u32, _: u64, bytes: &[u8]) -> vosu_status {
        assert_eq!(opcode, vosu_opcode::VOSU_OP_PUBLISH_BATCH);
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        if call == 0
            && let Some(entered) = &self.entered
        {
            entered.try_send(()).unwrap();
            self.release
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
        let batch = decode_publish_batch_ref(bytes).unwrap();
        self.last_batch_len.store(batch.len(), Ordering::Relaxed);
        for field in batch.entries() {
            if field.pointer == "/score"
                && let PublishValueRef::U64(score) = field.value
            {
                self.last.store(score as usize, Ordering::Relaxed);
            }
            if field.pointer == "/session/focused" {
                let value = match field.value {
                    PublishValueRef::Bool(false) => 1,
                    PublishValueRef::Bool(true) => 2,
                    PublishValueRef::Null => 0,
                    _ => panic!("unexpected foreground value"),
                };
                self.foreground.store(value, Ordering::Relaxed);
            }
        }
        match self.status.load(Ordering::Relaxed) {
            1 => vosu_status::VOSU_ERR_PERMISSION_DENIED,
            2 => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            _ => vosu_status::VOSU_OK,
        }
    }
}
fn sample(score: u64) -> Arc<StateBundleSnapshot> {
    Arc::new(StateBundleSnapshot {
        source: ClientType::Stable,
        generation: score,
        reset_epoch: 0,
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
        precise: Arc::new(PreciseState {
            catcher: None,
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
    })
}
fn projection(s: &StateBundleSnapshot, facts: &SessionFacts, sink: &mut dyn ProjectionSink) {
    sink.put(OsuPointer::PlayScore, PublishValueRef::U64(s.generation));
    sink.put(
        OsuPointer::ChartTitle,
        PublishValueRef::String("same-sized title"),
    );
    sink.put(
        OsuPointer::OsuSessionFocused,
        facts
            .foreground
            .map_or(PublishValueRef::Null, PublishValueRef::Bool),
    );
    sink.put_cbor(OsuPointer::OsuProfile, &mut |buf| {
        buf.extend_from_slice(&[0x81, 1])
    });
}
fn publisher(host: Arc<Host>) -> BusPublisher<TestHost> {
    let mut bus = BusPublisher::new(
        TestHost(host),
        81002,
        Arc::new(AtomicBool::new(true)),
        SessionFacts {
            client: ClientType::Stable,
            settings: InstanceSettings {
                tournament_spectator: false,
                tournament_ipc_id: None,
                devserver_endpoint: None,
            },
            foreground: None,
        },
    );
    bus.projections = &[projection];
    bus
}
fn publish(bus: &BusPublisher<TestHost>, snapshot: Arc<StateBundleSnapshot>) {
    bus.publish(snapshot, Delivery::All).unwrap();
}
#[test]
fn unchanged_snapshot_sends_nothing() {
    let host = Arc::new(Host::default());
    let mut bus = publisher(host.clone());
    let s = sample(1);
    bus.identified.store(false, Ordering::Release);
    publish(&bus, s.clone());
    assert_eq!(host.calls.load(Ordering::Relaxed), 0);
    bus.identified.store(true, Ordering::Release);
    publish(&bus, s.clone());
    assert_eq!(host.calls.load(Ordering::Relaxed), 1);
    fn must_not_project(_: &StateBundleSnapshot, _: &SessionFacts, _: &mut dyn ProjectionSink) {
        panic!("unchanged lane snapshots must skip projection");
    }
    bus.projections = &[must_not_project];
    publish(&bus, s.clone());
    // A new wrapper with the same two immutable lane snapshots also short-circuits.
    publish(&bus, Arc::new((*s).clone()));
    assert_eq!(host.calls.load(Ordering::Relaxed), 1);
    bus.projections = &[projection];
    publish(&bus, sample(1));
    assert_eq!(
        host.calls.load(Ordering::Relaxed),
        1,
        "equal values are diffed away"
    );
    publish(&bus, sample(2));
    assert_eq!(host.calls.load(Ordering::Relaxed), 2);
}
#[test]
fn second_identical_shape_publish_allocates_nothing() {
    let host = Arc::new(Host::default());
    let mut bus = publisher(host.clone());
    fn changing_values(s: &StateBundleSnapshot, _: &SessionFacts, sink: &mut dyn ProjectionSink) {
        sink.put(OsuPointer::PlayScore, PublishValueRef::U64(s.generation));
        sink.put(
            OsuPointer::ChartTitle,
            PublishValueRef::String(if s.generation == 1 {
                "first title"
            } else {
                "other title"
            }),
        );
        sink.put_cbor(OsuPointer::OsuProfile, &mut |buf| {
            buf.extend_from_slice(&[0x81, s.generation as u8])
        });
    }
    bus.projections = &[changing_values];
    let first = sample(1);
    let second = sample(2);
    publish(&bus, first);
    assert_eq!(host.last_batch_len.load(Ordering::Relaxed), 3);
    let (_, allocations) = crate::test_alloc::allocations_during(|| publish(&bus, second));
    assert_eq!(allocations, 0);
    assert_eq!(
        host.calls.load(Ordering::Relaxed),
        2,
        "must exercise encoding and host send, not the unchanged-Arc shortcut"
    );
    assert_eq!(host.last.load(Ordering::Relaxed), 2);
    assert_eq!(
        host.last_batch_len.load(Ordering::Relaxed),
        3,
        "all typed scalar, string and CBOR entries must be encoded again"
    );
}
#[test]
fn refused_batch_logs_once_and_keeps_worker_alive() {
    let host = Arc::new(Host::default());
    let bus = publisher(host.clone());
    let s = sample(1);
    host.status.store(1, Ordering::Relaxed);
    publish(&bus, s.clone());
    publish(&bus, s.clone());
    assert_eq!(
        host.calls.load(Ordering::Relaxed),
        2,
        "same refused snapshot is retried"
    );
    assert_eq!(host.logs.load(Ordering::Relaxed), 1);
    host.status.store(2, Ordering::Relaxed);
    publish(&bus, s.clone());
    host.status.store(1, Ordering::Relaxed);
    publish(&bus, s.clone());
    assert_eq!(host.logs.load(Ordering::Relaxed), 2);
    host.status.store(0, Ordering::Relaxed);
    publish(&bus, s.clone());
    assert_eq!(host.calls.load(Ordering::Relaxed), 5);
    publish(&bus, s);
    assert_eq!(host.calls.load(Ordering::Relaxed), 5);
}
#[test]
fn session_refresh_bypasses_snapshot_shortcut() {
    let host = Arc::new(Host::default());
    let bus = publisher(host.clone());
    publish(&bus, sample(1));
    bus.set_foreground(Some(true));
    assert_eq!(host.calls.load(Ordering::Relaxed), 2);
    bus.set_foreground(Some(true));
    assert_eq!(host.calls.load(Ordering::Relaxed), 2);
    bus.set_foreground(None);
    assert_eq!(host.calls.load(Ordering::Relaxed), 3);
}
#[test]
fn in_flight_publish_drains_latest_snapshot_and_session_refresh() {
    let (entered_tx, entered_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let host = Arc::new(Host {
        entered: Some(entered_tx),
        release: Some(Mutex::new(release_rx)),
        ..Host::default()
    });
    let bus = Arc::new(publisher(host.clone()));
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let sending = bus.clone();
    let worker = std::thread::spawn(move || {
        publish(&sending, sample(1));
        done_tx.try_send(()).unwrap();
    });
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    // These must return while the host is blocked; the single pending slot is latest-only.
    publish(&bus, sample(2));
    publish(&bus, sample(3));
    bus.set_foreground(Some(true));
    release_tx.try_send(()).unwrap();
    done_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    worker.join_bounded().unwrap();
    assert_eq!(host.calls.load(Ordering::Relaxed), 2);
    assert_eq!(host.last.load(Ordering::Relaxed), 3);
    assert_eq!(host.foreground.load(Ordering::Relaxed), 2);
    let shared = bus.inner.lock().unwrap();
    let inner = shared.inner.as_ref().unwrap();
    assert_eq!(
        inner.projector.last_snapshot.as_ref().unwrap().generation,
        3
    );
    assert_eq!(inner.facts.foreground, Some(true));
    assert!(shared.pending.is_none());
    assert!(!shared.refresh);
}
