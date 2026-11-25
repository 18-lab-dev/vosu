//! Core's supervision state machine over a real channel (memfd/eventfd on
//! Linux, section/event on Windows) with the child played by the test and
//! a scripted process handle: startup outcomes, acknowledged sends, loss
//! identity and controlled destroy. The real child is exercised natively
//! by the root `plugin_host_native` target.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vosu_plugin_api::{
    CborValue, LogLevel, LogRecord, Message, PlanBase, ReadPlanResult, ReadPlanSubmit, ReadResult,
    TimerSet, VOSU_READ_PLAN_SYNC, vosu_opcode, vosu_status,
};
use vosu_sys::plugin_process::RingObjects;

use super::super::channel::{Endpoint, connect};
use super::super::control::{self, Create, Failed, Ready, StartupFailure, StartupStage};
use super::super::fake_plugin::Gate;
use super::super::frame::{Direction, DirectionPolicy, FrameHeader, control as op};
use super::super::ring::{PayloadBuffer, REGION_BYTES, initialize};
use super::platform::{HostProcess, Started};
use super::supervision::{self, Start};
use crate::identity::PluginInstanceId;
use crate::plugin::error::HostError;
use crate::plugin::execution::{
    InstanceLoss, LossCause, LossState, MessageRef, PluginIngress, ProcessEndpoint,
};
use crate::plugin::host_services::PluginHostHandler;

pub(crate) mod channel;

const BOUND: Duration = Duration::from_secs(20);
const GENERATION: u64 = 11;

#[cfg(target_os = "linux")]
fn adopt(objects: &RingObjects) -> RingObjects {
    RingObjects::from_inherited(
        objects.mapping_fd().try_clone_to_owned().unwrap(),
        objects.doorbell_fd().try_clone_to_owned().unwrap(),
        REGION_BYTES,
    )
    .unwrap()
}

#[cfg(windows)]
fn adopt(objects: &RingObjects) -> RingObjects {
    RingObjects::from_inherited(
        objects.mapping_handle().try_clone_to_owned().unwrap(),
        objects.doorbell_handle().try_clone_to_owned().unwrap(),
        REGION_BYTES,
    )
    .unwrap()
}

fn pair() -> (Endpoint, Endpoint) {
    let to_plugin = RingObjects::create(REGION_BYTES).unwrap();
    let to_host = RingObjects::create(REGION_BYTES).unwrap();
    initialize(to_plugin.region()).unwrap();
    initialize(to_host.region()).unwrap();
    let child_to_plugin = adopt(&to_plugin);
    let child_to_host = adopt(&to_host);
    let core = connect(
        to_plugin.into(),
        to_host.into(),
        Box::new(DirectionPolicy::with_ledger(Direction::PluginToHost)),
    )
    .unwrap();
    let child = connect(
        child_to_host.into(),
        child_to_plugin.into(),
        Box::new(DirectionPolicy::with_ledger(Direction::HostToPlugin)),
    )
    .unwrap();
    (core, child)
}

/// A process handle the test exits or core kills.
#[derive(Default)]
struct ScriptedProcess {
    exit: Mutex<Option<i64>>,
    changed: Condvar,
    kills: AtomicUsize,
}

impl ScriptedProcess {
    fn exit(&self, status: i64) {
        let mut exit = self.exit.lock().unwrap_or_else(PoisonError::into_inner);
        exit.get_or_insert(status);
        self.changed.notify_all();
    }

    fn kills(&self) -> usize {
        self.kills.load(Ordering::SeqCst)
    }
}

impl HostProcess for ScriptedProcess {
    fn pid(&self) -> u32 {
        4242
    }

    fn kill(&self) -> std::io::Result<()> {
        self.kills.fetch_add(1, Ordering::SeqCst);
        self.exit(-9);
        Ok(())
    }

