//! Core's ingress lanes over a real channel, the child played by the test.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use vosu_plugin_api::{CborValue, LogLevel, LogRecord, Message, TimerSet, vosu_opcode};

use super::super::super::channel::{Endpoint, Receiver};
use super::super::super::fake_plugin::{Gate, channel_pair};
use super::*;
use crate::identity::PluginInstanceId;
use crate::plugin::host_services::PluginHostHandler;

const BOUND: Duration = Duration::from_secs(20);

/// Core's handler: blocks the first message of `block_on` until `gate`
/// opens, records everything, and answers SYNC plans with `sync`.
struct Handler {
    block_on: u32,
    blocked_once: AtomicBool,
    gate: Arc<Gate>,
    handled: Mutex<Vec<u32>>,
    sync_gate: Arc<Gate>,
    sync_overlap: Mutex<Vec<bool>>,
    sync: vosu_status,
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
        self.sync_gate.arrive();
        let overlap = self.sync_gate.wait_arrivals(3, Duration::from_secs(5));
        lock(&self.sync_overlap).push(overlap);
        Some(self.sync)
    }
}

struct Lanes {
    ingress: Ingress,
    handler: Arc<Handler>,
    ledger: Arc<Ledger>,
    child: Endpoint,
    buffer: PayloadBuffer,
}

fn start(block_on: vosu_opcode, sync: vosu_status) -> Lanes {
    let (core, child) = channel_pair();
    let handler = Arc::new(Handler {
        block_on: block_on as u32,
        blocked_once: AtomicBool::new(false),
        gate: Gate::new(),
        handled: Mutex::new(Vec::new()),
        sync_gate: Gate::new(),
        sync_overlap: Mutex::new(Vec::new()),
        sync,
    });
    let dyn_handler: Arc<dyn PluginHostHandler> = handler.clone();
    let ingress = Ingress::start(
        PluginIngress::new(PluginInstanceId::new(1), dyn_handler),
        core.sender.clone(),
        Arc::clone(&core.ledger),
        Arc::new(StatusGate::default()),
        1,
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

/// SYNC submissions run on their own workers: a blocked ordinary handler
/// delays none of them, and several run at once.
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
    assert_eq!(lanes.ingress.sync_workers(), 3);
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
