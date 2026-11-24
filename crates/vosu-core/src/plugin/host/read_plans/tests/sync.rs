//! `VOSU_READ_PLAN_SYNC` (D5, §6.3): dispatch-thread refusal, the
//! whole-call `VOSU_ERR_TIMED_OUT`/`VOSU_ERR_TARGET_GONE` errors (F2),
//! and `VOSU_ERR_NOT_READY` for a torn-down instance. Split out of
//! `tests.rs` so this module stays under the maintained-file line limit
//! (`docs/architecture/source-map.md`).

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use vosu_plugin_api::{PlanBase, PlanMemoryError, ReadResult, vosu_status};
use vosu_plugin_testkit::read_plan::{ReadPlanCommand, ReadPlanEvent, ReadPlanPluginConfig};
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessMemory, ReadRequest};

use super::super::super::tests::{test_host, test_process};
use super::super::*;
use super::helpers::{
    DyingMemory, RECV_TIMEOUT, SlowMemory, create_read_plan_instance, expect_attached,
};

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
            stages: vec![super::helpers::one_op_stage(8)],
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
        super::helpers::expect_status(&events),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
}

/// F2 (D5, §6.3, §5's status table): a `VOSU_READ_PLAN_SYNC` deadline
/// passed mid-plan rejects the *whole call* with `VOSU_ERR_TIMED_OUT` —
/// `send`'s own return value, never a partial `VOSU_OK` result with a
/// per-read timeout entry. The async path's own per-read conversion
/// (P1-3) is exercised separately
/// (`basics::async_per_read_notready_and_timeout_are_delivered`).
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
            stages: vec![
                super::helpers::one_op_stage(1),
                super::helpers::one_op_stage(1),
            ],
        })
        .expect("sending the sync submit command");

    match events.recv_timeout(RECV_TIMEOUT).expect("sync result") {
        ReadPlanEvent::SyncResult(Err(PlanMemoryError::Rejected(status))) => {
            assert_eq!(status, vosu_status::VOSU_ERR_TIMED_OUT);
        }
        other => panic!("expected a TIMED_OUT SyncResult, got {other:?}"),
    }
}

/// §5: a session whose process is already gone answers
/// `VOSU_ERR_TARGET_GONE` — exercised here through a failed `open`
/// (`open_session_memory`, checked before `execute` ever runs): see
/// `sync_on_a_cached_dead_process_answers_target_gone` for `execute`'s
/// own `is_alive()` check instead.
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

/// F2's second half: a `VOSU_READ_PLAN_SYNC` against a session whose
/// process was opened and cached while alive, but has since died,
/// answers the whole-call `VOSU_ERR_TARGET_GONE` — this is `execute`'s
/// own liveness check (`memory.is_alive()`), not merely a failed `open`
/// the way `dead_process_answers_target_gone` exercises.
#[test]
fn sync_on_a_cached_dead_process_answers_target_gone() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.cacheddead",
        &["proc-cacheddead"],
        ReadPlanPluginConfig::default(),
    );

    let pid = 500_100_301;
    let memory = Arc::new(DyingMemory::new());
    set_session_memory_hook_for_tests(
        pid,
        memory.clone() as Arc<dyn ProcessMemory>,
        ProcessBitness::X64,
    );
    host.process_appeared(test_process(pid, "proc-cacheddead"));
    let handle = expect_attached(&events);

    // The process is cached as alive at this point (the open above
    // succeeded); only now does it "die".
    memory.kill();

    commands
        .send(ReadPlanCommand::SubmitSync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(0),
            stages: vec![super::helpers::one_op_stage(1)],
        })
        .expect("sending the sync submit command");

    match events.recv_timeout(RECV_TIMEOUT).expect("sync result") {
        ReadPlanEvent::SyncResult(Err(PlanMemoryError::Rejected(status))) => {
            assert_eq!(status, vosu_status::VOSU_ERR_TARGET_GONE);
        }
        other => panic!("expected a TARGET_GONE SyncResult, got {other:?}"),
    }
}

/// Signals `started` the moment a read actually begins, then blocks on
/// `gate` until released — lets `sync_on_a_torn_down_instance_answers_not_ready`
/// know its submission has genuinely reached a blocking read before
/// tearing the instance down, instead of guessing with a fixed sleep.
struct StartGatedMemory {
    started: Arc<(Mutex<bool>, Condvar)>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl ProcessMemory for StartGatedMemory {
    fn read_exact(&self, _address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        {
            let (lock, cv) = &*self.started;
            *lock.lock().unwrap_or_else(|poison| poison.into_inner()) = true;
            cv.notify_all();
        }
        let (lock, cv) = &*self.gate;
        let mut released = lock.lock().unwrap_or_else(|poison| poison.into_inner());
        while !*released {
            released = cv
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

fn wait_for_start(started: &Arc<(Mutex<bool>, Condvar)>) {
    let (lock, cv) = &**started;
    let guard = lock.lock().unwrap_or_else(|poison| poison.into_inner());
    let guard = cv
        .wait_while(guard, |started| !*started)
        .unwrap_or_else(|poison| poison.into_inner());
    drop(guard);
}

/// P3: `deliver_sync_result_raw`'s own `instances` lookup finding no
/// `Live` entry (the instance tore down while this `VOSU_READ_PLAN_SYNC`
/// submission was still blocked on its own gated read, on a thread other
/// than the one `destroy_all` runs on) answers `VOSU_ERR_NOT_READY` — the
/// submission genuinely ran to completion, but nothing was left to
/// deliver its result to.
#[test]
fn sync_on_a_torn_down_instance_answers_not_ready() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.notready",
        &["proc-notready"],
        ReadPlanPluginConfig::default(),
    );

    let pid = 500_100_303;
    let started = Arc::new((Mutex::new(false), Condvar::new()));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    set_session_memory_hook_for_tests(
        pid,
        Arc::new(StartGatedMemory {
            started: Arc::clone(&started),
            gate: Arc::clone(&gate),
        }),
        ProcessBitness::X64,
    );
    host.process_appeared(test_process(pid, "proc-notready"));
    let handle = expect_attached(&events);

    commands
        .send(ReadPlanCommand::SubmitSync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(0),
            stages: vec![super::helpers::one_op_stage(1)],
        })
        .expect("sending the sync submit command");
    wait_for_start(&started);

    // Tear the instance down while its SYNC submission is still blocked
    // inside the gated read: `deliver_sync_result_raw`'s own lookup (run
    // once `execute_into` finally returns) must then find no `Live` entry.
    host.destroy_all(Duration::from_secs(5));

    let (lock, cv) = &*gate;
    *lock.lock().unwrap_or_else(|poison| poison.into_inner()) = true;
    cv.notify_all();

    match events.recv_timeout(RECV_TIMEOUT).expect("sync result") {
        ReadPlanEvent::SyncResult(Err(PlanMemoryError::Rejected(status))) => {
            assert_eq!(status, vosu_status::VOSU_ERR_NOT_READY);
        }
        other => panic!("expected a NOT_READY SyncResult, got {other:?}"),
    }
}