    fn wait(&self) -> std::io::Result<i64> {
        let mut exit = self.exit.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(status) = *exit {
                return Ok(status);
            }
            exit = self
                .changed
                .wait(exit)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

type MessageHook = Box<dyn Fn(&Message) + Send + Sync>;

/// Core's handler: answers every message with `status` and records losses.
/// `hook` runs inside `handle`; a SYNC plan is answered by dispatching a
/// result through `endpoint` on the handling thread, as the host does.
struct Handler {
    status: vosu_status,
    messages: Mutex<Vec<(Message, u64)>>,
    losses: Mutex<Vec<LossCause>>,
    changed: Condvar,
    hook: Mutex<Option<MessageHook>>,
    endpoint: OnceLock<std::sync::Weak<dyn ProcessEndpoint>>,
    broker: crate::plugin::scan::ScanWorkerPool,
    /// Blocks every SYNC plan until it opens (a plan stuck in a read).
    sync_hold: Mutex<Option<Arc<Gate>>>,
    /// The plan of every SYNC result the handler dispatched, in order.
    sync_done: Mutex<Vec<u64>>,
}

impl Handler {
    fn new(status: vosu_status) -> Arc<Self> {
        Arc::new(Self {
            status,
            messages: Mutex::new(Vec::new()),
            losses: Mutex::new(Vec::new()),
            changed: Condvar::new(),
            hook: Mutex::new(None),
            endpoint: OnceLock::new(),
            broker: crate::plugin::scan::ScanWorkerPool::new(2),
            sync_hold: Mutex::new(None),
            sync_done: Mutex::new(Vec::new()),
        })
    }

    fn wait_for<T>(&self, mutex: &Mutex<Vec<T>>, count: usize) -> usize {
        let deadline = Instant::now() + BOUND;
        let mut items = mutex.lock().unwrap();
        while items.len() < count && Instant::now() < deadline {
            items = self
                .changed
                .wait_timeout(items, deadline - Instant::now())
                .unwrap()
                .0;
        }
        items.len()
    }
}

impl PluginHostHandler for Handler {
    fn handle(&self, _: PluginInstanceId, message: Message, correlation_id: u64) -> vosu_status {
        if let Some(hook) = self.hook.lock().unwrap().as_ref() {
            hook(&message);
        }
        self.messages
            .lock()
            .unwrap()
            .push((message, correlation_id));
        self.changed.notify_all();
        self.status
    }

    fn handle_read_plan_sync(
        &self,
        _: PluginInstanceId,
        payload: &[u8],
        _: u32,
    ) -> Option<vosu_status> {
        let hold = self.sync_hold.lock().unwrap().clone();
        if let Some(gate) = hold {
            gate.wait();
        }
        let endpoint = self.endpoint.get()?.upgrade()?;
        let plan = u64::from_le_bytes(payload[..8].try_into().unwrap());
        let result = sync_result(plan).encode_payload().unwrap();
        endpoint.dispatch(MessageRef {
            opcode: vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
            flags: 0,
            correlation_id: 0,
            payload: &result,
        });
        self.sync_done.lock().unwrap().push(plan);
        self.changed.notify_all();
        Some(vosu_status::VOSU_OK)
    }

    fn instance_lost(&self, _: PluginInstanceId, cause: LossCause) {
        self.losses.lock().unwrap().push(cause);
        self.changed.notify_all();
    }

    fn submit_broker_work(
        &self,
        instance: PluginInstanceId,
        job: crate::plugin::host_services::BrokerJob,
    ) -> Result<(), crate::plugin::host_services::BrokerJob> {
        self.broker.submit(instance.0, job);
        Ok(())
    }
}

fn sync_result(plan: u64) -> Message {
    Message::ReadPlanResult(ReadPlanResult {
        plan,
        results: vec![ReadResult::Ok(plan.to_be_bytes().to_vec())],
    })
}

type Launch = JoinHandle<Result<Arc<dyn ProcessEndpoint>, StartupFailure>>;

struct Harness {
    launch: Option<Launch>,
    child: Endpoint,
    process: Arc<ScriptedProcess>,
    handler: Arc<Handler>,
    buffer: PayloadBuffer,
}

fn start_harness(status: vosu_status, startup: Duration) -> Harness {
    let (core, child) = pair();
    let process = Arc::new(ScriptedProcess::default());
    let handler = Handler::new(status);
    let dyn_handler: Arc<dyn PluginHostHandler> = handler.clone();
    let id = PluginInstanceId::new(5);
    let create = Create {
        generation: GENERATION,
        plugin_id: "acme.fixture".to_owned(),
        library: "/opt/acme/lib.so".into(),
        library_sha256: [7; 32],
        concurrent_dispatch: false,
        sync_budget_ms: 2_000,
        init: vosu_plugin_api::Init {
            plugin_id: "acme.fixture".to_owned(),
            settings: CborValue::Map(Vec::new()),
            data_dir: "/tmp".into(),
        },
    }
    .encode()
    .unwrap();
    let start = Start {
        generation: GENERATION,
        plugin_id: "acme.fixture".to_owned(),
        create,
        deadline: Instant::now() + startup,
        ingress: PluginIngress::new(id, Arc::clone(&dyn_handler)),
        loss: InstanceLoss::new(id, LossState::new(), &dyn_handler),
        sync_budget: BOUND,
    };
    let started = Started {
        endpoint: core,
        process: process.clone(),
    };
    let launch = std::thread::spawn(move || supervision::start(started, start));
    Harness {
        launch: Some(launch),
        child,
        process,
        handler,
        buffer: PayloadBuffer::new(),
    }
}

fn private(opcode: u32, correlation: u64) -> FrameHeader {
    FrameHeader {
        opcode,
        flags: 0,
        correlation,
    }
}

fn public(message: &Message) -> (FrameHeader, Vec<u8>) {
    (
        FrameHeader {
            opcode: message.opcode() as u32,
            flags: 0,
            correlation: 0,
        },
        message.encode_payload().unwrap(),
    )
}

fn timer_set() -> Message {
    Message::TimerSet(TimerSet {
        timer: 9,
        interval_ms: Some(250),
    })
}

fn log_record() -> Message {
    Message::LogRecord(LogRecord {
        level: LogLevel::Info,
        message: "hello".into(),
        fields: CborValue::Map(Vec::new()),
    })
}

impl Harness {
    fn recv(&mut self) -> FrameHeader {
        self.child
            .receiver
            .recv_until(&mut self.buffer, Instant::now().checked_add(BOUND))
            .unwrap()
            .expect("core sent a frame within the bound")
    }

    /// Reads `CREATE` and checks it names this generation.
    fn expect_create(&mut self) {
        let header = self.recv();
        assert_eq!(header, private(op::CREATE, 0));
        let create = Create::decode(self.buffer.bytes()).unwrap();
        assert_eq!(create.generation, GENERATION);
        assert_eq!(create.library_sha256, [7; 32]);
    }

    fn send_ready(&self, ready: Ready) {
        self.child
            .sender
            .send(&private(op::READY, 0), &ready.encode())
            .unwrap();
    }

    fn launch(&mut self) -> Result<Arc<dyn ProcessEndpoint>, StartupFailure> {
        self.launch.take().unwrap().join().unwrap()
    }

    fn ready(&mut self) -> Arc<dyn ProcessEndpoint> {
        self.expect_create();
        self.send_ready(Ready {
            generation: GENERATION,
            struct_size: crate::plugin::execution::descriptor::descriptor_minimum() as u32,
            abi_version: 1,
        });
        self.launch().expect("READY publishes the instance")
    }

    fn losses(&self) -> Vec<LossCause> {
        self.handler.losses.lock().unwrap().clone()
    }
}

fn failure(result: Result<Arc<dyn ProcessEndpoint>, StartupFailure>) -> StartupFailure {
    match result {
        Ok(_) => panic!("startup unexpectedly succeeded"),
        Err(failure) => failure,
    }
}

#[test]
fn ready_dispatch_acknowledged_status_and_controlled_destroy() {
    let mut harness = start_harness(vosu_status::VOSU_ERR_PERMISSION_DENIED, BOUND);
    let endpoint = harness.ready();

    // Core → child: the exact frame, unacknowledged.
    let payload = [1_u8, 2, 3, 4, 5];
    let status = endpoint.dispatch(MessageRef {
        opcode: vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32,
        flags: 0,
        correlation_id: 77,
        payload: &payload,
    });
    assert_eq!(status, vosu_status::VOSU_OK);
    let header = harness.recv();
    assert_eq!(header.opcode, vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32);
    assert_eq!(header.correlation, 77);
    assert_eq!(harness.buffer.bytes(), payload);

    // Child → core acknowledged: core's own status comes back for that id.
    let (header, payload) = public(&timer_set());
    harness
        .child
        .sender
        .send_acknowledged(1, &header, &payload)
        .unwrap();
    let reply = harness.recv();
    assert_eq!(reply, private(op::STATUS, 1));
    assert_eq!(
        harness.buffer.bytes(),
        (vosu_status::VOSU_ERR_PERMISSION_DENIED as u32).to_le_bytes()
    );
    // LOG_RECORD alone is unacknowledged and gets no reply.
    let (header, payload) = public(&log_record());
    harness.child.sender.send(&header, &payload).unwrap();
    assert_eq!(harness.handler.wait_for(&harness.handler.messages, 2), 2);
    {
        let messages = harness.handler.messages.lock().unwrap();
        assert_eq!(messages[0].0, timer_set());
        assert_eq!(messages[1].0, log_record());
    }

    let destroy = std::thread::spawn({
        let endpoint = Arc::clone(&endpoint);
        move || endpoint.destroy(BOUND)
    });
    assert_eq!(harness.recv(), private(op::DESTROY, 0));
    harness
        .child
        .sender
        .send(&private(op::DESTROYED, 0), &[])
        .unwrap();
    harness.process.exit(0);
    destroy.join().unwrap().unwrap();
    assert_eq!(harness.process.kills(), 0, "a clean destroy kills nothing");
    assert!(
        harness.losses().is_empty(),
        "controlled teardown is no loss"
    );
    assert_eq!(
        endpoint.dispatch(MessageRef {
            opcode: vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32,
            flags: 0,
            correlation_id: 0,
            payload: &[],
        }),
        vosu_status::VOSU_ERR_NOT_READY
    );
}

#[test]
fn failed_reports_its_stage_and_the_child_is_reaped() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    harness.expect_create();
    let failed = Failed {
        generation: 0,
        stage: StartupStage::Security,
        code: "security_unavailable".into(),
        errno: Some(1),
        message: "seccomp: refused".into(),
    };
    harness
        .child
        .sender
        .send(&private(op::FAILED, 0), &failed.encode())
        .unwrap();
    let failure = failure(harness.launch());
    assert_eq!(failure.stage, StartupStage::Security);
    assert_eq!(failure.code, "security_unavailable");
    assert_eq!(failure.errno, Some(1));
    assert!(harness.losses().is_empty());
    assert_eq!(harness.process.kills(), 1);
}

#[test]
fn an_exit_before_ready_names_the_pre_channel_stage() {
    for (status, stage, code) in [
        (
            i64::from(control::EXIT_GENERATION),
            StartupStage::Generation,
            "executable_updated",
        ),
        (
            i64::from(control::EXIT_ARGUMENTS),
            StartupStage::Arguments,
            "arguments",
        ),
        (
            i64::from(control::EXIT_PARENT),
            StartupStage::Parent,
            "parent",
        ),
        (
            i64::from(control::EXIT_ENDPOINTS),
            StartupStage::Endpoints,
            "endpoints",
        ),
        (0, StartupStage::Crash, "exited"),
        (-11, StartupStage::Crash, "exited"),
    ] {
        let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
        harness.process.exit(status);
        let failure = failure(harness.launch());
        assert_eq!(
            (failure.stage, failure.code.as_str()),
            (stage, code),
            "{status}"
        );
        assert!(harness.losses().is_empty());
    }
}

#[test]
fn a_silent_child_is_killed_at_the_startup_deadline() {
    let mut harness = start_harness(vosu_status::VOSU_OK, Duration::from_millis(200));
    harness.expect_create();
    let started = Instant::now();
    let failure = failure(harness.launch());
    assert_eq!(failure.stage, StartupStage::Timeout);
    assert!(started.elapsed() < BOUND);
    assert_eq!(harness.process.kills(), 1);
    assert!(harness.losses().is_empty());
}

#[test]
fn ready_for_another_generation_or_abi_fails_startup() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    harness.expect_create();
    harness.send_ready(Ready {
        generation: GENERATION + 1,
        struct_size: 4096,
        abi_version: 1,
    });
    assert_eq!(failure(harness.launch()).stage, StartupStage::Crash);
    assert_eq!(harness.process.kills(), 1);

