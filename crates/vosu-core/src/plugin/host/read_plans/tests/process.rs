//! M5 B0: read plans and scans of `process`-runtime instances, driven
//! through a real `PluginHost`, core's real supervision, ingress and
//! endpoint, and a child the test plays frame by frame over a real
//! channel ([`ChannelLauncher`]). Every expected result is built
//! independently of the executor, and every target is synthetic memory
//! that counts the backend batches core issued against it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::time::Duration;

use vosu_plugin_api::memory::{
    MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest,
};
use vosu_plugin_api::{
    Bitness, Message, PlanBase, PlanOp, ReadPlanCancel, ReadPlanResult, ReadPlanSubmit, ReadResult,
    Stage, StageBase, TimerFired, vosu_opcode, vosu_status,
};
use vosu_plugin_testkit::read_plan::{ReadPlanCommand, ReadPlanEvent, ReadPlanPluginConfig};

use super::super::super::tests::{test_host, test_process};
use super::super::*;
use super::helpers::{create_read_plan_instance, expect_attached};
use crate::plugin::process::frame::{FrameHeader, control as op};
use crate::plugin::process::launcher::{ChannelChild, ChannelLauncher};
use crate::plugin::registry::{self, LoadedPlugin};
use crate::plugin::test_support::write_development_package;
use crate::plugin::{MessageRef, PluginIngress, ProcessEndpoint, ProcessLaunch, ProcessLauncher};

const BOUND: Duration = Duration::from_secs(20);
const RESULT: u32 = vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32;

/// Synthetic target memory: a pointer chain at `0x1000 → 0x2000 → 0x3000`
/// of `width`-byte pointers ending in 16 identifying bytes, counting every
/// backend batch, optionally holding reads until released.
struct Target {
    bytes: BTreeMap<usize, u8>,
    batches: AtomicUsize,
    held: Mutex<bool>,
    changed: Condvar,
    entered: Mutex<usize>,
}

impl Target {
    fn chain(width: usize, tag: [u8; 16]) -> Arc<Self> {
        let mut bytes = BTreeMap::new();
        let mut put = |address: usize, value: &[u8]| {
            for (offset, byte) in value.iter().enumerate() {
                bytes.insert(address + offset, *byte);
            }
        };
        put(0x1000, &0x2000_u64.to_le_bytes()[..width]);
        put(0x2000, &0x3000_u64.to_le_bytes()[..width]);
        put(0x3000, &tag);
        Arc::new(Self {
            bytes,
            batches: AtomicUsize::new(0),
            held: Mutex::new(false),
            changed: Condvar::new(),
            entered: Mutex::new(0),
        })
    }

    fn hold(&self) {
        *self.held.lock().unwrap() = true;
    }

    fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.changed.notify_all();
    }

    fn batches(&self) -> usize {
        self.batches.load(Ordering::SeqCst)
    }

    /// Waits until `count` batches entered (and possibly block).
    fn wait_entered(&self, count: usize) {
        let entered = self.entered.lock().unwrap();
        let (entered, _) = self
            .changed
            .wait_timeout_while(entered, BOUND, |entered| *entered < count)
            .unwrap();
        assert!(*entered >= count, "{} of {count} reads started", *entered);
    }

    fn copy(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        for (offset, byte) in dst.iter_mut().enumerate() {
            *byte = *self
                .bytes
                .get(&(address + offset))
                .ok_or_else(|| MemoryError::Read {
                    address,
                    len: offset,
                    message: "outside the target".to_owned(),
                })?;
        }
        Ok(())
    }
}

