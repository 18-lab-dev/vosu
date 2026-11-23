//! Core read-plan routing: async/dependent-stage reads, permission
//! checks, module-table caching, open-error classification, async
//! per-read NotReady/timeout delivery (P1-3), and the synchronous
//! `ref_op`-width admission check (F6). Split out of `tests.rs` so this
//! module stays under the maintained-file line limit
//! (`docs/architecture/source-map.md`).

use std::sync::Arc;
use std::time::Duration;

use vosu_plugin_api::{PlanBase, PlanOp, ReadResult, Stage, StageBase, vosu_status};
use vosu_plugin_testkit::read_plan::{ReadPlanCommand, ReadPlanPluginConfig};
use vosu_sys::process::MemoryError;

use super::super::super::tests::{test_host, test_process};
use super::super::*;
use super::helpers::{
    DyingMemory, SlowMemory, create_read_plan_instance, expect_attached, expect_status,
    expect_status_and_async_result, one_op_stage,
};

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

    let (status, result) = expect_status_and_async_result(&events);
    assert_eq!(status, vosu_status::VOSU_OK);
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

    let (status, result) = expect_status_and_async_result(&events);
    assert_eq!(status, vosu_status::VOSU_OK);
    assert_eq!(result.results.len(), 2);
    match &result.results[1] {
        ReadResult::Ok(bytes) => {
            assert_eq!(bytes.as_slice(), &inner.to_le_bytes());
        }
        other => panic!("expected Ok, got {other:?}"),
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

/// P1-1: a hit never rebuilds; a miss (empty cache, or a module the
/// cached table does not have) rebuilds exactly once per call, even when
/// the fresh table still cannot resolve it — the module may have loaded
/// since the last snapshot, so the next miss tries again rather than
/// trusting a stale "not found" forever.
#[test]
fn resolve_cached_hits_without_rebuilding_and_refreshes_on_miss() {
    struct FixedTable(std::collections::HashMap<&'static str, u64>);
    impl ModuleResolver for FixedTable {
        fn resolve(&self, module: &str) -> Option<u64> {
            self.0.get(module).copied()
        }
    }

    let cache: Mutex<Option<Arc<dyn ModuleResolver + Send + Sync>>> = Mutex::new(None);
    let builds = std::sync::atomic::AtomicUsize::new(0);

    // First resolve: cache empty, so `build` runs once and finds it.
    let mut found = std::collections::HashMap::new();
    found.insert("found.dll", 0x1000u64);
    let address = resolve_cached(&cache, "found.dll", || {
        builds.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(Arc::new(FixedTable(found.clone())) as Arc<dyn ModuleResolver + Send + Sync>)
    });
    assert_eq!(address, Some(0x1000));
    assert_eq!(builds.load(std::sync::atomic::Ordering::Relaxed), 1);

    // Second resolve of the same module: a cache hit, `build` must not
    // run again.
    let address = resolve_cached(&cache, "found.dll", || {
        builds.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        panic!("a cache hit must never rebuild")
    });
    assert_eq!(address, Some(0x1000));
    assert_eq!(builds.load(std::sync::atomic::Ordering::Relaxed), 1);

    // A different module the cached table does not have: a miss, so
    // `build` runs again.
    let mut refreshed = std::collections::HashMap::new();
    refreshed.insert("found.dll", 0x1000u64);
    refreshed.insert("new.dll", 0x2000u64);
    let address = resolve_cached(&cache, "new.dll", || {
        builds.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(Arc::new(FixedTable(refreshed.clone())) as Arc<dyn ModuleResolver + Send + Sync>)
    });
    assert_eq!(address, Some(0x2000));
    assert_eq!(builds.load(std::sync::atomic::Ordering::Relaxed), 2);
}

/// P2-8: only a `pid` that is actually dead maps to `TargetGone`; a live
/// process this host simply failed to open does not look dead to the
/// plugin, and a `PermissionDenied` error is reported as such regardless
/// of liveness.
#[test]
fn classify_open_error_distinguishes_dead_denied_and_backend() {
    let permission_denied = MemoryError::PermissionDenied {
        address: 0,
        len: 0,
        message: "denied".to_owned(),
    };
    assert_eq!(
        classify_open_error(u32::MAX, &permission_denied),
        SessionMemoryError::PermissionDenied
    );

    let backend = MemoryError::Backend {
        operation: "open",
        message: "boom".to_owned(),
    };
    // `u32::MAX` is not a real pid on any test machine this gate runs on.
    assert_eq!(
        classify_open_error(u32::MAX, &backend),
        SessionMemoryError::TargetGone
    );
    // This test process's own pid is alive, so the same error kind must
    // not look like a dead target.
    assert_eq!(
        classify_open_error(std::process::id(), &backend),
        SessionMemoryError::Backend
    );
}

/// P1-3 (§5): the async path's own per-read conversion stays async-only
/// (F2: `VOSU_READ_PLAN_SYNC` gets whole-call errors instead,
/// `sync::sync_deadline_times_out`'s own doc comment). A dead cached
/// target answers `VOSU_OK` plus all-`NotReady` reads; a deadline passed
/// mid-plan answers `VOSU_OK` with the completed stage's own result kept
/// and the remaining stage reporting a per-read timeout — never a
/// whole-call rejection either way.
#[test]
fn async_per_read_notready_and_timeout_are_delivered() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());

    // Dead target: every read answers NotReady, never a call rejection.
    let dead_pid = 500_100_401;
    let memory = Arc::new(DyingMemory::new());
    memory.kill();
    set_session_memory_hook_for_tests(dead_pid, memory, ProcessBitness::X64);
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.asyncnotready",
        &["proc-asyncnotready"],
        ReadPlanPluginConfig::default(),
    );
    host.process_appeared(test_process(dead_pid, "proc-asyncnotready"));
    let handle = expect_attached(&events);
    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(0),
            stages: vec![one_op_stage(1)],
        })
        .expect("sending the async submit command");
    let (status, result) = expect_status_and_async_result(&events);
    assert_eq!(status, vosu_status::VOSU_OK);
    assert_eq!(result.results, vec![ReadResult::NotReady]);

    // Deadline passed mid-plan: the completed stage keeps its result,
    // the remaining stage reports a per-read timeout instead.
    let timeout_pid = 500_100_402;
    set_session_memory_hook_for_tests(
        timeout_pid,
        Arc::new(SlowMemory {
            sleep: Duration::from_millis(50),
        }),
        ProcessBitness::X64,
    );
    set_read_plan_deadline_for_tests(&host, Duration::from_millis(10));
    let (events2, commands2) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(2),
        "testkit.readplan.asynctimeout",
        &["proc-asynctimeout"],
        ReadPlanPluginConfig::default(),
    );
    host.process_appeared(test_process(timeout_pid, "proc-asynctimeout"));
    let handle2 = expect_attached(&events2);
    commands2
        .send(ReadPlanCommand::SubmitAsync {
            plan: 1,
            handle: handle2,
            root: PlanBase::Absolute(0),
            stages: vec![one_op_stage(1), one_op_stage(1)],
        })
        .expect("sending the async submit command");
    let (status2, result2) = expect_status_and_async_result(&events2);
    assert_eq!(status2, vosu_status::VOSU_OK);
    assert_eq!(result2.results.len(), 2);
    assert!(matches!(result2.results[0], ReadResult::Ok(_)));
    match &result2.results[1] {
        ReadResult::Error { code, message } => {
            assert_eq!(*code, vosu_plugin_api::ReadErrorCode::Backend);
            assert_eq!(message, "plan deadline exceeded");
        }
        other => panic!("expected a timeout Error for the second stage, got {other:?}"),
    }
}