    let mut second = start_harness(vosu_status::VOSU_OK, BOUND);
    second.expect_create();
    second.send_ready(Ready {
        generation: GENERATION,
        struct_size: 4096,
        abi_version: 2,
    });
    assert_eq!(failure(second.launch()).stage, StartupStage::Abi);
}

#[test]
fn an_unacknowledged_status_frame_is_one_channel_loss() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    let endpoint = harness.ready();
    let (header, payload) = public(&timer_set());
    harness.child.sender.send(&header, &payload).unwrap();
    assert_eq!(harness.handler.wait_for(&harness.handler.losses, 1), 1);
    assert_eq!(harness.losses(), vec![LossCause::ChannelFailed]);
    // The channel failure killed the child; its exit is not a second loss.
    assert_eq!(harness.process.kills(), 1);
    endpoint.terminate();
    assert_eq!(harness.losses(), vec![LossCause::ChannelFailed]);
}

#[test]
fn an_unexpected_exit_after_ready_is_a_loss_even_with_status_zero() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    let endpoint = harness.ready();
    harness.process.exit(0);
    assert_eq!(harness.handler.wait_for(&harness.handler.losses, 1), 1);
    assert_eq!(
        harness.losses(),
        vec![LossCause::ProcessExited { status: Some(0) }]
    );
    assert_eq!(
        endpoint.dispatch(MessageRef {
            opcode: vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32,
            flags: 0,
            correlation_id: 0,
            payload: &[],
        }),
        vosu_status::VOSU_ERR_NOT_READY
    );
    endpoint.terminate();
    assert_eq!(harness.losses().len(), 1);
}

