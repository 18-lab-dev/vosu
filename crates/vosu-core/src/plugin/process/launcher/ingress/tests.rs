//! Core's ingress lanes over a real channel, the child played by the test.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vosu_plugin_api::{CborValue, LogLevel, LogRecord, Message, TimerSet, vosu_opcode};

use super::super::super::channel::{Endpoint, Receiver};
use super::super::super::fake_plugin::{Gate, channel_pair};
use super::*;
use crate::identity::PluginInstanceId;
use crate::plugin::host_services::PluginHostHandler;
use crate::plugin::scan::ScanWorkerPool;

const BOUND: Duration = Duration::from_secs(20);

/// Core's handler: blocks the first message of `block_on` until `gate`
/// opens, records everything, and answers SYNC plans with `sync` — after
/// `sync_block` opens when `block_sync` is set, else after a three-way
/// rendezvous. Broker work runs on `broker`'s workers, or is refused
/// without one.
struct Handler {
    block_on: u32,
    blocked_once: AtomicBool,
    gate: Arc<Gate>,
    handled: Mutex<Vec<u32>>,
    sync_gate: Arc<Gate>,
    sync_overlap: Mutex<Vec<bool>>,
    sync: vosu_status,
    block_sync: bool,
    sync_block: Arc<Gate>,
    sync_calls: AtomicUsize,
    broker: Option<ScanWorkerPool>,
    brokered: AtomicUsize,
}

impl PluginHostHandler for Handler {
    fn handle(&self, _: PluginInstanceId, message: Message, _: u64) -> vosu_status {
        let opcode = message.opcode() as u32;
        if opcode == self.block_on && !self.blocked_once.swap(true, Ordering::SeqCst) {
            self.gate.wait();
        }
        lock(&self.handled).push(opcode);
        vosu_status::VOSU_OK
    }

    fn handle_read_plan_sync(&self, _: PluginInstanceId, _: &[u8], _: u32) -> Option<vosu_status> {
        self.sync_calls.fetch_add(1, Ordering::SeqCst);
        if self.block_sync {
            self.sync_block.wait();
            return Some(self.sync);
        }
        self.sync_gate.arrive();
        let overlap = self.sync_gate.wait_arrivals(3, Duration::from_secs(5));
        lock(&self.sync_overlap).push(overlap);
        Some(self.sync)
    }

    fn submit_broker_work(
        &self,
        instance: PluginInstanceId,
        job: crate::plugin::host_services::BrokerJob,
    ) -> Result<(), crate::plugin::host_services::BrokerJob> {
        let Some(broker) = &self.broker else {
            return Err(job);
        };
        self.brokered.fetch_add(1, Ordering::SeqCst);
        broker.submit(instance.0, job);
        Ok(())
    }
}

struct Lanes {
    ingress: Ingress,
    handler: Arc<Handler>,
    ledger: Arc<Ledger>,
    child: Endpoint,
    buffer: PayloadBuffer,
}

/// How [`start_with`] configures the handler and the SYNC budget.
struct Setup {
    block_on: vosu_opcode,
    sync: vosu_status,
    block_sync: bool,
    broker_workers: Option<usize>,
    budget: Duration,
}

fn start(block_on: vosu_opcode, sync: vosu_status) -> Lanes {
    start_with(Setup {
        block_on,
        sync,
        block_sync: false,
        broker_workers: Some(4),
        budget: BOUND,
    })
}

fn start_with(setup: Setup) -> Lanes {
    let (core, child) = channel_pair();
    let handler = Arc::new(Handler {
        block_on: setup.block_on as u32,
        blocked_once: AtomicBool::new(false),
        gate: Gate::new(),
        handled: Mutex::new(Vec::new()),
        sync_gate: Gate::new(),
        sync_overlap: Mutex::new(Vec::new()),
        sync: setup.sync,
        block_sync: setup.block_sync,
        sync_block: Gate::new(),
        sync_calls: AtomicUsize::new(0),
        broker: setup.broker_workers.map(ScanWorkerPool::new),
        brokered: AtomicUsize::new(0),
    });
    let dyn_handler: Arc<dyn PluginHostHandler> = handler.clone();
    let ingress = Ingress::start(
        PluginIngress::new(PluginInstanceId::new(1), dyn_handler),
        core.sender.clone(),
        Arc::clone(&core.ledger),
        Arc::new(StatusGate::default()),
        1,
        setup.budget,
    )
    .unwrap();
    Lanes {
        ingress,
        handler,
        ledger: Arc::clone(&core.ledger),
        child,
        buffer: PayloadBuffer::new(),
    }
}