/// P1-3 (§6.3): a dependent stage's `ref_op` whose declared `length`
/// does not match the session's pointer width is validated
/// synchronously, before admission, so an async submission's own `send`
/// reports `VOSU_ERR_INVALID_ARGUMENT` directly — never admitted, never
/// silently accepted only for a worker with no synchronous caller left
/// to discover it later.
#[test]
fn async_ref_op_wrong_width_is_invalid_argument() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.refopwidth",
        &["proc-refopwidth"],
        ReadPlanPluginConfig::default(),
    );

    let pid = std::process::id();
    host.process_appeared(test_process(pid, "proc-refopwidth"));
    let handle = expect_attached(&events);

    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(0),
            stages: vec![
                // This test process's own pointer width is 8 bytes; this
                // op declares 4, so stage 1's dependent `ref_op` mismatches.
                one_op_stage(4),
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

    assert_eq!(
        expect_status(&events),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert!(
        events.recv_timeout(Duration::from_millis(200)).is_err(),
        "a refused submission must never still produce an AsyncResult"
    );
}

/// N2: a failed process open must never be cached — only `Ok` is, so the
/// next submission against the same handle retries the open and
/// succeeds instead of replaying the same failure for the rest of the
/// session's life (e.g. `ptrace_scope` denying the open once, then the
/// user lowering it at runtime).
#[test]
fn a_failed_open_is_never_cached_so_the_next_submission_retries_and_succeeds() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.retryafteropen",
        &["proc-retryafteropen"],
        ReadPlanPluginConfig::default(),
    );

    let pid = 500_100_501;
    set_open_failure_count_for_tests(pid, 1);
    set_session_memory_hook_for_tests(pid, Arc::new(DyingMemory::new()), ProcessBitness::X64);
    host.process_appeared(test_process(pid, "proc-retryafteropen"));
    let handle = expect_attached(&events);

    let stages = vec![one_op_stage(1)];
    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(0),
            stages: stages.clone(),
        })
        .expect("sending the first submit command");
    assert_eq!(
        expect_status(&events),
        vosu_status::VOSU_ERR_PERMISSION_DENIED,
        "the first submission must hit the injected open failure"
    );

    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 2,
            handle,
            root: PlanBase::Absolute(0),
            stages,
        })
        .expect("sending the retry submit command");
    // N2's retry actually opens and admits the plan this time (unlike
    // the first submission, rejected before admission), so its
    // `SubmitStatus` and `READ_PLAN_RESULT` can arrive in either order
    // (`expect_status_and_async_result`'s own doc comment) — a plain
    // `expect_status` here raced `AsyncResult` arriving first and
    // occasionally failed on "expected SubmitStatus, got AsyncResult"
    // under load, not on the behavior this test actually claims to
    // check.
    let (status, result) = expect_status_and_async_result(&events);
    assert_eq!(
        status,
        vosu_status::VOSU_OK,
        "a submission after a failed open must retry the open, not replay the same failure"
    );
    assert_eq!(
        result.plan, 2,
        "the retry's own READ_PLAN_RESULT must name its own plan id"
    );
}
