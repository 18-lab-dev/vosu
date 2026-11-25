//! M5 B0: the production read-plan broker across the process boundary
//! (`docs/v2/plugin-abi.md` §5, §6.3–§6.4, §7.6 "Brokered reads"), on real
//! processes: the real `vosu plugin-host` child started by core's real
//! launcher, a real `PluginHost`, and real x64 and x86 target processes
//! ([`targets`]) whose memory core reads through the operating system.
//!
//! Plugins are P0's testkit (its read receipts prove which target each
//! instance read) and B0's C fixture ([`fixture`]), which `send`s exact
//! request bytes the test writes and records the status, the result its
//! submitting thread received, and every byte each ring carried meanwhile.
//!
//! Most cases let core open the target itself. The cases that must prove
//! "no read happened" or hold an execution in flight wrap the same real OS
//! backend in a [`Probe`] installed through core's development memory hook:
//! the reads are still real reads of the real target, counted and gated.

#[path = "broker/fixture.rs"]
mod fixture;
#[path = "broker/targets.rs"]
mod targets;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use fixture::Frame;
use targets::{Target, Width};
use vosu_core::plugin::LoadedPlugin;
use vosu_core::plugin::test_support::write_development_package;
use vosu_plugin_api::memory::{MemoryError, MemoryRegion, ProcessMemory, ReadRequest};
use vosu_plugin_api::{
    DemandChanged, FocusChanged, Message, PlanBase, PlanOp, ReadPlanCancel, ReadPlanResult,
    ReadPlanSubmit, ReadResult, Stage, StageBase, VOSU_OPCODE_PRIVATE_BIT, VOSU_READ_PLAN_SYNC,
    vosu_opcode, vosu_status,
};

use super::support::artifacts::Artifact;
use super::support::fixture::Fixture;
use super::support::host::{Host, Identity, wait_until};

const BOUND: Duration = Duration::from_secs(20);
const SUBMIT: u32 = vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT as u32;
const RESULT: u32 = vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32;
const OK: u32 = vosu_status::VOSU_OK as u32;
const INVALID_ARGUMENT: u32 = vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32;
const PERMISSION_DENIED: u32 = vosu_status::VOSU_ERR_PERMISSION_DENIED as u32;
const TARGET_GONE: u32 = vosu_status::VOSU_ERR_TARGET_GONE as u32;

/// What one fixture request observed.
#[derive(Debug)]
struct Response {
    child: u32,
    status: u32,
    /// `1`: the result reached the submitting thread, aligned; `0`: none.
    same_thread: String,
    result: Option<Vec<u8>>,
    /// The frames each ring carried during the `send`; `None` in-process.
    p2h: Option<Vec<Frame>>,
    h2p: Option<Vec<Frame>>,
}

impl Response {
    fn parse(child: u32, text: &str) -> Self {
        let optional = |key| Some(fixture::field(text, key)).filter(|value| value != "-");
        let census = |key| {
            optional(key).map(|census| {
                assert_ne!(census, "overflow", "{key} census over its cap: {text}");
                fixture::frames(&census)
            })
        };
        Self {
            child,
            status: fixture::field(text, "status").parse().unwrap(),
            same_thread: fixture::field(text, "same_thread"),
            result: optional("result").map(|hex| fixture::hex(&hex)),
            p2h: census("p2h"),
            h2p: census("h2p"),
        }
    }

    /// The decoded result the submitting thread received.
    fn plan_result(&self) -> ReadPlanResult {
        let bytes = self.result.as_ref().expect("no result reached the thread");
        match Message::decode(vosu_opcode::VOSU_OP_READ_PLAN_RESULT, 0, bytes).unwrap() {
            Message::ReadPlanResult(result) => result,
            other => panic!("{other:?}"),
        }
    }

    /// A refusal: `status`, and nothing delivered to the thread.
    fn assert_refused(&self, status: u32, case: &str) {
        assert_eq!(self.status, status, "{case}: {self:?}");
        assert_eq!(self.same_thread, "0", "{case}: a refused plan answered");
        assert!(self.result.is_none(), "{case}: a refused plan answered");
    }
}

/// One B0 fixture package and its markers.
struct Plugin {
    markers: PathBuf,
    loaded: LoadedPlugin,
    next: Cell<u16>,
}

