//! End-to-end read-plan routing tests, driven through a real
//! [`vosu_plugin_testkit::read_plan::ReadPlanPlugin`] instance (never
//! `read_plan.rs`/this module's own functions called directly), against
//! the test process's own memory exactly as `tests/process_memory.rs`
//! does for `vosu-sys`.

use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use vosu_plugin_api::{
    PlanBase, PlanMemoryError, PlanOp, ReadResult, Stage, StageBase, vosu_status,
};
use vosu_plugin_testkit::read_plan::{
    self as read_plan_plugin, ReadPlanCommand, ReadPlanEvent, ReadPlanPluginConfig,
};
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessMemory, ReadRequest};

use super::super::super::registry;
use super::super::tests::{test_host, test_process};
use super::*;

const RECV_TIMEOUT: Duration = Duration::from_secs(5);

fn read_plan_manifest(plugin_id: &str, process_names: &[&str]) -> String {
    let names = process_names
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=[{names}]\n"
    )
}

/// Registers and creates a [`read_plan_plugin::ReadPlanPlugin`] instance
/// synchronously (`PluginHost::create_instance` runs the plugin's own
/// `create` before returning), so the returned event [`Receiver`] is
/// ready to observe `SESSION_ATTACHED` the moment the caller drives a
/// matching [`ProcessRecord`] through [`PluginHost::process_appeared`].
fn create_read_plan_instance(
    host: &Arc<PluginHost>,
    id: PluginInstanceId,
    plugin_id: &str,
    process_names: &[&str],
    config: ReadPlanPluginConfig,
) -> (Receiver<ReadPlanEvent>, SyncSender<ReadPlanCommand>) {
    let (events, commands) = read_plan_plugin::register_with_config(plugin_id, config);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &read_plan_manifest(plugin_id, process_names),
        vtable: read_plan_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the read-plan testkit plugin");
    let data_dir = tempfile::tempdir().unwrap();
    host.create_instance(id, &loaded[0], data_dir.path().to_path_buf(), None)
        .expect("creating the read-plan testkit instance");
    (events, commands)
}

fn expect_attached(events: &Receiver<ReadPlanEvent>) -> u64 {
    match events.recv_timeout(RECV_TIMEOUT).expect("SESSION_ATTACHED") {
        ReadPlanEvent::Attached { handle } => handle,
        other => panic!("expected Attached, got {other:?}"),
    }
}

fn expect_status(events: &Receiver<ReadPlanEvent>) -> vosu_status {
    match events
        .recv_timeout(RECV_TIMEOUT)
        .expect("submit/cancel status")
    {
        ReadPlanEvent::SubmitStatus(status) => status,
        other => panic!("expected SubmitStatus, got {other:?}"),
    }
}

fn one_op_stage(length: u32) -> Stage {
    Stage {
        base: StageBase::Root,
        base_offset: 0,
        ops: vec![PlanOp { offset: 0, length }],
    }
}

/// M1 Exit (phase (d1) step 3): an asynchronous `VOSU_OP_READ_PLAN_SUBMIT`
/// against the test process's own memory returns the right bytes through
/// `VOSU_OP_READ_PLAN_RESULT`.
#[test]
fn async_plan_returns_the_right_bytes() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.async",
        &["proc-async"],
        ReadPlanPluginConfig::default(),
    );

    let pid = std::process::id();
    host.process_appeared(test_process(pid, "proc-async"));
    let handle = expect_attached(&events);

    let marker: u64 = 0x1122_3344_5566_7788;
    let boxed = Box::new(marker);
    let address = std::ptr::from_ref(&*boxed) as u64;

    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(address),
            stages: vec![one_op_stage(8)],
        })
        .expect("sending the async submit command");

    assert_eq!(expect_status(&events), vosu_status::VOSU_OK);

    let result = match events.recv_timeout(RECV_TIMEOUT).expect("READ_PLAN_RESULT") {
        ReadPlanEvent::AsyncResult(result) => result,
        other => panic!("expected AsyncResult, got {other:?}"),
    };
    assert_eq!(result.plan, 1);
    match &result.results[0] {
        ReadResult::Ok(bytes) => assert_eq!(bytes.as_slice(), &marker.to_le_bytes()),
        other => panic!("expected Ok, got {other:?}"),
    }
}