impl Lanes {
    fn submit(&self, request: Option<(u64, RequestKind)>, message: &Message) {
        if let Some((id, kind)) = request {
            assert!(self.ledger.open(id, kind), "the policy admitted it");
        }
        let header = FrameHeader {
            opcode: message.opcode() as u32,
            flags: message.flags(),
            correlation: 0,
        };
        let mut buffer = PayloadBuffer::new();
        buffer.fill(&message.encode_payload().unwrap());
        self.ingress
            .submit(request.map(|(id, _)| id), header, &mut buffer);
    }

    /// The next `STATUS` core wrote: (request, status).
    fn status(&mut self) -> (u64, vosu_status) {
        let header = recv(&mut self.child.receiver, &mut self.buffer);
        assert_eq!(header.opcode, control::STATUS);
        let bytes = self.buffer.bytes();
        let raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        (header.correlation, vosu_status::try_from(raw).unwrap())
    }
}

fn recv(receiver: &mut Receiver, buffer: &mut PayloadBuffer) -> FrameHeader {
    receiver
        .recv_until(buffer, Some(Instant::now() + BOUND))
        .unwrap()
        .expect("core wrote a frame within the bound")
}

fn log(len: usize) -> Message {
    Message::LogRecord(LogRecord {
        level: LogLevel::Info,
        message: "x".repeat(len),
        fields: CborValue::Map(Vec::new()),
    })
}

fn timer_set() -> Message {
    Message::TimerSet(TimerSet {
        timer: 1,
        interval_ms: None,
    })
}

fn sync_submit() -> Message {
    Message::ReadPlanSubmit(vosu_plugin_api::ReadPlanSubmit {
        plan: 1,
        handle: 1,
        root: vosu_plugin_api::PlanBase::Absolute(0),
        stages: Vec::new(),
        sync: true,
    })
}

/// Unacknowledged log records are the one byte-bounded ordinary input:
/// past [`LOG_BUDGET`] they are dropped and counted, while acknowledged
/// requests are always queued and answered.
#[test]
fn log_records_past_the_budget_are_dropped_while_requests_are_answered() {
    let mut lanes = start(vosu_opcode::VOSU_OP_LOG_RECORD, vosu_status::VOSU_OK);
    lanes.submit(None, &log(16));
    assert!(
        lanes.handler.gate.wait_arrivals(1, BOUND),
        "the worker is blocked"
    );
    let record = log(64 * 1024);
    for _ in 0..40 {
        lanes.submit(None, &record);
    }
    lanes.submit(Some((1, RequestKind::Status)), &timer_set());
    let dropped = lanes.ingress.dropped_logs();
    let queued = 40 - dropped as usize;
    assert!(dropped > 0 && queued >= 14, "{dropped} dropped");
    lanes.handler.gate.open();
    assert_eq!(lanes.status(), (1, vosu_status::VOSU_OK));
    let handled = lock(&lanes.handler.handled).clone();
    assert_eq!(handled.len(), 1 + queued + 1);
    assert_eq!(
        *handled.last().unwrap(),
        vosu_opcode::VOSU_OP_TIMER_SET as u32
    );
    lanes.ingress.shutdown();
}

/// SYNC submissions run on the broker's workers (M5 B0): a blocked
/// ordinary handler delays none of them, and several run at once.
#[test]
fn sync_requests_run_beside_a_blocked_ordinary_lane() {
    let mut lanes = start(
        vosu_opcode::VOSU_OP_TIMER_SET,
        vosu_status::VOSU_ERR_TARGET_GONE,
    );
    lanes.submit(Some((1, RequestKind::Status)), &timer_set());
    assert!(lanes.handler.gate.wait_arrivals(1, BOUND));
    for id in 2..=4 {
        lanes.submit(Some((id, RequestKind::Sync)), &sync_submit());
    }
    let mut answered: Vec<_> = (0..3).map(|_| lanes.status()).collect();
    answered.sort_unstable_by_key(|(id, _)| *id);
    assert_eq!(
        answered,
        [2, 3, 4].map(|id| (id, vosu_status::VOSU_ERR_TARGET_GONE)),
        "a refused SYNC plan is answered by its STATUS"
    );
    assert_eq!(*lock(&lanes.handler.sync_overlap), vec![true; 3]);
    assert_eq!(lanes.handler.brokered.load(Ordering::SeqCst), 3);
    for id in 2..=4 {
        assert_eq!(lanes.ledger.kind(id), None, "closed before the reply");
    }
    lanes.handler.gate.open();
    assert_eq!(lanes.status(), (1, vosu_status::VOSU_OK));
    lanes.ingress.shutdown();
}

/// An accepted SYNC plan is answered only by REPLY_NEXT and its result; a
/// handler reporting OK without delivering one is answered BACKEND,
/// never `STATUS OK`.
#[test]
fn an_ok_sync_without_a_result_is_never_answered_ok() {
    let mut lanes = start(vosu_opcode::VOSU_OP_TIMER_SET, vosu_status::VOSU_OK);
    // Let the handler's three-way rendezvous pass at once.
    for _ in 0..2 {
        lanes.handler.sync_gate.arrive();
    }
    lanes.submit(Some((9, RequestKind::Sync)), &sync_submit());
    assert_eq!(lanes.status(), (9, vosu_status::VOSU_ERR_BACKEND));
    lanes.ingress.shutdown();
}