impl Plugin {
    /// A package `test.b0.<name>` of the fixture under `runtime`, matching
    /// processes named `process`.
    fn new(root: &Path, host: &Host, name: &str, runtime: &str, process: &str) -> Self {
        let base = fixture::dir(root, name);
        let markers = fixture::dir(&base, "markers");
        let library = fixture::build(&fixture::dir(&base, "build"), &markers);
        let package = base.join("package");
        write_development_package(
            &package,
            &format!("test.b0.{name}"),
            process,
            runtime,
            fixture::library_name(),
            &library,
        )
        .unwrap();
        Self {
            markers,
            loaded: host.load(&package),
            next: Cell::new(1),
        }
    }

    /// Has instance `id` send `message` from `mode`'s thread; returns the
    /// request number without waiting.
    fn start(&self, host: &Host, id: u64, mode: &str, message: &Message) -> u16 {
        let n = self.next.get();
        self.next.set(n + 1);
        fixture::write_request(
            &self.markers,
            n,
            mode,
            message.opcode() as u32,
            message.flags(),
            &message.encode_payload().unwrap(),
        );
        self.trigger(host, id, n);
        n
    }

    fn trigger(&self, host: &Host, id: u64, n: u16) {
        host.dispatch(
            id,
            &Message::DemandChanged(DemandChanged {
                handle: 0xB0DE_0000 | u64::from(n),
                regular: false,
                precise: false,
                selectors: Vec::new(),
            }),
        )
        .expect("core accepted the trigger");
    }

    fn response(&self, n: u16) -> Response {
        let (child, text) = fixture::wait_marker(&self.markers, &format!("response-{n}"), BOUND)
            .unwrap_or_else(|| panic!("request {n} never returned"));
        Response::parse(child, &text)
    }

    /// [`Self::start`], then waits for its response.
    fn send(&self, host: &Host, id: u64, mode: &str, message: &Message) -> Response {
        let n = self.start(host, id, mode, message);
        self.response(n)
    }

    /// Ends instance `id`'s plugin host abruptly.
    fn exit(&self, host: &Host, id: u64) {
        let n = self.next.get();
        self.next.set(n + 1);
        fixture::write_request(&self.markers, n, "exit", 0, 0, &[]);
        self.trigger(host, id, n);
    }

    /// The logged messages of `child`, decoded.
    fn log(&self, child: u32) -> Vec<Message> {
        fixture::log(&self.markers, child)
            .into_iter()
            .map(|(opcode, _, payload)| {
                Message::decode(vosu_opcode::try_from(opcode).unwrap(), 0, &payload).unwrap()
            })
            .collect()
    }

    /// The asynchronous plan results `child` received, in order.
    fn results(&self, child: u32) -> Vec<ReadPlanResult> {
        self.log(child)
            .into_iter()
            .filter_map(|message| match message {
                Message::ReadPlanResult(result) => Some(result),
                _ => None,
            })
            .collect()
    }

    /// Waits for `count` asynchronous results in `child`.
    fn wait_results(&self, child: u32, count: usize) -> Vec<ReadPlanResult> {
        assert!(
            wait_until(BOUND, || self.results(child).len() >= count),
            "{:?}",
            self.results(child)
        );
        self.results(child)
    }

    /// The `(child, handle)` of a session on `target` attached to a process
    /// not in `exclude`.
    fn session(&self, target: &Target, exclude: &[u32]) -> (u32, u64) {
        let find = || {
            fixture::logging_processes(&self.markers)
                .into_iter()
                .filter(|child| !exclude.contains(child))
                .find_map(|child| {
                    self.log(child)
                        .into_iter()
                        .find_map(|message| match message {
                            Message::SessionAttached(attached) if attached.pid == target.pid => {
                                assert_eq!(attached.bitness, target.width.bitness());
                                Some((child, attached.handle))
                            }
                            _ => None,
                        })
                })
        };
        assert!(
            wait_until(BOUND, || find().is_some()),
            "{target:?} never attached"
        );
        find().unwrap()
    }

    fn wait_detached(&self, child: u32, handle: u64) {
        let detached = || {
            self.log(child).iter().any(|message| {
                matches!(message, Message::SessionDetached(detached) if detached.handle == handle)
            })
        };
        assert!(
            wait_until(BOUND, detached),
            "session {handle} never detached"
        );
    }
}