#[test]
fn destroy_past_its_grace_kills_and_reaps() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    let endpoint = harness.ready();
    let started = Instant::now();
    let result = endpoint.destroy(Duration::from_millis(300));
    assert!(
        matches!(result, Err(HostError::DestroyTimedOut)),
        "{result:?}"
    );
    assert!(started.elapsed() >= Duration::from_millis(300));
    assert_eq!(harness.recv(), private(op::DESTROY, 0));
    assert_eq!(harness.process.kills(), 1);
    assert!(
        harness.losses().is_empty(),
        "controlled teardown is no loss"
    );
}

#[test]
fn destroyed_without_destroy_is_a_protocol_loss() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    let _endpoint = harness.ready();
    harness
        .child
        .sender
        .send(&private(op::DESTROYED, 0), &[])
        .unwrap();
    assert_eq!(harness.handler.wait_for(&harness.handler.losses, 1), 1);
    assert_eq!(harness.losses(), vec![LossCause::ChannelFailed]);
}

#[test]
fn a_second_ready_is_a_protocol_loss() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    let _endpoint = harness.ready();
    harness.send_ready(Ready {
        generation: GENERATION,
        struct_size: 4096,
        abi_version: 1,
    });
    assert_eq!(harness.handler.wait_for(&harness.handler.losses, 1), 1);
}