/// A dependent stage resolves a real pointer chase: stage 0 reads the
/// pointer stored at the plan's root, stage 1 reads through it.
#[test]
fn dependent_stage_resolves_a_pointer_chase() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.dependent",
        &["proc-dependent"],
        ReadPlanPluginConfig::default(),
    );

    let pid = std::process::id();
    host.process_appeared(test_process(pid, "proc-dependent"));
    let handle = expect_attached(&events);

    let inner: u64 = 0xDEAD_BEEF_CAFE_u64;
    let inner_box = Box::new(inner);
    let inner_addr = std::ptr::from_ref(&*inner_box) as u64;
    let outer_box = Box::new(inner_addr);
    let outer_addr = std::ptr::from_ref(&*outer_box) as u64;

    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(outer_addr),
            stages: vec![
                one_op_stage(8),
                Stage {
                    base: StageBase::Dependent {
                        ref_stage: 0,
                        ref_op: 0,
                    },
                    base_offset: 0,
                    ops: vec![PlanOp {
                        offset: 0,
                        length: 8,
                    }],
                },
            ],
        })
        .expect("sending the async submit command");

    assert_eq!(expect_status(&events), vosu_status::VOSU_OK);

    let result = match events.recv_timeout(RECV_TIMEOUT).expect("READ_PLAN_RESULT") {
        ReadPlanEvent::AsyncResult(result) => result,
        other => panic!("expected AsyncResult, got {other:?}"),
    };
    assert_eq!(result.results.len(), 2);
    match &result.results[1] {
        ReadResult::Ok(bytes) => {
            assert_eq!(bytes.as_slice(), &inner.to_le_bytes());
        }
        other => panic!("expected Ok, got {other:?}"),
    }
}

/// D5: a `VOSU_READ_PLAN_SYNC` submission's result is delivered on the
/// submitting thread before `send` returns — the plugin's reader thread
/// never observes a separate, later `VOSU_OP_READ_PLAN_RESULT` for it.
#[test]
fn sync_returns_before_send_returns() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.sync",
        &["proc-sync"],
        ReadPlanPluginConfig::default(),
    );

    let pid = std::process::id();
    host.process_appeared(test_process(pid, "proc-sync"));
    let handle = expect_attached(&events);

    let marker: u64 = 0xABCD_EF01_2345_6789;
    let boxed = Box::new(marker);
    let address = std::ptr::from_ref(&*boxed) as u64;

    commands
        .send(ReadPlanCommand::SubmitSync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(address),
            stages: vec![one_op_stage(8)],
        })
        .expect("sending the sync submit command");

    match events.recv_timeout(RECV_TIMEOUT).expect("sync result") {
        ReadPlanEvent::SyncResult(Ok(result)) => {
            assert_eq!(result.plan, 1);
            match &result.results[0] {
                ReadResult::Ok(bytes) => assert_eq!(bytes.as_slice(), &marker.to_le_bytes()),
                other => panic!("expected Ok, got {other:?}"),
            }
        }
        other => panic!("expected an Ok SyncResult, got {other:?}"),
    }
    assert!(
        events.try_recv().is_err(),
        "no separate READ_PLAN_RESULT expected: SYNC never queues"
    );
}