/// Real OS memory of one target, counted and optionally held.
struct Probe {
    inner: Box<dyn ProcessMemory>,
    state: Mutex<ProbeState>,
    changed: Condvar,
}

#[derive(Default)]
struct ProbeState {
    held: bool,
    /// Reads currently inside the probe.
    inside: usize,
    /// Read calls (one per batch) that ever entered.
    entered: usize,
}

impl Probe {
    fn open(pid: u32) -> Arc<Self> {
        #[cfg(target_os = "linux")]
        let inner = Box::new(vosu_sys::process::open_process(pid).unwrap());
        #[cfg(windows)]
        let inner = Box::new(vosu_sys::process::WindowsProcessMemory::open(pid).unwrap());
        Arc::new(Self {
            inner,
            state: Mutex::default(),
            changed: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, ProbeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_held(&self, held: bool) {
        self.lock().held = held;
        self.changed.notify_all();
    }

    fn entered(&self) -> usize {
        self.lock().entered
    }

    /// Waits until `count` reads are inside the probe.
    fn wait_inside(&self, count: usize) {
        let state = self.lock();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, BOUND, |state| state.inside != count)
            .unwrap_or_else(PoisonError::into_inner);
        assert_eq!(state.inside, count, "reads inside the probe");
    }

    /// Counts one read and waits while the probe is held.
    fn enter(&self) {
        let mut state = self.lock();
        state.entered += 1;
        state.inside += 1;
        self.changed.notify_all();
        let (mut state, _) = self
            .changed
            .wait_timeout_while(state, BOUND, |state| state.held)
            .unwrap_or_else(PoisonError::into_inner);
        state.inside -= 1;
        self.changed.notify_all();
    }
}

impl ProcessMemory for Probe {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.enter();
        self.inner.read_exact(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.enter();
        self.inner.read_many(reads)
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        self.inner.readable_regions()
    }

    fn process_id(&self) -> Option<u32> {
        self.inner.process_id()
    }

    fn is_alive(&self) -> bool {
        self.inner.is_alive()
    }

    fn reads_pin_process_identity(&self) -> bool {
        self.inner.reads_pin_process_identity()
    }
}

/// Hooks `target`'s real memory through a new [`Probe`] and offers it as
/// `process`.
fn probe(host: &Host, target: &Target, process: &str) -> Arc<Probe> {
    let probe = Probe::open(target.pid);
    host.hook_memory(target.pid, probe.clone(), target.width.process_bitness());
    host.offer_process(target.pid, process, target.width.bitness());
    probe
}

fn submit(plan: u64, handle: u64, root: u64, stages: Vec<Stage>, sync: bool) -> Message {
    Message::ReadPlanSubmit(ReadPlanSubmit {
        plan,
        handle,
        root: PlanBase::Absolute(root),
        stages,
        sync,
    })
}

/// One stage reading `length` bytes at the root.
fn single(length: u32) -> Vec<Stage> {
    vec![Stage {
        base: StageBase::Root,
        base_offset: 0,
        ops: vec![PlanOp { offset: 0, length }],
    }]
}

/// `root[0] → mid[0] → tag`, references `width` bytes wide; the last stage
/// reads the tag in two halves.
fn chain(width: u32) -> Vec<Stage> {
    let pointer = |base| Stage {
        base,
        base_offset: 0,
        ops: vec![PlanOp {
            offset: 0,
            length: width,
        }],
    };
    let dependent = |ref_stage| StageBase::Dependent {
        ref_stage,
        ref_op: 0,
    };
    vec![
        pointer(StageBase::Root),
        pointer(dependent(0)),
        Stage {
            base: dependent(1),
            base_offset: 0,
            ops: vec![
                PlanOp {
                    offset: 0,
                    length: 8,
                },
                PlanOp {
                    offset: 8,
                    length: 8,
                },
            ],
        },
    ]
}

/// The chain's logical result on `target`.
fn chain_result(plan: u64, target: &Target) -> ReadPlanResult {
    let width = target.width.bytes() as usize;
    let tag = target.tag();
    ReadPlanResult {
        plan,
        results: vec![
            ReadResult::Ok(target.mid.to_le_bytes()[..width].to_vec()),
            ReadResult::Ok(target.tag_address.to_le_bytes()[..width].to_vec()),
            ReadResult::Ok(tag[..8].to_vec()),
            ReadResult::Ok(tag[8..].to_vec()),
        ],
    }
}

fn bytes_result(plan: u64, bytes: &[u8]) -> ReadPlanResult {
    ReadPlanResult {
        plan,
        results: vec![ReadResult::Ok(bytes.to_vec())],
    }
}

/// The public (non-private) frames, without the correlation word the
/// transport owns: `(opcode, flags, payload)`.
fn public(frames: &[Frame]) -> Vec<(u32, u32, Vec<u8>)> {
    frames
        .iter()
        .filter(|(opcode, ..)| opcode & VOSU_OPCODE_PRIVATE_BIT == 0)
        .map(|(opcode, flags, _, payload)| (*opcode, *flags, payload.clone()))
        .collect()
}

fn opcodes(frames: &[Frame]) -> Vec<u32> {
    frames.iter().map(|(opcode, ..)| *opcode).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// B0 counterexample: a multi-stage plan counted as several transport
/// submissions. A three-stage dependent chain on a real x64 and a real x86
/// target crosses the rings exactly once each way — the one unmodified
/// `READ_PLAN_SUBMIT` out, the one `READ_PLAN_RESULT` back to the
/// submitting thread — with the same frames as a one-stage plan, and with
/// the direct (in-process) runtime's exact result bytes. The asynchronous
/// chain delivers the same logical result.
#[test]
fn broker_a_dependent_chain_is_one_transport_submission_with_the_direct_bytes() {
    let root = super::support::scratch();
    // One host per runtime: two plugins of one host may not match the same
    // process name, and both runtimes read the same two targets.
    let direct_host = Host::new(&fixture::dir(root.path(), "direct-host"));
    let host = Host::new(&fixture::dir(root.path(), "process-host"));
    let build = fixture::dir(root.path(), "targets");
    let process = format!("b0-chain-{}", std::process::id());
    let direct = Plugin::new(
        root.path(),
        &direct_host,
        "chain-direct",
        "in-process",
        &process,
    );
    let brokered = Plugin::new(root.path(), &host, "chain-process", "process", &process);
    direct_host.create(1, &direct.loaded).unwrap();
    host.create(2, &brokered.loaded).unwrap();
    let targets = [
        Target::start(&build, Width::X64, 31),
        Target::start(&build, Width::X86, 32),
    ];
    for target in &targets {
        direct_host.offer_process(target.pid, &process, target.width.bitness());
        host.offer_process(target.pid, &process, target.width.bitness());
    }
    for target in &targets {
        let width = target.width.bytes();
        let (direct_child, direct_handle) = direct.session(target, &[]);
        assert_eq!(direct_child, std::process::id());
        let (child, handle) = brokered.session(target, &[]);
        assert_ne!(child, std::process::id(), "the plugin ran in core");

        let mut shapes = Vec::new();
        let mut chained = Vec::new();
        for (plan, stages) in [(1, single(width)), (2, chain(width))] {
            let message = submit(plan, handle, target.root, stages.clone(), true);
            let response = brokered.send(&host, 2, "thread", &message);
            assert_eq!(response.status, OK, "{target:?}: {response:?}");
            assert_eq!(response.same_thread, "1", "{target:?}: {response:?}");
            let p2h = response.p2h.clone().expect("a census in the plugin host");
            let h2p = response.h2p.clone().expect("a census in the plugin host");
            println!(
                "RECORD broker {target:?} stages={} p2h={:x?} h2p={:x?}",
                stages.len(),
                opcodes(&p2h),
                opcodes(&h2p)
            );
            assert_eq!(
                public(&p2h),
                [(
                    SUBMIT,
                    VOSU_READ_PLAN_SYNC,
                    message.encode_payload().unwrap()
                )],
                "{target:?}: one unmodified submission crossed to core"
            );
            assert_eq!(
                public(&h2p),
                [(RESULT, 0, response.result.clone().unwrap())],
                "{target:?}: one unmodified result crossed back"
            );
            shapes.push((opcodes(&p2h), opcodes(&h2p)));
            if plan == 2 {
                assert_eq!(response.plan_result(), chain_result(2, target));
                let direct_response = direct.send(
                    &direct_host,
                    1,
                    "thread",
                    &submit(2, direct_handle, target.root, stages, true),
                );
                assert_eq!(direct_response.status, OK);
                assert_eq!(direct_response.same_thread, "1");
                assert!(
                    direct_response.p2h.is_none(),
                    "the direct runtime has no rings"
                );
                chained.push(response.result.clone().unwrap());
                chained.push(direct_response.result.unwrap());
            }
        }
        assert_eq!(
            shapes[0], shapes[1],
            "{target:?}: three stages cost the transport what one stage does"
        );
        assert_eq!(
            chained[0], chained[1],
            "{target:?}: the brokered chain's bytes are the direct runtime's"
        );

        let before = brokered.results(child).len();
        let response = brokered.send(
            &host,
            2,
            "dispatch",
            &submit(3, handle, target.root, chain(width), false),
        );
        assert_eq!(response.status, OK, "{target:?}: {response:?}");
        let sent: Vec<u32> = public(response.p2h.as_ref().unwrap())
            .into_iter()
            .map(|(opcode, ..)| opcode)
            .collect();
        assert_eq!(sent, [SUBMIT], "{target:?}: one asynchronous submission");
        let results = brokered.wait_results(child, before + 1);
        assert_eq!(results[before..], [chain_result(3, target)], "{target:?}");
    }
}

/// B0 counterexamples: the same numeric handle used by another instance
/// reads that instance's target, and an arbitrary PID is accepted as a
/// handle. Two testkit instances, each matching only its own real target
/// (x64 and x86, distinct bytes), read exactly their own bytes (P0 read
/// receipts). A third instance, owning a third target, is refused both
/// testkit handles (`VOSU_ERR_PERMISSION_DENIED`) and every target's PID
/// (`VOSU_ERR_INVALID_ARGUMENT`, never issued), SYNC and asynchronous,
/// with no result; its own handle reads its own bytes.
#[test]
fn broker_foreign_handles_and_raw_pids_never_read_another_target() {
    let root = super::support::scratch();
    let host = Host::new(root.path());
    let build = fixture::dir(root.path(), "targets");
    let a = Target::start(&build, Width::X64, 41);
    let b = Target::start(&build, Width::X86, 42);
    let c = Target::start(&build, Width::X64, 43);
    assert!(a.tag() != b.tag() && b.tag() != c.tag() && a.tag() != c.tag());

    let mut testkits = Vec::new();
    for (index, (name, target)) in [("a", &a), ("b", &b)].into_iter().enumerate() {
        let id = format!("test.p0.testkit.b0{name}");
        let process = format!("vosu-p0-target-b0{name}");
        let staged = Artifact::Testkit.stage(
            &root.path().join(format!("package-{name}")),
            &[
                ("test.p0.testkit", id.as_str()),
                ("vosu-p0-target", process.as_str()),
            ],
        );
        let receipts = Fixture::new(&root.path().join(format!("receipts-{name}")));
        receipts.control(&staged, "none", None, Some((target.tag_address, 16)));
        host.create(index as u64 + 1, &host.load(&staged.dir))
            .unwrap();
        testkits.push((receipts, process, target));
    }
    let process = format!("b0-owner-{}", std::process::id());
    let owner = Plugin::new(root.path(), &host, "owner", "process", &process);
    host.create(3, &owner.loaded).unwrap();
    for (_, name, target) in &testkits {
        host.offer_process(target.pid, name, target.width.bitness());
    }
    host.offer_process(c.pid, &process, c.width.bitness());

    let mut foreign = Vec::new();
    for (receipts, _, target) in &testkits {
        let attach = receipts.wait("attach", BOUND).expect("SESSION_ATTACHED");
        assert_eq!(attach.get("target"), target.pid.to_string());
        assert_eq!(attach.get("submit"), "0", "the read plan was refused");
        let read = receipts.wait("read", BOUND).expect("READ_PLAN_RESULT");
        assert_eq!(read.pid, attach.pid);
        assert_eq!(read.get("result"), "ok");
        assert_eq!(
            read.get("bytes"),
            hex(&target.tag()),
            "{target:?}: another target's bytes"
        );
        foreign.push((attach.get("handle").parse::<u64>().unwrap(), *target));
    }
    assert_ne!(
        testkits[0].0.wait("read", BOUND).unwrap().pid,
        testkits[1].0.wait("read", BOUND).unwrap().pid,
        "two instances shared one plugin host"
    );

    let (child, own) = owner.session(&c, &[]);
    let response = owner.send(
        &host,
        3,
        "thread",
        &submit(1, own, c.tag_address, single(16), true),
    );
    assert_eq!(response.status, OK, "{response:?}");
    assert_eq!(response.plan_result(), bytes_result(1, &c.tag()));

    let handles: Vec<u64> = foreign
        .iter()
        .map(|(handle, _)| *handle)
        .chain([own])
        .collect();
    for sync in [true, false] {
        let mode = if sync { "thread" } else { "dispatch" };
        for (handle, target) in &foreign {
            let message = submit(2, *handle, target.tag_address, single(16), sync);
            owner.send(&host, 3, mode, &message).assert_refused(
                PERMISSION_DENIED,
                &format!("{target:?}'s handle {handle}, sync={sync}"),
            );
        }
        for target in [&a, &b, &c] {
            let pid = u64::from(target.pid);
            assert!(
                !handles.contains(&pid),
                "a PID coincides with an issued handle"
            );
            let message = submit(3, pid, target.tag_address, single(16), sync);
            owner
                .send(&host, 3, mode, &message)
                .assert_refused(INVALID_ARGUMENT, &format!("{target:?}'s PID, sync={sync}"));
        }
    }

    // An accepted asynchronous plan after every refusal: its result is the
    // only one, so no refused plan produced a later result.
    let response = owner.send(
        &host,
        3,
        "dispatch",
        &submit(4, own, c.tag_address, single(16), false),
    );
    assert_eq!(response.status, OK);
    assert_eq!(owner.wait_results(child, 1), [bytes_result(4, &c.tag())]);
    for (receipts, _, _) in &testkits {
        assert_eq!(receipts.receipts("read").len(), 1, "an extra read result");
    }
}

/// B0 counterexamples: never-issued and retired handles confused, and a
/// response over 64 KiB executing its first read. Both runtimes give every
/// refusal the same status, decided before any read of the real target
/// (zero reads through its [`Probe`]): a dependent reference that is not
/// the session's pointer width (x86 and x64), a two-stage plan whose
/// worst-case response exceeds 64 KiB although its first stage is a valid
/// pointer read, a never-issued handle and a raw PID
/// (`VOSU_ERR_INVALID_ARGUMENT`), and a handle whose target exited
/// (`VOSU_ERR_TARGET_GONE`). An accepted plan afterwards does read.
#[test]
fn broker_refusals_match_the_direct_runtime_before_any_read() {
    let mut observed = Vec::new();
    for (index, runtime) in ["in-process", "process"].into_iter().enumerate() {
        let root = super::support::scratch();
        let host = Host::new(root.path());
        let build = fixture::dir(root.path(), "targets");
        let process = format!("b0-refuse-{index}-{}", std::process::id());
        let plugin = Plugin::new(
            root.path(),
            &host,
            &format!("refuse{index}"),
            runtime,
            &process,
        );
        host.create(1, &plugin.loaded).unwrap();
        let mut x86 = Target::start(&build, Width::X86, 51 + index as u8 * 2);
        let mut x64 = Target::start(&build, Width::X64, 52 + index as u8 * 2);
        let probes = [probe(&host, &x86, &process), probe(&host, &x64, &process)];
        let (child, h86) = plugin.session(&x86, &[]);
        let (_, h64) = plugin.session(&x64, &[]);
        assert_eq!(child == std::process::id(), runtime == "in-process");

        let overflow = vec![
            single(8).remove(0),
            Stage {
                base: StageBase::Dependent {
                    ref_stage: 0,
                    ref_op: 0,
                },
                base_offset: 0,
                ops: vec![
                    PlanOp {
                        offset: 0,
                        length: 40_000,
                    };
                    2
                ],
            },
        ];
        let cases = [
            (
                "x86 session, 8-byte references",
                h86,
                x86.root,
                chain(8),
                INVALID_ARGUMENT,
            ),
            (
                "x64 session, 4-byte references",
                h64,
                x64.root,
                chain(4),
                INVALID_ARGUMENT,
            ),
            (
                "worst case over 64 KiB",
                h64,
                x64.root,
                overflow,
                INVALID_ARGUMENT,
            ),
            (
                "never-issued handle",
                0x7777_0000,
                x64.root,
                chain(8),
                INVALID_ARGUMENT,
            ),
            (
                "raw PID",
                u64::from(x64.pid),
                x64.root,
                chain(8),
                INVALID_ARGUMENT,
            ),
        ];
        let mut statuses = Vec::new();
        for (case, handle, address, stages, expected) in cases {
            for sync in [true, false] {
                let mode = if sync { "thread" } else { "dispatch" };
                let response = plugin.send(
                    &host,
                    1,
                    mode,
                    &submit(9, handle, address, stages.clone(), sync),
                );
                response.assert_refused(expected, &format!("{runtime} {case}, sync={sync}"));
                statuses.push(response.status);
            }
        }
        assert_eq!(
            probes
                .iter()
                .map(|probe| probe.entered())
                .collect::<Vec<_>>(),
            [0, 0],
            "{runtime}: every refusal preceded every read"
        );

        x64.stop();
        host.process_exited(x64.pid);
        plugin.wait_detached(child, h64);
        for sync in [true, false] {
            let mode = if sync { "thread" } else { "dispatch" };
            for (case, handle, expected) in [
                ("retired handle", h64, TARGET_GONE),
                ("never-issued handle", 0x7777_0001, INVALID_ARGUMENT),
            ] {
                let response = plugin.send(
                    &host,
                    1,
                    mode,
                    &submit(10, handle, x64.root, chain(8), sync),
                );
                response.assert_refused(expected, &format!("{runtime} {case}, sync={sync}"));
                statuses.push(response.status);
            }
        }
        assert_eq!(
            probes[1].entered(),
            0,
            "{runtime}: the retired target was read"
        );

        let response = plugin.send(
            &host,
            1,
            "thread",
            &submit(11, h86, x86.root, chain(4), true),
        );
        assert_eq!(response.status, OK, "{runtime}: {response:?}");
        assert_eq!(response.plan_result(), chain_result(11, &x86));
        assert!(probes[0].entered() > 0, "the probe sees real reads");
        observed.push(statuses);
        drop(host);
        x86.stop();
    }
    assert_eq!(observed[0], observed[1], "in-process vs process statuses");
}

/// B0 counterexample: a cancel frees its plan ID before its execution
/// finishes. While a cancelled plan's real read is held in flight, its ID
/// stays reserved (`VOSU_ERR_INVALID_ARGUMENT`) though other plans are
/// admitted; once it finishes, the ID is admitted again, and only the new
/// plan's result (the tag's second half, not the cancelled first half)
/// ever reaches the plugin.
#[test]
fn broker_a_cancelled_plan_keeps_its_id_until_its_execution_finishes() {
    let root = super::support::scratch();
    let host = Host::new(root.path());
    let build = fixture::dir(root.path(), "targets");
    let process = format!("b0-cancel-{}", std::process::id());
    let plugin = Plugin::new(root.path(), &host, "cancel", "process", &process);
    host.create(1, &plugin.loaded).unwrap();
    let target = Target::start(&build, Width::X64, 61);
    let gate = probe(&host, &target, &process);
    let (child, handle) = plugin.session(&target, &[]);
    let tag = target.tag();
    let half = |plan, second: bool| {
        let address = target.tag_address + if second { 8 } else { 0 };
        submit(plan, handle, address, single(8), false)
    };

    gate.set_held(true);
    assert_eq!(
        plugin.send(&host, 1, "dispatch", &half(7, false)).status,
        OK
    );
    gate.wait_inside(1);
    let cancel = Message::ReadPlanCancel(ReadPlanCancel { plan: 7 });
    assert_eq!(plugin.send(&host, 1, "dispatch", &cancel).status, OK);
    plugin
        .send(&host, 1, "dispatch", &half(7, true))
        .assert_refused(
            INVALID_ARGUMENT,
            "the cancelled plan's ID while it executes",
        );
    assert_eq!(plugin.send(&host, 1, "dispatch", &half(8, true)).status, OK);
    gate.set_held(false);

    // The reservation ends when the cancelled execution finishes, which
    // nothing outside core observes: retry the reuse until it is admitted.
    let deadline = Instant::now() + BOUND;
    loop {
        let response = plugin.send(&host, 1, "dispatch", &half(7, true));
        if response.status == OK {
            break;
        }
        response.assert_refused(INVALID_ARGUMENT, "the reserved ID");
        assert!(
            Instant::now() < deadline,
            "the cancelled ID was never released"
        );
    }
    assert_eq!(
        plugin.send(&host, 1, "dispatch", &half(9, false)).status,
        OK
    );
    let mut results = plugin.wait_results(child, 3);
    results.sort_by_key(|result| result.plan);
    assert_eq!(
        results,
        [
            bytes_result(7, &tag[8..]),
            bytes_result(8, &tag[8..]),
            bytes_result(9, &tag[..8]),
        ],
        "the cancelled plan delivered nothing"
    );
}

/// B0 counterexample: a result from an old child completes a new child's
/// waiter. Instance 1's first plugin host submits a SYNC plan whose real
/// read is held, then dies; instance 1's new plugin host submits a SYNC
/// plan with the same plan ID and correlation on another target, also
/// held. Releasing the old read while the new waiter is outstanding
/// answers nothing; the new waiter gets exactly its own target's bytes.
#[test]
fn broker_an_old_childs_result_never_completes_a_new_childs_waiter() {
    let root = super::support::scratch();
    let host = Host::new(root.path());
    let build = fixture::dir(root.path(), "targets");
    let process = format!("b0-generation-{}", std::process::id());
    let plugin = Plugin::new(root.path(), &host, "generation", "process", &process);
    host.create(1, &plugin.loaded).unwrap();
    let old_target = Target::start(&build, Width::X64, 71);
    let new_target = Target::start(&build, Width::X86, 72);
    let old_gate = probe(&host, &old_target, &process);
    let (old, old_handle) = plugin.session(&old_target, &[]);

    old_gate.set_held(true);
    let old_request = plugin.start(
        &host,
        1,
        "thread",
        &submit(5, old_handle, old_target.tag_address, single(16), true),
    );
    old_gate.wait_inside(1);
    let identity = Identity::of(old);
    plugin.exit(&host, 1);
    assert!(
        wait_until(BOUND, || !identity.alive()),
        "the old child survived"
    );
    let focus = Message::FocusChanged(FocusChanged {
        handle: 7,
        focused: true,
        foreground: None,
    });
    assert!(
        host.wait_retired(1, &focus, BOUND),
        "the lost instance stayed live"
    );

    // Supervision (C0) restarts the lost instance under the same id after
    // its 1-second backoff: the new generation is a new plugin host.
    assert!(
        wait_until(BOUND, || host
            .host
            .process_status(vosu_core::plugin::PluginInstanceId::new(1))
            .is_some_and(|status| {
                status.restarts == 1
                    && status.phase == vosu_core::plugin::process::supervisor::ProcessPhase::Running
            })),
        "the lost instance never restarted"
    );
    let new_gate = probe(&host, &new_target, &process);
    let (new, new_handle) = plugin.session(&new_target, &[old]);
    assert_ne!(new, old);
    new_gate.set_held(true);
    let new_request = plugin.start(
        &host,
        1,
        "thread",
        &submit(5, new_handle, new_target.tag_address, single(16), true),
    );
    new_gate.wait_inside(1);
    old_gate.set_held(false);
    old_gate.wait_inside(0);
    // The old execution has its bytes now; any wrong delivery would answer
    // the outstanding new waiter within this window.
    let answered = fixture::wait_marker(
        &plugin.markers,
        &format!("response-{new_request}"),
        Duration::from_millis(500),
    );
    assert!(
        answered.is_none(),
        "the old child's result answered: {answered:?}"
    );
    new_gate.set_held(false);
    let response = plugin.response(new_request);
    assert_eq!(response.child, new);
    assert_eq!(response.status, OK, "{response:?}");
    assert_eq!(response.same_thread, "1");
    assert_eq!(response.plan_result(), bytes_result(5, &new_target.tag()));
    assert!(
        fixture::marker(&plugin.markers, &format!("response-{old_request}")).is_none(),
        "the dead child answered"
    );
}