#[test]
fn core_never_dispatches_a_private_opcode() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    let endpoint = harness.ready();
    assert_eq!(
        endpoint.dispatch(MessageRef {
            opcode: op::DESTROY,
            flags: 0,
            correlation_id: 0,
            payload: &[],
        }),
        vosu_status::VOSU_ERR_UNSUPPORTED
    );
    // Nothing was written: the next frame the child sees is the real one.
    endpoint.dispatch(MessageRef {
        opcode: vosu_opcode::VOSU_OP_TIMER_FIRED as u32,
        flags: 0,
        correlation_id: 0,
        payload: &[],
    });
    assert_eq!(
        harness.recv().opcode,
        vosu_opcode::VOSU_OP_TIMER_FIRED as u32
    );
}

fn sync_submit(plan: u64) -> (FrameHeader, Vec<u8>) {
    let message = Message::ReadPlanSubmit(ReadPlanSubmit {
        plan,
        handle: 1,
        root: PlanBase::Absolute(0),
        stages: Vec::new(),
        sync: true,
    });
    (
        FrameHeader {
            opcode: message.opcode() as u32,
            flags: VOSU_READ_PLAN_SYNC,
            correlation: 0,
        },
        message.encode_payload().unwrap(),
    )
}

/// T1/B0 transport: a SYNC plan is handled on its own worker and answered
/// with `REPLY_NEXT` plus the unmodified result, while the ordinary lane
/// is blocked in another request's handler.
#[test]
fn a_sync_plan_is_answered_by_reply_next_while_ordinary_handling_is_blocked() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    let endpoint = harness.ready();
    harness
        .handler
        .endpoint
        .set(Arc::downgrade(&endpoint))
        .unwrap();
    let gate = Gate::new();
    *harness.handler.hook.lock().unwrap() = Some(Box::new({
        let gate = Arc::clone(&gate);
        move |message| {
            if matches!(message, Message::TimerSet(_)) {
                gate.wait();
            }
        }
    }));
    let (header, payload) = public(&timer_set());
    harness
        .child
        .sender
        .send_acknowledged(1, &header, &payload)
        .unwrap();
    assert!(gate.wait_arrivals(1, BOUND), "the ordinary lane is blocked");
    let (header, payload) = sync_submit(42);
    harness
        .child
        .sender
        .send_acknowledged(2, &header, &payload)
        .unwrap();
    assert_eq!(harness.recv(), private(op::REPLY_NEXT, 2));
    let result = harness.recv();
    assert_eq!(result.opcode, vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32);
    assert_eq!(
        harness.buffer.bytes(),
        sync_result(42).encode_payload().unwrap(),
        "the result crosses byte for byte"
    );
    gate.open();
    assert_eq!(harness.recv(), private(op::STATUS, 1));
    assert!(harness.losses().is_empty());
    endpoint.terminate();
}

