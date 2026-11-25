//! Core's supervision state machine over a real channel (memfd/eventfd on
//! Linux, section/event on Windows) with the child played by the test and
//! a scripted process handle: startup outcomes, acknowledged sends, loss
//! identity and controlled destroy. The real child is exercised natively
//! by the root `plugin_host_native` target.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vosu_plugin_api::{
    CborValue, LogLevel, LogRecord, Message, TimerSet, vosu_opcode, vosu_status,
};
use vosu_sys::plugin_process::RingObjects;

use super::super::channel::{Endpoint, connect};
use super::super::control::{self, Create, Failed, Ready, StartupFailure, StartupStage};
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
        Box::new(DirectionPolicy::new(Direction::PluginToHost)),
    )
    .unwrap();
    let child = connect(
        child_to_host.into(),
        child_to_plugin.into(),
        Box::new(DirectionPolicy::new(Direction::HostToPlugin)),
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

/// Core's handler: answers every message with `status` and records losses.
struct Handler {
    status: vosu_status,
    messages: Mutex<Vec<(Message, u64)>>,
    losses: Mutex<Vec<LossCause>>,
    changed: Condvar,
}

impl Handler {
    fn new(status: vosu_status) -> Arc<Self> {
        Arc::new(Self {
            status,
            messages: Mutex::new(Vec::new()),
            losses: Mutex::new(Vec::new()),
            changed: Condvar::new(),
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
        self.messages
            .lock()
            .unwrap()
            .push((message, correlation_id));
        self.changed.notify_all();
        self.status
    }

    fn instance_lost(&self, _: PluginInstanceId, cause: LossCause) {
        self.losses.lock().unwrap().push(cause);
        self.changed.notify_all();
    }
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
