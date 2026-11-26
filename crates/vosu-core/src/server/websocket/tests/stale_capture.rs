//! A publication pass must never send a client an image older than the one
//! its acknowledgement observed, nor shared bytes encoded for an earlier
//! generation (latest-only).
use super::*;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::ThreadId;

const BOUND: Duration = Duration::from_secs(10);

type Gate = Arc<Mutex<Option<(SyncSender<u64>, Receiver<()>)>>>;

/// Reports `generation` to the armed gate's owner and blocks until released.
fn pass_gate(gate: &Gate, generation: u64) {
    let armed = lock_unpoisoned(gate).take();
    if let Some((reached, release)) = armed {
        reached.send(generation).unwrap();
        release.recv_timeout(BOUND).unwrap();
    }
}

fn arm(gate: &Gate) -> (Receiver<u64>, SyncSender<()>) {
    let (reached, reached_rx) = sync_channel(1);
    let (release_tx, release) = sync_channel(1);
    *lock_unpoisoned(gate) = Some((reached, release));
    (reached_rx, release_tx)
}

struct GatedSnapshot {
    generation: u64,
    unfiltered: Arc<AtomicUsize>,
    filtered: Arc<AtomicUsize>,
    filter_gate: Gate,
}

impl StateSnapshot for GatedSnapshot {
    fn generation(&self) -> u64 {
        self.generation
    }
    fn encode_unfiltered(&self) -> Result<Bytes, String> {
        self.unfiltered.fetch_add(1, Ordering::Relaxed);
        Ok(Bytes::from(format!("unfiltered:{}", self.generation)))
    }
    fn encode_filtered(&self, _: &[Filter]) -> Result<Bytes, String> {
        self.filtered.fetch_add(1, Ordering::Relaxed);
        pass_gate(&self.filter_gate, self.generation);
        Ok(Bytes::from(format!("filtered:{}", self.generation)))
    }
}

/// Explicit barriers inside one publication pass: an armed `capture_gate`
/// stops the hub runner's next capture (acknowledgement captures on the test
/// thread pass through); an armed `filter_gate` stops the next filtered encode.
struct GatedProvider {
    generation: AtomicU64,
    unfiltered: Arc<AtomicUsize>,
    filtered: Arc<AtomicUsize>,
    events: watch::Sender<u64>,
    acknowledging: ThreadId,
    capture_gate: Gate,
    filter_gate: Gate,
}

impl GatedProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            generation: AtomicU64::new(1),
            unfiltered: Arc::new(AtomicUsize::new(0)),
            filtered: Arc::new(AtomicUsize::new(0)),
            events: watch::channel(1).0,
            acknowledging: std::thread::current().id(),
            capture_gate: Gate::default(),
            filter_gate: Gate::default(),
        })
    }
}

impl StateProvider for GatedProvider {
    // Delivery comes only from acknowledgements and explicit wakes.
    fn poll_rate(&self, _: Schema) -> Duration {
        Duration::from_secs(3600)
    }
    fn subscribe(&self, _: Schema) -> watch::Receiver<u64> {
        self.events.subscribe()
    }
    fn capture(&self, _: Schema) -> Option<Arc<dyn StateSnapshot>> {
        let generation = self.generation.load(Ordering::Acquire);
        if std::thread::current().id() != self.acknowledging {
            pass_gate(&self.capture_gate, generation);
        }
        Some(Arc::new(GatedSnapshot {
            generation,
            unfiltered: Arc::clone(&self.unfiltered),
            filtered: Arc::clone(&self.filtered),
            filter_gate: Arc::clone(&self.filter_gate),
        }))
    }
}

fn text(runtime: &tokio::runtime::Runtime, receiver: &mut OutboundReceiver) -> Bytes {
    match runtime.block_on(async { tokio::time::timeout(BOUND, receiver.recv()).await }) {
        Ok(Some(Outbound::Text(bytes))) => bytes,
        frame => panic!("expected a state frame, got {frame:?}"),
    }
}