/// D5: a `VOSU_READ_PLAN_SYNC` submission attempted from this plugin's own
/// dispatch thread (inside `on_message`'s own call, not its background
/// reader thread) is refused.
#[test]
fn sync_on_dispatch_thread_is_refused() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let config = ReadPlanPluginConfig {
        sync_on_attach: Some((1, PlanBase::Absolute(0), Vec::new())),
    };
    let (events, _commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.dispatchthread",
        &["proc-dispatch"],
        config,
    );

    host.process_appeared(test_process(std::process::id(), "proc-dispatch"));
    let _handle = expect_attached(&events);
    assert_eq!(
        expect_status(&events),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
}

/// A deliberately slow [`ProcessMemory`] (set up through
/// [`set_session_memory_hook_for_tests`]) trips the read-plan deadline
/// (shortened for this test, [`set_read_plan_deadline_for_tests`]), so the
/// dependent second stage never starts and `send` reports
/// `VOSU_ERR_TIMED_OUT`.
struct SlowMemory {
    sleep: Duration,
}

impl ProcessMemory for SlowMemory {
    fn read_exact(&self, _address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        std::thread::sleep(self.sleep);
        dst.fill(0);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

#[test]
fn sync_deadline_times_out() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());

    let pid = 500_000_001;
    set_session_memory_hook_for_tests(
        pid,
        Arc::new(SlowMemory {
            sleep: Duration::from_millis(50),
        }),
        ProcessBitness::X64,
    );
    set_read_plan_deadline_for_tests(&host, Duration::from_millis(10));

    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.timeout",
        &["proc-timeout"],
        ReadPlanPluginConfig::default(),
    );
    host.process_appeared(test_process(pid, "proc-timeout"));
    let handle = expect_attached(&events);

    commands
        .send(ReadPlanCommand::SubmitSync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(0),
            // Two stages: the deadline is only checked once per stage, so
            // a single slow stage alone would still finish and return Ok.
            stages: vec![one_op_stage(1), one_op_stage(1)],
        })
        .expect("sending the sync submit command");

    match events.recv_timeout(RECV_TIMEOUT).expect("sync result") {
        ReadPlanEvent::SyncResult(Err(PlanMemoryError::Rejected(status))) => {
            assert_eq!(status, vosu_status::VOSU_ERR_TIMED_OUT);
        }
        other => panic!("expected a TIMED_OUT SyncResult, got {other:?}"),
    }
}

/// §5: a read plan touching a session this plugin did not match answers
/// `VOSU_ERR_PERMISSION_DENIED`.
#[test]
fn permission_denied_for_another_plugins_session() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events_a, commands_a) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.a",
        &["proc-a"],
        ReadPlanPluginConfig::default(),
    );
    let (events_b, _commands_b) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(2),
        "testkit.readplan.b",
        &["proc-b"],
        ReadPlanPluginConfig::default(),
    );

    host.process_appeared(test_process(901, "proc-a"));
    let _handle_a = expect_attached(&events_a);
    host.process_appeared(test_process(902, "proc-b"));
    let handle_b = expect_attached(&events_b);

    commands_a
        .send(ReadPlanCommand::SubmitAsync {
            plan: 1,
            handle: handle_b,
            root: PlanBase::Absolute(0),
            stages: Vec::new(),
        })
        .expect("sending the async submit command");

    assert_eq!(
        expect_status(&events_a),
        vosu_status::VOSU_ERR_PERMISSION_DENIED
    );
}

/// §5: a session whose process is already gone answers
/// `VOSU_ERR_TARGET_GONE`.
#[test]
fn dead_process_answers_target_gone() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.dead",
        &["proc-dead"],
        ReadPlanPluginConfig::default(),
    );

    // Not registered through `set_session_memory_hook_for_tests`, and far
    // past any real pid this machine will ever assign.
    let pid = 999_999_999;
    host.process_appeared(test_process(pid, "proc-dead"));
    let handle = expect_attached(&events);

    commands
        .send(ReadPlanCommand::SubmitSync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(0),
            stages: Vec::new(),
        })
        .expect("sending the sync submit command");

    match events.recv_timeout(RECV_TIMEOUT).expect("sync result") {
        ReadPlanEvent::SyncResult(Err(PlanMemoryError::Rejected(status))) => {
            assert_eq!(status, vosu_status::VOSU_ERR_TARGET_GONE);
        }
        other => panic!("expected a TARGET_GONE SyncResult, got {other:?}"),
    }
}