impl ProcessMemory for Target {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.read_many(&mut [ReadRequest { address, dst }])
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.batches.fetch_add(1, Ordering::SeqCst);
        *self.entered.lock().unwrap() += 1;
        self.changed.notify_all();
        let held = self.held.lock().unwrap();
        drop(
            self.changed
                .wait_timeout_while(held, BOUND, |held| *held)
                .unwrap(),
        );
        for read in reads {
            self.copy(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

/// The three-stage dependent chain [`Target::chain`] lays out.
fn chain_stages(width: u32) -> Vec<Stage> {
    let pointer = |base| Stage {
        base,
        base_offset: 0,
        ops: vec![PlanOp {
            offset: 0,
            length: width,
        }],
    };
    vec![
        pointer(StageBase::Root),
        pointer(StageBase::Dependent {
            ref_stage: 0,
            ref_op: 0,
        }),
        Stage {
            base: StageBase::Dependent {
                ref_stage: 1,
                ref_op: 0,
            },
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

fn submit(plan: u64, handle: u64, stages: Vec<Stage>, sync: bool) -> Message {
    Message::ReadPlanSubmit(ReadPlanSubmit {
        plan,
        handle,
        root: PlanBase::Absolute(0x1000),
        stages,
        sync,
    })
}

/// The logical result of the chain, built without the executor.
fn chain_logical(plan: u64, width: usize, tag: [u8; 16]) -> ReadPlanResult {
    ReadPlanResult {
        plan,
        results: vec![
            ReadResult::Ok(0x2000_u64.to_le_bytes()[..width].to_vec()),
            ReadResult::Ok(0x3000_u64.to_le_bytes()[..width].to_vec()),
            ReadResult::Ok(tag[..8].to_vec()),
            ReadResult::Ok(tag[8..].to_vec()),
        ],
    }
}

/// The asynchronous result payload: the owned message encoding core
/// delivers to an in-process plugin too.
fn chain_result(plan: u64, width: usize, tag: [u8; 16]) -> Vec<u8> {
    Message::ReadPlanResult(chain_logical(plan, width, tag))
        .encode_payload()
        .unwrap()
}

/// The SYNC result bytes the direct (in-process) runtime hands a plugin
/// for `message` over the same chain, checked against [`chain_logical`].
fn direct_sync(message: &Message, width: usize, tag: [u8; 16]) -> Vec<u8> {
    let payload = message.encode_payload().unwrap();
    let bitness = if width == 4 {
        ProcessBitness::X86
    } else {
        ProcessBitness::X64
    };
    let mut out = Vec::new();
    read_plan::ValidatedPlan::new(&payload, message.flags())
        .unwrap()
        .execute_sync(
            Target::chain(width, tag).as_ref(),
            bitness,
            &NoModules,
            BOUND,
            &mut out,
        )
        .unwrap();
    let Message::ReadPlanSubmit(submit) = message else {
        unreachable!()
    };
    assert_eq!(
        Message::decode(vosu_opcode::VOSU_OP_READ_PLAN_RESULT, 0, &out).unwrap(),
        Message::ReadPlanResult(chain_logical(submit.plan, width, tag)),
        "the direct runtime's result is the chain's"
    );
    out
}

/// An acknowledged request with no reply and no effect (§6.3: cancelling
/// an id that is not outstanding is a silent no-op).
fn noop() -> Message {
    Message::ReadPlanCancel(ReadPlanCancel { plan: 0xDEAD_BEEF })
}

struct Rig {
    runtime: tokio::runtime::Runtime,
    host: Arc<PluginHost>,
    launcher: Arc<ChannelLauncher>,
    dirs: Vec<tempfile::TempDir>,
}

impl Rig {
    fn new(sync_budget: Duration) -> Self {
        let launcher = ChannelLauncher::new(sync_budget);
        Self::installing(launcher.clone(), launcher)
    }

    /// A host whose process instances start through `installed`.
    fn installing(installed: Arc<dyn ProcessLauncher>, launcher: Arc<ChannelLauncher>) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (logs, host) = {
            let _enter = runtime.enter();
            test_host(runtime.handle().clone())
        };
        host.install_process_launcher(installed).unwrap();
        Self {
            runtime,
            host,
            launcher,
            dirs: vec![logs],
        }
    }

    fn package(&mut self, plugin_id: &str, process: &str) -> LoadedPlugin {
        let dir = tempfile::tempdir().unwrap();
        write_development_package(
            dir.path(),
            plugin_id,
            process,
            "process",
            "plugin.bin",
            b"x",
        )
        .unwrap();
        let loaded = registry::load_development(
            &semver::Version::parse("2.0.0-alpha.1").unwrap(),
            &crate::plugin::ValidationSnapshot::default(),
            dir.path(),
            true,
        )
        .unwrap();
        self.dirs.push(dir);
        loaded
    }

    /// Creates process instance `id` matching `process` and takes its child.
    fn process_instance(&mut self, id: u64, process: &str) -> (LoadedPlugin, ChannelChild) {
        let loaded = self.package(&format!("test.b0.{process}"), process);
        let _enter = self.runtime.enter();
        self.host
            .create_instance(
                PluginInstanceId::new(id),
                &loaded,
                std::env::temp_dir(),
                None,
            )
            .unwrap();
        (loaded, self.launcher.take_child())
    }

    /// Attaches `pid` (named `process`, memory `target`) and returns the
    /// handle `child` was offered.
    fn attach(
        &self,
        child: &mut ChannelChild,
        pid: u32,
        process: &str,
        target: &Arc<Target>,
        bitness: ProcessBitness,
    ) -> u64 {
        set_session_memory_hook_for_tests(pid, target.clone(), bitness);
        let mut record = test_process(pid, process);
        record.bitness = match bitness {
            ProcessBitness::X86 => Bitness::X86,
            ProcessBitness::X64 => Bitness::X64,
        };
        let _enter = self.runtime.enter();
        self.host.process_appeared(record);
        expect_attached_frame(child)
    }

    /// Waits until every broker worker finished what it held: all of them
    /// meet at one barrier only once each finished its earlier jobs.
    fn drain_broker(&self) {
        let barrier = Arc::new(Barrier::new(BACKGROUND_WORKER_COUNT + 1));
        for _ in 0..BACKGROUND_WORKER_COUNT {
            let barrier = Arc::clone(&barrier);
            self.host.background_pool.submit(u64::MAX, move || {
                barrier.wait();
            });
        }
        barrier.wait();
    }

    /// Dispatches a marker frame to `instance` behind everything core
    /// already queued for it, including reserved terminal results.
    fn mark(&self, instance: u64, timer: u64) {
        let _enter = self.runtime.enter();
        let message = Message::TimerFired(TimerFired { timer });
        drop(
            self.host
                .dispatch_to_instance(PluginInstanceId::new(instance), &message, 0)
                .unwrap(),
        );
    }

    fn finish(self, pids: &[u32]) {
        {
            let _enter = self.runtime.enter();
            self.host.destroy_all(Duration::from_millis(200));
        }
        for pid in pids {
            clear_session_memory_hook_for_tests(*pid);
        }
    }
}

fn expect_attached_frame(child: &mut ChannelChild) -> u64 {
    loop {
        let (header, payload) = child.recv();
        if header.opcode == vosu_opcode::VOSU_OP_SESSION_ATTACHED as u32 {
            let Ok(Message::SessionAttached(attached)) =
                Message::decode(vosu_opcode::VOSU_OP_SESSION_ATTACHED, 0, &payload)
            else {
                panic!("undecodable SESSION_ATTACHED");
            };
            return attached.handle;
        }
    }
}

/// Every frame up to the marker `timer`, which is consumed.
fn frames_until_mark(child: &mut ChannelChild, timer: u64) -> Vec<(FrameHeader, Vec<u8>)> {
    let marker = Message::TimerFired(TimerFired { timer })
        .encode_payload()
        .unwrap();
    let mut frames = Vec::new();
    loop {
        let (header, payload) = child.recv();
        if header.opcode == vosu_opcode::VOSU_OP_TIMER_FIRED as u32 && payload == marker {
            return frames;
        }
        frames.push((header, payload));
    }
}

/// A plan of a process instance crosses the channel once, whatever its
/// stage count: one acknowledged submission, then exactly one
/// `REPLY_NEXT` + `READ_PLAN_RESULT` (SYNC) or `STATUS` + result (async),
/// whose bytes are the exact flat result. Core resolved the dependent
/// chain itself, one backend batch per stage, against the one target the
/// session owns: two distinct targets of different pointer widths and
/// bytes are each read only through their own owner, and a foreign handle
/// reads nothing.
#[test]
fn read_plan_process_chains_cross_once_with_exact_bytes_per_owned_target() {
    let mut rig = Rig::new(BOUND);
    let (_a, mut a) = rig.process_instance(9_601, "b0-chain-a");
    let (_b, mut b) = rig.process_instance(9_602, "b0-chain-b");
    let (pid_a, pid_b) = (500_960_001, 500_960_002);
    let tag_a = *b"B0 target A, x64";
    let tag_b = *b"b0 TARGET b, x86";
    let target_a = Target::chain(8, tag_a);
    let target_b = Target::chain(4, tag_b);
    let handle_a = rig.attach(&mut a, pid_a, "b0-chain-a", &target_a, ProcessBitness::X64);
    let handle_b = rig.attach(&mut b, pid_b, "b0-chain-b", &target_b, ProcessBitness::X86);
    assert_ne!(handle_a, handle_b);

    for (child, handle, width, tag, target) in [
        (&mut a, handle_a, 8_usize, tag_a, &target_a),
        (&mut b, handle_b, 4, tag_b, &target_b),
    ] {
        let stages = chain_stages(width as u32);
        let message = submit(1, handle, stages.clone(), true);
        let request = child.send(&message, 0);
        assert_eq!(
            child.sync_reply(),
            (request, direct_sync(&message, width, tag)),
            "the direct runtime's exact bytes"
        );
        assert_eq!(target.batches(), 3, "one batch per stage, all in core");
        let request = child.send(&submit(2, handle, stages, false), 0);
        assert_eq!(child.status(), (request, vosu_status::VOSU_OK));
        let (header, bytes) = child.recv();
        assert_eq!(
            (header.opcode, bytes),
            (RESULT, chain_result(2, width, tag))
        );
        assert_eq!(target.batches(), 6);
        let request = child.send(&noop(), 0);
        assert_eq!(
            child.status(),
            (request, vosu_status::VOSU_OK),
            "nothing else crossed for either plan"
        );
    }

    // A's plan naming B's handle: refused before any read of B.
    for sync in [true, false] {
        let request = a.send(&submit(3, handle_b, chain_stages(4), sync), 0);
        assert_eq!(
            a.status(),
            (request, vosu_status::VOSU_ERR_PERMISSION_DENIED)
        );
    }
    assert_eq!(target_b.batches(), 6, "a foreign handle read nothing");
    rig.finish(&[pid_a, pid_b]);
}

/// Refusals a process instance meets are the in-process ones, decided
/// before any read: a dependent stage whose reference is not the session's
/// pointer width (x86 and x64), a response over 64 KiB, a never-issued
/// handle (`VOSU_ERR_INVALID_ARGUMENT`) and a retired one
/// (`VOSU_ERR_TARGET_GONE`).
#[test]
fn read_plan_process_refusals_match_in_process_before_any_read() {
    let mut rig = Rig::new(BOUND);
    let (_p86, mut p86) = rig.process_instance(9_611, "b0-refuse-86");
    let (_p64, mut p64) = rig.process_instance(9_612, "b0-refuse-64");
    let local = {
        let _enter = rig.runtime.enter();
        create_read_plan_instance(
            &rig.host,
            PluginInstanceId::new(9_613),
            "test.b0.refuse-local",
            &["b0-refuse-l86", "b0-refuse-l64"],
            ReadPlanPluginConfig::default(),
        )
    };
    let (events, commands) = local;
    let pids = [500_961_001, 500_961_002, 500_961_003, 500_961_004];
    let targets: Vec<_> = [4, 8, 4, 8]
        .into_iter()
        .map(|width| Target::chain(width, [7; 16]))
        .collect();
    let handle_86 = rig.attach(
        &mut p86,
        pids[0],
        "b0-refuse-86",
        &targets[0],
        ProcessBitness::X86,
    );
    let handle_64 = rig.attach(
        &mut p64,
        pids[1],
        "b0-refuse-64",
        &targets[1],
        ProcessBitness::X64,
    );
    let local_handle = |pid, name, target: &Arc<Target>, bitness| {
        set_session_memory_hook_for_tests(pid, target.clone(), bitness);
        let mut record = test_process(pid, name);
        record.bitness = match bitness {
            ProcessBitness::X86 => Bitness::X86,
            ProcessBitness::X64 => Bitness::X64,
        };
        let _enter = rig.runtime.enter();
        rig.host.process_appeared(record);
        expect_attached(&events)
    };
    let local_86 = local_handle(pids[2], "b0-refuse-l86", &targets[2], ProcessBitness::X86);
    let local_64 = local_handle(pids[3], "b0-refuse-l64", &targets[3], ProcessBitness::X64);

    let oversized = vec![Stage {
        base: StageBase::Root,
        base_offset: 0,
        ops: vec![
            PlanOp {
                offset: 0,
                length: 40_000,
            };
            2
        ],
    }];
    let never_issued = 0x7777_0000;
    // (process child, process handle, local handle, stages, expected)
    let cases: Vec<(bool, u64, u64, Vec<Stage>, vosu_status)> = vec![
        (
            true,
            handle_86,
            local_86,
            chain_stages(8),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        ),
        (
            false,
            handle_64,
            local_64,
            chain_stages(4),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        ),
        (
            false,
            handle_64,
            local_64,
            oversized,
            vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        ),
        (
            false,
            never_issued,
            never_issued,
            chain_stages(8),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        ),
    ];
    let local_status = |sync: bool, handle: u64, stages: Vec<Stage>| {
        let root = PlanBase::Absolute(0x1000);
        let command = if sync {
            ReadPlanCommand::SubmitSync {
                plan: 40,
                handle,
                root,
                stages,
            }
        } else {
            ReadPlanCommand::SubmitAsync {
                plan: 40,
                handle,
                root,
                stages,
            }
        };
        commands.send(command).unwrap();
        match events.recv_timeout(BOUND).unwrap() {
            ReadPlanEvent::SubmitStatus(status) => status,
            ReadPlanEvent::SyncResult(Err(vosu_plugin_internal::PlanMemoryError::Rejected(
                status,
            ))) => status,
            other => panic!("unexpected {other:?}"),
        }
    };
    for (on_86, process_handle, local, stages, expected) in cases {
        for sync in [true, false] {
            let child = if on_86 { &mut p86 } else { &mut p64 };
            let request = child.send(&submit(40, process_handle, stages.clone(), sync), 0);
            let (replied, status) = child.status();
            assert_eq!(
                (replied, status),
                (request, expected),
                "sync={sync} {stages:?}"
            );
            assert_eq!(local_status(sync, local, stages.clone()), status);
        }
    }
    assert!(
        targets.iter().all(|target| target.batches() == 0),
        "every refusal preceded every read"
    );

    // Retired handles answer TARGET_GONE in both runtimes.
    {
        let _enter = rig.runtime.enter();
        rig.host.process_exited(pids[1]);
        rig.host.process_exited(pids[3]);
    }
    loop {
        let (header, _) = p64.recv();
        if header.opcode == vosu_opcode::VOSU_OP_SESSION_DETACHED as u32 {
            break;
        }
    }
    assert!(matches!(
        events.recv_timeout(BOUND).unwrap(),
        ReadPlanEvent::Detached { .. }
    ));
    for sync in [true, false] {
        let request = p64.send(&submit(41, handle_64, chain_stages(8), sync), 0);
        assert_eq!(p64.status(), (request, vosu_status::VOSU_ERR_TARGET_GONE));
        assert_eq!(
            local_status(sync, local_64, chain_stages(8)),
            vosu_status::VOSU_ERR_TARGET_GONE
        );
    }
    rig.finish(&pids);
}

/// T1 handoffs 1 and 2: a SYNC plan executes on the broker's bounded
/// workers, so while one is blocked inside a native read the channel's
/// ingress keeps draining — acknowledged traffic and a second SYNC plan
/// complete — and core itself answers the blocked plan
/// `VOSU_ERR_TIMED_OUT` at its budget, never the child's backstop. The late
/// result is discarded.
#[test]
fn read_plan_process_sync_runs_on_broker_workers_and_times_out_in_core() {
    let mut rig = Rig::new(Duration::from_millis(300));
    let (_p, mut child) = rig.process_instance(9_621, "b0-sync");
    let (held_pid, free_pid) = (500_962_001, 500_962_002);
    let held = Target::chain(8, [1; 16]);
    let free = Target::chain(8, [2; 16]);
    let held_handle = rig.attach(&mut child, held_pid, "b0-sync", &held, ProcessBitness::X64);
    let free_handle = rig.attach(&mut child, free_pid, "b0-sync", &free, ProcessBitness::X64);
    held.hold();
    let blocked = child.send(&submit(1, held_handle, chain_stages(8), true), 0);
    held.wait_entered(1);
    let ordinary = child.send(&noop(), 0);
    assert_eq!(child.status(), (ordinary, vosu_status::VOSU_OK));
    let message = submit(2, free_handle, chain_stages(8), true);
    let second = child.send(&message, 0);
    assert_eq!(
        child.sync_reply(),
        (second, direct_sync(&message, 8, [2; 16]))
    );
    assert_eq!(
        child.status(),
        (blocked, vosu_status::VOSU_ERR_TIMED_OUT),
        "core's budget timer answered the blocked plan"
    );
    held.release();
    rig.drain_broker();
    let after = child.send(&noop(), 0);
    assert_eq!(
        child.status(),
        (after, vosu_status::VOSU_OK),
        "no late REPLY_NEXT for the timed-out plan"
    );
    rig.finish(&[held_pid, free_pid]);
}

/// §6.3: a cancelled plan frees its slot at once but keeps its id reserved
/// until its execution finishes, and its late result is discarded.
#[test]
fn read_plan_process_cancel_keeps_the_id_reserved_until_execution_finishes() {
    let mut rig = Rig::new(BOUND);
    let (_p, mut child) = rig.process_instance(9_631, "b0-cancel");
    let pid = 500_963_001;
    let target = Target::chain(8, [3; 16]);
    let handle = rig.attach(&mut child, pid, "b0-cancel", &target, ProcessBitness::X64);
    target.hold();
    let request = child.send(&submit(5, handle, chain_stages(8), false), 0);
    assert_eq!(child.status(), (request, vosu_status::VOSU_OK));
    target.wait_entered(1);
    let cancel = child.send(&Message::ReadPlanCancel(ReadPlanCancel { plan: 5 }), 0);
    assert_eq!(child.status(), (cancel, vosu_status::VOSU_OK));
    let reused = child.send(&submit(5, handle, chain_stages(8), false), 0);
    assert_eq!(
        child.status(),
        (reused, vosu_status::VOSU_ERR_INVALID_ARGUMENT),
        "the cancelled id stays reserved while it executes"
    );
    let other = child.send(&submit(6, handle, chain_stages(8), false), 0);
    assert_eq!(child.status(), (other, vosu_status::VOSU_OK));
    target.release();
    rig.drain_broker();
    rig.mark(9_631, 1);
    let results: Vec<_> = frames_until_mark(&mut child, 1)
        .into_iter()
        .filter(|(header, _)| header.opcode == RESULT)
        .map(|(_, bytes)| bytes)
        .collect();
    assert_eq!(
        results,
        vec![chain_result(6, 8, [3; 16])],
        "plan 5 discarded"
    );
    let again = child.send(&submit(5, handle, chain_stages(8), false), 0);
    assert_eq!(child.status(), (again, vosu_status::VOSU_OK));
    let (header, bytes) = child.recv();
    assert_eq!(
        (header.opcode, bytes),
        (RESULT, chain_result(5, 8, [3; 16]))
    );
    rig.finish(&[pid]);
}

/// A result computed for a lost generation never reaches the next one,
/// even when the new generation reuses the instance id: neither the
/// asynchronous result (it travels through the old instance's reserved
/// outbox, never a by-id lookup) nor the SYNC one (a broker worker
/// answering the old channel's request delivers to no other endpoint).
#[test]
fn read_plan_process_results_never_reach_a_new_generation() {
    let mut rig = Rig::new(BOUND);
    let (_loaded, mut old) = rig.process_instance(9_641, "b0-generation");
    let pid = 500_964_001;
    let target = Target::chain(8, [4; 16]);
    let handle = rig.attach(&mut old, pid, "b0-generation", &target, ProcessBitness::X64);
    target.hold();
    let request = old.send(&submit(1, handle, chain_stages(8), false), 0);
    assert_eq!(old.status(), (request, vosu_status::VOSU_OK));
    old.send(&submit(2, handle, chain_stages(8), true), 0);
    target.wait_entered(2);
    // The child exits: one loss, the instance retired.
    old.exit(1);
    {
        let instances = rig.host.instances.lock().unwrap();
        let (instances, timeout) = rig
            .host
            .instances_changed
            .wait_timeout_while(instances, BOUND, |map| {
                map.contains_key(&PluginInstanceId::new(9_641))
            })
            .unwrap();
        assert!(!timeout.timed_out(), "the lost instance was retired");
        drop(instances);
    }
    // Supervision starts the next generation under the same instance id
    // after the first backoff (M5 C0).
    let mut new = rig.launcher.take_child();
    target.release();
    rig.drain_broker();
    rig.mark(9_641, 2);
    let stale: Vec<_> = frames_until_mark(&mut new, 2)
        .into_iter()
        .filter(|(header, _)| header.opcode == RESULT || header.opcode == op::REPLY_NEXT)
        .collect();
    assert!(stale.is_empty(), "{stale:?}");
    rig.finish(&[pid]);
}

/// An endpoint whose dispatch blocks until opened: a stalled child.
#[derive(Default)]
struct GatedEndpoint {
    open: Mutex<bool>,
    changed: Condvar,
    entered: Mutex<usize>,
    frames: Mutex<Vec<(u32, u64, Vec<u8>)>>,
}

impl GatedEndpoint {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait<T>(&self, mutex: &Mutex<T>, done: impl Fn(&T) -> bool) {
        let guard = mutex.lock().unwrap();
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, BOUND, |value| !done(value))
            .unwrap();
        assert!(done(&guard), "condition within the bound");
    }
}

impl ProcessEndpoint for GatedEndpoint {
    fn dispatch(&self, message: MessageRef<'_>) -> vosu_status {
        *self.entered.lock().unwrap() += 1;
        self.changed.notify_all();
        let open = self.open.lock().unwrap();
        drop(self.changed.wait_while(open, |open| !*open).unwrap());
        self.frames.lock().unwrap().push((
            message.opcode,
            message.correlation_id,
            message.payload.to_vec(),
        ));
        self.changed.notify_all();
        vosu_status::VOSU_OK
    }

    fn destroy(&self, _grace: Duration) -> Result<(), crate::plugin::HostError> {
        self.open();
        Ok(())
    }

    fn terminate(&self) {
        self.open();
    }
}

struct GatedLauncher {
    endpoint: Arc<GatedEndpoint>,
    ingress: Mutex<Option<PluginIngress>>,
}

impl ProcessLauncher for GatedLauncher {
    fn launch(
        &self,
        launch: ProcessLaunch<'_>,
    ) -> Result<Arc<dyn ProcessEndpoint>, crate::plugin::HostError> {
        *self.ingress.lock().unwrap() = Some(launch.ingress);
        Ok(self.endpoint.clone())
    }
}

/// T1 handoff 3: an admitted asynchronous plan's and scan's terminal
/// results are delivered even when the instance's ordinary dispatch queue
/// is full behind a stalled child (they used to be dropped there, leaking
/// the child's local slots), and ahead of that backlog.
#[test]
fn read_plan_process_terminal_results_survive_a_full_dispatch_queue() {
    let endpoint = Arc::new(GatedEndpoint::default());
    let launcher = Arc::new(GatedLauncher {
        endpoint: Arc::clone(&endpoint),
        ingress: Mutex::new(None),
    });
    let mut rig = Rig::installing(launcher.clone(), ChannelLauncher::new(BOUND));
    let loaded = rig.package("test.b0.b0-full", "b0-full");
    let id = PluginInstanceId::new(9_651);
    {
        let _enter = rig.runtime.enter();
        rig.host
            .create_instance(id, &loaded, std::env::temp_dir(), None)
            .unwrap();
    }
    let pid = 500_965_001;
    let target = Target::chain(8, [5; 16]);
    set_session_memory_hook_for_tests(pid, target.clone(), ProcessBitness::X64);
    {
        let _enter = rig.runtime.enter();
        rig.host.process_appeared(test_process(pid, "b0-full"));
    }
    // SESSION_ATTACHED holds the dispatch worker in the stalled child.
    endpoint.wait(&endpoint.entered, |entered| *entered == 1);
    let handle = rig
        .host
        .sessions
        .lock()
        .unwrap()
        .handle_for_pid(pid)
        .unwrap()
        .0;
    let filler = Message::TimerFired(TimerFired { timer: 1 });
    let mut queued = 0;
    while rig.host.dispatch_to_instance(id, &filler, 0).is_ok() {
        queued += 1;
    }
    assert_eq!(
        queued,
        crate::plugin::DEFAULT_QUEUE_CAPACITY,
        "the queue is full"
    );
    let ingress = launcher.ingress.lock().unwrap().clone().unwrap();
    let deliver = |message: &Message, correlation_id| {
        let payload = message.encode_payload().unwrap();
        ingress.deliver(MessageRef {
            opcode: message.opcode() as u32,
            flags: message.flags(),
            correlation_id,
            payload: &payload,
        })
    };
    assert_eq!(
        deliver(&submit(9, handle, chain_stages(8), false), 0),
        vosu_status::VOSU_OK
    );
    let scan = Message::ScanRequest(vosu_plugin_api::ScanRequest {
        session: handle,
        module: None,
        pattern: "AA".to_owned(),
        non_zero_mask: false,
        all_matches: false,
    });
    assert_eq!(deliver(&scan, 77), vosu_status::VOSU_OK);
    rig.drain_broker();
    endpoint.open();
    endpoint.wait(&endpoint.frames, |frames| frames.len() == 1 + 2 + queued);
    let frames = endpoint.frames.lock().unwrap().clone();
    let mut terminals: Vec<_> = frames[1..3]
        .iter()
        .map(|(opcode, correlation, _)| (*opcode, *correlation))
        .collect();
    terminals.sort_unstable();
    assert_eq!(
        terminals,
        [(RESULT, 0), (vosu_opcode::VOSU_OP_SCAN_RESULT as u32, 77)],
        "both terminals delivered, ahead of the backlog"
    );
    assert!(
        frames
            .iter()
            .any(|(opcode, _, bytes)| *opcode == RESULT && *bytes == chain_result(9, 8, [5; 16]))
    );
    rig.finish(&[pid]);
}