struct Fixture {
    runtime: tokio::runtime::Runtime,
    provider: Arc<GatedProvider>,
    hub: WebSocketHub,
    run: tokio::task::JoinHandle<()>,
    plain: (Connection, OutboundReceiver),
    filtered: (Connection, OutboundReceiver),
}

/// A plain and a filtered client, each holding a generation-1 frame in flight.
fn fixture() -> Fixture {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let provider = GatedProvider::new();
    let hub = WebSocketHub::new(Schema::V2, provider.clone());
    let (plain, filtered) = {
        let _enter = runtime.enter();
        let plain = hub.connect();
        let filtered = hub.connect();
        filtered.0.set_filters(vec![Filter::Field("play".into())]);
        (plain, filtered)
    };
    let run = runtime.spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let mut fixture = Fixture {
        runtime,
        provider,
        hub,
        run,
        plain,
        filtered,
    };
    assert_eq!(text(&fixture.runtime, &mut fixture.plain.1), "unfiltered:1");
    assert_eq!(
        text(&fixture.runtime, &mut fixture.filtered.1),
        "filtered:1"
    );
    fixture
}

impl Fixture {
    fn finish(self) {
        self.hub.shutdown();
        let run = self.run;
        self.runtime
            .block_on(async { tokio::time::timeout(BOUND, run).await })
            .unwrap()
            .unwrap();
    }
}

#[test]
fn acknowledged_client_never_receives_a_capture_older_than_its_acknowledgement() {
    let mut f = fixture();
    let unfiltered = f.provider.unfiltered.load(Ordering::Relaxed);
    let filtered = f.provider.filtered.load(Ordering::Relaxed);
    // Generation 2: the filtered client's acknowledgement starts a pass, which
    // captures generation 2 and stops inside capture.
    f.provider.generation.store(2, Ordering::Release);
    let (reached, release) = arm(&f.provider.capture_gate);
    f.filtered.0.write_completed();
    assert_eq!(reached.recv_timeout(BOUND).unwrap(), 2);
    // Generation 3 commits; the plain client acknowledges it mid-pass.
    f.provider.generation.store(3, Ordering::Release);
    f.plain.0.write_completed();
    release.send(()).unwrap();

    assert_eq!(
        text(&f.runtime, &mut f.plain.1),
        "unfiltered:3",
        "the first frame after the acknowledgement is the latest image"
    );
    assert_eq!(text(&f.runtime, &mut f.filtered.1), "filtered:3");
    assert_eq!(
        f.provider.unfiltered.load(Ordering::Relaxed) - unfiltered,
        1,
        "the stale image is never encoded"
    );
    assert_eq!(f.provider.filtered.load(Ordering::Relaxed) - filtered, 1);
    f.finish();
}

/// A plain client turning writable while a pass encodes another client must
/// get bytes for that pass's generation, not unfiltered bytes kept from an
/// earlier one. Client order within a pass follows the hub's map, so each
/// round reaches the plain client after the gated filtered encode with
/// probability 1/2; 32 independent hubs make a missed interleaving ~2^-32.
#[test]
fn client_writable_mid_pass_never_receives_older_shared_bytes() {
    for _ in 0..32 {
        let mut f = fixture();
        // Generation 2 is committed; the filtered client acknowledges it and
        // its pass stops inside that client's filtered encode.
        f.provider.generation.store(2, Ordering::Release);
        let (reached, release) = arm(&f.provider.filter_gate);
        f.filtered.0.write_completed();
        assert_eq!(reached.recv_timeout(BOUND).unwrap(), 2);
        // The plain client acknowledges the same generation mid-pass.
        f.plain.0.write_completed();
        release.send(()).unwrap();
        assert_eq!(text(&f.runtime, &mut f.filtered.1), "filtered:2");
        assert_eq!(text(&f.runtime, &mut f.plain.1), "unfiltered:2");
        f.finish();
    }
}