/// A [`ProcessMemory`] whose every read blocks until [`release_gate`] is
/// called, so a plan admitted under it never finishes (and so never frees
/// its [`OutstandingPlans`] slot by itself) until the test releases it —
/// the only way to hold 64 async plans genuinely outstanding at once
/// deterministically, rather than racing real near-instant reads.
struct GatedMemory {
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl ProcessMemory for GatedMemory {
    fn read_exact(&self, _address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let (lock, condvar) = &*self.gate;
        let mut released = lock.lock().unwrap_or_else(|poison| poison.into_inner());
        while !*released {
            released = condvar
                .wait(released)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        dst.fill(0);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

fn release_gate(gate: &Arc<(Mutex<bool>, Condvar)>) {
    let (lock, condvar) = &**gate;
    *lock.lock().unwrap_or_else(|poison| poison.into_inner()) = true;
    condvar.notify_all();
}

/// §5/§6.3: the 64-outstanding-async-plan cap holds; `READ_PLAN_CANCEL`
/// frees a slot immediately (synchronously, independent of whether its
/// worker has even started); a session's detach frees every slot it still
/// held.
#[test]
fn outstanding_plans_capped_then_freed_by_cancel_and_detach() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.cap",
        &["proc-cap"],
        ReadPlanPluginConfig::default(),
    );

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let pid1 = 500_100_001;
    let pid2 = 500_100_002;
    set_session_memory_hook_for_tests(
        pid1,
        Arc::new(GatedMemory {
            gate: Arc::clone(&gate),
        }),
        ProcessBitness::X64,
    );
    set_session_memory_hook_for_tests(
        pid2,
        Arc::new(GatedMemory {
            gate: Arc::clone(&gate),
        }),
        ProcessBitness::X64,
    );

    host.process_appeared(test_process(pid1, "proc-cap"));
    let handle1 = expect_attached(&events);

    let stages = vec![one_op_stage(1)];
    for plan in 1..=64u64 {
        commands
            .send(ReadPlanCommand::SubmitAsync {
                plan,
                handle: handle1,
                root: PlanBase::Absolute(0),
                stages: stages.clone(),
            })
            .expect("sending the async submit command");
        assert_eq!(expect_status(&events), vosu_status::VOSU_OK, "plan {plan}");
    }

    // The cap holds: a 65th plan is refused outright.
    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 65,
            handle: handle1,
            root: PlanBase::Absolute(0),
            stages: stages.clone(),
        })
        .expect("sending the async submit command");
    assert_eq!(
        expect_status(&events),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );

    // Cancelling one of the 64 (still blocked on the gate, never finished
    // on its own) frees its slot immediately.
    commands
        .send(ReadPlanCommand::Cancel { plan: 1 })
        .expect("sending the cancel command");
    assert_eq!(expect_status(&events), vosu_status::VOSU_OK);
    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 66,
            handle: handle1,
            root: PlanBase::Absolute(0),
            stages: stages.clone(),
        })
        .expect("sending the async submit command");
    assert_eq!(expect_status(&events), vosu_status::VOSU_OK);

    // Detaching the session frees every slot it still held, for the whole
    // plugin instance (not just that one session).
    host.process_exited(pid1);
    host.process_appeared(test_process(pid2, "proc-cap"));
    let handle2 = expect_attached(&events);
    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 67,
            handle: handle2,
            root: PlanBase::Absolute(0),
            stages,
        })
        .expect("sending the async submit command");
    assert_eq!(expect_status(&events), vosu_status::VOSU_OK);

    // Let every gated worker (cancelled or not) finish, so dropping `host`
    // below does not wait on one still blocked.
    release_gate(&gate);
}