/// M5 B0: a SYNC plan blocked past its budget is answered
/// `VOSU_ERR_TIMED_OUT` by core's own timer, once: when the blocked
/// handler finally returns, nothing more is written for that request, and
/// the ordinary lane kept answering throughout.
#[test]
fn read_plan_sync_past_its_budget_is_answered_timed_out_by_core_once() {
    let mut lanes = start_with(Setup {
        block_on: vosu_opcode::VOSU_OP_LOG_RECORD,
        sync: vosu_status::VOSU_ERR_TARGET_GONE,
        block_sync: true,
        broker_workers: Some(4),
        budget: Duration::from_millis(150),
    });
    lanes.submit(Some((1, RequestKind::Sync)), &sync_submit());
    assert!(lanes.handler.sync_block.wait_arrivals(1, BOUND), "blocked");
    lanes.submit(Some((2, RequestKind::Status)), &timer_set());
    let mut answered = [lanes.status(), lanes.status()];
    answered.sort_unstable_by_key(|(id, _)| *id);
    assert_eq!(
        answered,
        [
            (1, vosu_status::VOSU_ERR_TIMED_OUT),
            (2, vosu_status::VOSU_OK)
        ],
        "the timer answers the blocked plan; the ordinary lane drains beside it"
    );
    assert_eq!(lanes.ledger.kind(1), None, "closed by the timer");
    assert_eq!(lanes.ingress.expired_sync(), 1);
    lanes.handler.sync_block.open();
    // The handler returns TARGET_GONE now; only the next request's STATUS
    // may follow.
    lanes.submit(Some((3, RequestKind::Status)), &timer_set());
    assert_eq!(lanes.status(), (3, vosu_status::VOSU_OK));
    assert_eq!(lanes.handler.sync_calls.load(Ordering::SeqCst), 1);
    lanes.ingress.shutdown();
}

/// M5 B0: a SYNC plan still queued behind a busy broker when its budget
/// expires is answered `VOSU_ERR_TIMED_OUT` and never starts afterwards.
#[test]
fn read_plan_sync_answered_by_the_timer_never_starts() {
    let mut lanes = start_with(Setup {
        block_on: vosu_opcode::VOSU_OP_LOG_RECORD,
        sync: vosu_status::VOSU_ERR_TARGET_GONE,
        block_sync: true,
        broker_workers: Some(1),
        budget: Duration::from_millis(150),
    });
    lanes.submit(Some((1, RequestKind::Sync)), &sync_submit());
    assert!(lanes.handler.sync_block.wait_arrivals(1, BOUND));
    lanes.submit(Some((2, RequestKind::Sync)), &sync_submit());
    let mut answered = [lanes.status(), lanes.status()];
    answered.sort_unstable_by_key(|(id, _)| *id);
    assert_eq!(
        answered,
        [1, 2].map(|id| (id, vosu_status::VOSU_ERR_TIMED_OUT))
    );
    lanes.handler.sync_block.open();
    lanes.submit(Some((3, RequestKind::Status)), &timer_set());
    assert_eq!(lanes.status(), (3, vosu_status::VOSU_OK));
    // Request 2 reached the broker but its job found it answered: a probe
    // queued behind it on the one worker runs only after it.
    let (ran_tx, ran_rx) = std::sync::mpsc::sync_channel(1);
    lanes.handler.broker.as_ref().unwrap().submit(1, move || {
        ran_tx.send(()).unwrap();
    });
    ran_rx.recv_timeout(BOUND).unwrap();
    assert_eq!(lanes.handler.sync_calls.load(Ordering::SeqCst), 1);
    assert_eq!(lanes.handler.brokered.load(Ordering::SeqCst), 2);
    lanes.ingress.shutdown();
}

/// A handler without broker workers cannot run a SYNC plan: the ordinary
/// worker refuses it with `VOSU_ERR_BACKEND`, and the reader never writes.
#[test]
fn read_plan_sync_without_broker_workers_is_refused_backend() {
    let mut lanes = start_with(Setup {
        block_on: vosu_opcode::VOSU_OP_LOG_RECORD,
        sync: vosu_status::VOSU_OK,
        block_sync: false,
        broker_workers: None,
        budget: BOUND,
    });
    lanes.submit(Some((4, RequestKind::Sync)), &sync_submit());
    assert_eq!(lanes.status(), (4, vosu_status::VOSU_ERR_BACKEND));
    assert_eq!(lanes.handler.sync_calls.load(Ordering::SeqCst), 0);
    lanes.ingress.shutdown();
}