/// §7.6: an accepted request's `STATUS` precedes every result it causes.
/// A frame another thread dispatches while the request is still being
/// handled waits for that `STATUS`.
#[test]
fn a_status_precedes_the_frames_dispatched_while_its_request_is_handled() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    let endpoint = harness.ready();
    let finished_early = Arc::new(Mutex::new(None));
    *harness.handler.hook.lock().unwrap() = Some(Box::new({
        let endpoint = Arc::clone(&endpoint);
        let finished_early = Arc::clone(&finished_early);
        move |_| {
            let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
            let endpoint = Arc::clone(&endpoint);
            std::thread::spawn(move || {
                endpoint.dispatch(MessageRef {
                    opcode: vosu_opcode::VOSU_OP_TIMER_FIRED as u32,
                    flags: 0,
                    correlation_id: 0,
                    payload: &[],
                });
                let _ = done_tx.send(());
            });
            // Without the gate the dispatch above would finish at once.
            *finished_early.lock().unwrap() =
                Some(done_rx.recv_timeout(Duration::from_millis(300)).is_ok());
        }
    }));
    let (header, payload) = public(&timer_set());
    harness
        .child
        .sender
        .send_acknowledged(1, &header, &payload)
        .unwrap();
    assert_eq!(harness.recv(), private(op::STATUS, 1));
    assert_eq!(
        harness.recv().opcode,
        vosu_opcode::VOSU_OP_TIMER_FIRED as u32
    );
    assert_eq!(*finished_early.lock().unwrap(), Some(false));
    *harness.handler.hook.lock().unwrap() = None;
    endpoint.terminate();
}

/// The ledger closes each request before its `STATUS` is written: a child
/// that reuses each freed waiter at once never meets the 64 cap.
#[test]
fn a_freed_request_can_be_reused_at_once_without_a_cap_violation() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    let endpoint = harness.ready();
    let gate = Gate::new();
    *harness.handler.hook.lock().unwrap() = Some(Box::new({
        let gate = Arc::clone(&gate);
        move |_| gate.wait()
    }));
    let (header, payload) = public(&timer_set());
    for request in 1..=64 {
        harness
            .child
            .sender
            .send_acknowledged(request, &header, &payload)
            .unwrap();
    }
    assert!(gate.wait_arrivals(1, BOUND));
    gate.open();
    for next in 65..=300 {
        assert_eq!(harness.recv().opcode, op::STATUS);
        harness
            .child
            .sender
            .send_acknowledged(next, &header, &payload)
            .unwrap();
    }
    for _ in 0..64 {
        assert_eq!(harness.recv().opcode, op::STATUS);
    }
    assert!(harness.losses().is_empty(), "{:?}", harness.losses());
    endpoint.terminate();
}

/// A child that skips its own 64-waiter refusal breaks the protocol.
#[test]
fn a_sixty_fifth_unanswered_request_is_one_channel_loss() {
    let mut harness = start_harness(vosu_status::VOSU_OK, BOUND);
    let endpoint = harness.ready();
    let gate = Gate::new();
    *harness.handler.hook.lock().unwrap() = Some(Box::new({
        let gate = Arc::clone(&gate);
        move |_| gate.wait()
    }));
    let (header, payload) = public(&timer_set());
    for request in 1..=65 {
        harness
            .child
            .sender
            .send_acknowledged(request, &header, &payload)
            .unwrap();
    }
    assert_eq!(harness.handler.wait_for(&harness.handler.losses, 1), 1);
    assert_eq!(harness.losses(), vec![LossCause::ChannelFailed]);
    gate.open();
    endpoint.terminate();
}

/// M5 B0 counterexample: a result a broker worker computed for one
/// generation's `SYNC` request never reaches another generation's child,
/// even when the host routes it to the newer endpoint (an instance id the
/// newer generation reuses). The old request is answered on its own
/// channel; the new child receives nothing for it.
#[test]
fn read_plan_sync_result_of_an_old_generation_never_reaches_a_new_child() {
    let mut old = start_harness(vosu_status::VOSU_OK, BOUND);
    let old_endpoint = old.ready();
    let mut new = start_harness(vosu_status::VOSU_OK, BOUND);
    let new_endpoint = new.ready();
    old.handler
        .endpoint
        .set(Arc::downgrade(&new_endpoint))
        .unwrap();
    let (header, payload) = sync_submit(7);
    old.child
        .sender
        .send_acknowledged(1, &header, &payload)
        .unwrap();
    assert_eq!(old.recv(), private(op::STATUS, 1));
    assert_eq!(
        old.buffer.bytes(),
        (vosu_status::VOSU_ERR_BACKEND as u32).to_le_bytes(),
        "the old request is refused, never answered OK without its result"
    );
    let (header, payload) = public(&timer_set());
    new.child
        .sender
        .send_acknowledged(1, &header, &payload)
        .unwrap();
    assert_eq!(
        new.recv(),
        private(op::STATUS, 1),
        "the new child's next frame is its own STATUS, never the stale result"
    );
    assert!(old.losses().is_empty() && new.losses().is_empty());
    old_endpoint.terminate();
    new_endpoint.terminate();
}

/// M5 B0: a `SYNC` plan's result is written exactly once even when the
/// budget timer races it: here the timer wins, and the late result is
/// discarded without a `REPLY_NEXT`.
#[test]
fn read_plan_sync_result_after_core_timed_out_is_discarded() {
    let (core, child) = pair();
    let process = Arc::new(ScriptedProcess::default());
    let handler = Handler::new(vosu_status::VOSU_OK);
    let dyn_handler: Arc<dyn PluginHostHandler> = handler.clone();
    let id = PluginInstanceId::new(6);
    let create = Create {
        generation: GENERATION,
        plugin_id: "acme.fixture".to_owned(),
        library: "/opt/acme/lib.so".into(),
        library_sha256: [7; 32],
        concurrent_dispatch: false,
        sync_budget_ms: 100,
        init: vosu_plugin_api::Init {
            plugin_id: "acme.fixture".to_owned(),
            settings: CborValue::Map(Vec::new()),
            data_dir: "/tmp".into(),
        },
    }
    .encode()
    .unwrap();
    let start = Start {
        generation: GENERATION,
        plugin_id: "acme.fixture".to_owned(),
        create,
        deadline: Instant::now() + BOUND,
        ingress: PluginIngress::new(id, Arc::clone(&dyn_handler)),
        loss: InstanceLoss::new(id, LossState::new(), &dyn_handler),
        sync_budget: Duration::from_millis(100),
    };
    let started = Started {
        endpoint: core,
        process: process.clone(),
    };
    let mut harness = Harness {
        launch: Some(std::thread::spawn(move || {
            supervision::start(started, start)
        })),
        child,
        process,
        handler,
        buffer: PayloadBuffer::new(),
    };
    let endpoint = harness.ready();
    harness
        .handler
        .endpoint
        .set(Arc::downgrade(&endpoint))
        .unwrap();
    let gate = Gate::new();
    *harness.handler.sync_hold.lock().unwrap() = Some(Arc::clone(&gate));
    // The handler's SYNC path blocks first (a plan stuck in one native
    // read), then dispatches its result.
    let (header, payload) = sync_submit(9);
    harness
        .child
        .sender
        .send_acknowledged(1, &header, &payload)
        .unwrap();
    assert_eq!(harness.recv(), private(op::STATUS, 1));
    assert_eq!(
        harness.buffer.bytes(),
        (vosu_status::VOSU_ERR_TIMED_OUT as u32).to_le_bytes()
    );
    gate.open();
    assert_eq!(
        harness.handler.wait_for(&harness.handler.sync_done, 1),
        1,
        "the late result was dispatched to the endpoint"
    );
    let (header, payload) = public(&timer_set());
    harness
        .child
        .sender
        .send_acknowledged(2, &header, &payload)
        .unwrap();
    assert_eq!(
        harness.recv(),
        private(op::STATUS, 2),
        "no REPLY_NEXT follows the timer's answer"
    );
    assert!(harness.losses().is_empty());
    endpoint.terminate();
}
