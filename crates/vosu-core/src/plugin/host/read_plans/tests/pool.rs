//! The shared background pool: the 64-outstanding-async-plan cap,
//! `Weak<PluginHost>` self-join safety (P1-2), fairness across
//! instances (P2-7), and cancel-before-detach ordering (F6). Split out
//! of `tests.rs` so this module stays under the maintained-file line
//! limit (`docs/architecture/source-map.md`).

use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use vosu_plugin_api::{PlanBase, vosu_status};
use vosu_plugin_testkit::read_plan::{ReadPlanCommand, ReadPlanEvent, ReadPlanPluginConfig};

use super::super::super::tests::{test_host, test_process};
use super::super::*;
use super::helpers::{
    GatedMemory, RECV_TIMEOUT, create_read_plan_instance, expect_attached, expect_status,
    one_op_stage, release_gate,
};

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
    // plugin instance (not just that one session). `process_exited`
    // also dispatches `SESSION_DETACHED` for `handle1`, which this test
    // drains before the next `Attached` so `expect_attached` below sees
    // the right event.
    host.process_exited(pid1);
    match events.recv_timeout(RECV_TIMEOUT).expect("SESSION_DETACHED") {
        ReadPlanEvent::Detached {
            handle: detached_handle,
        } => assert_eq!(detached_handle, handle1),
        other => panic!("expected Detached, got {other:?}"),
    }
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

    // Release every gated worker (cancelled or not): dropping `host`
    // below runs `background_pool`'s own `Drop`, which joins every
    // worker thread on this (the test's) thread — leaving one gated
    // worker blocked would make that join (correctly) block until it
    // finishes, not hang forever, but there would be nothing left to
    // release it from inside this test.
    release_gate(&gate);
}

/// P1-2: a background job captures only a `Weak<PluginHost>`. Dropping
/// the last strong `Arc<PluginHost>` while a job is still blocked must
/// neither abort (the old bug: a strong `Arc` captured by the job let the
/// worker thread running it become the one to drop the last reference,
/// which `ScanWorkerPool::drop` then self-joins against) nor hang forever
/// — it runs on a different thread than the worker, which blocks exactly
/// until the job finishes, then returns normally.
#[test]
fn dropping_host_while_a_background_job_runs_neither_aborts_nor_self_joins() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.dropwhilerunning",
        &["proc-dropwhilerunning"],
        ReadPlanPluginConfig::default(),
    );

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let pid = 500_100_101;
    set_session_memory_hook_for_tests(
        pid,
        Arc::new(GatedMemory {
            gate: Arc::clone(&gate),
        }),
        ProcessBitness::X64,
    );
    host.process_appeared(test_process(pid, "proc-dropwhilerunning"));
    let handle = expect_attached(&events);

    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(0),
            stages: vec![one_op_stage(1)],
        })
        .expect("sending the async submit command");
    assert_eq!(expect_status(&events), vosu_status::VOSU_OK);

    // `host.instances` holds this instance, whose own `HostContext`
    // holds a strong `Arc<PluginHost>` back (so `handle()` can be called
    // from inside the plugin) — a reference cycle the test must break
    // first, exactly as real shutdown does, or the external `host` below
    // is never actually the last strong reference.
    host.destroy_all(Duration::from_secs(5));

    // The worker is still blocked inside `GatedMemory::read_exact`,
    // holding this job's own `Weak<PluginHost>` — never a strong
    // reference. Dropping the test's own `host` (now the only strong
    // reference left) on a separate thread must only ever block that
    // thread (joining `background_pool`'s workers), never abort or
    // deadlock.
    let dropper = std::thread::spawn(move || drop(host));

    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !dropper.is_finished(),
        "dropping host should still be blocked on the gated worker"
    );
    release_gate(&gate);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !dropper.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(dropper.is_finished(), "dropping host did not complete");
    dropper.join().expect("dropping host must not panic");
}

/// P2-7: a full *shared* background-pool queue must never turn already-
/// admitted work into `VOSU_ERR_INVALID_ARGUMENT`, and one plugin cannot
/// starve another's admitted work by filling that shared queue. Three
/// instances each submit their own full 64-outstanding budget (192
/// total) of async plans gated on the same never-yet-released memory;
/// every one of the 192 submissions must still answer `VOSU_OK`, and —
/// once released — every one must still receive its `READ_PLAN_RESULT`.
#[test]
fn one_plugin_cannot_starve_another_through_the_shared_pool_queue() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let instances: Vec<(Receiver<ReadPlanEvent>, SyncSender<ReadPlanCommand>, u64)> = (0..3u64)
        .map(|i| {
            // Offset away from `scans/tests.rs`'s own `500_200_0xx` pids
            // (a process-wide static hook map, module doc comment) so
            // parallel test threads never clobber each other's hook for
            // the same pid.
            let pid = 500_210_000 + i as u32;
            set_session_memory_hook_for_tests(
                pid,
                Arc::new(GatedMemory {
                    gate: Arc::clone(&gate),
                }),
                ProcessBitness::X64,
            );
            let process_name = format!("proc-starve-{i}");
            let (events, commands) = create_read_plan_instance(
                &host,
                PluginInstanceId::new(i + 1),
                &format!("testkit.readplan.starve{i}"),
                &[process_name.as_str()],
                ReadPlanPluginConfig::default(),
            );
            host.process_appeared(test_process(pid, &process_name));
            let handle = expect_attached(&events);
            (events, commands, handle)
        })
        .collect();

    let stages = vec![one_op_stage(1)];
    for (events, commands, handle) in &instances {
        for plan in 1..=64u64 {
            commands
                .send(ReadPlanCommand::SubmitAsync {
                    plan,
                    handle: *handle,
                    root: PlanBase::Absolute(0),
                    stages: stages.clone(),
                })
                .expect("sending the async submit command");
            assert_eq!(
                expect_status(events),
                vosu_status::VOSU_OK,
                "plan {plan} must be admitted: work already granted a slot must never be \
                 refused because the shared pool's queue happened to be full"
            );
        }
    }

    release_gate(&gate);

    for (events, _commands, _handle) in &instances {
        for _ in 1..=64u64 {
            match events.recv_timeout(RECV_TIMEOUT).expect("READ_PLAN_RESULT") {
                ReadPlanEvent::AsyncResult(_) => {}
                other => panic!("expected AsyncResult, got {other:?}"),
            }
        }
    }
}

/// P3 (§6.3, mirroring `host/scans/tests.rs`'s own
/// `detach_mid_scan_delivers_session_detached_error_exactly_once`):
/// `cancel_read_plans_for_session` runs before `SESSION_DETACHED` is
/// dispatched (`host/lifecycle.rs::finish_detach`'s own doc comment) —
/// proven here through a plan genuinely still outstanding (blocked on
/// `GatedMemory`) when its session detaches: once the gate finally
/// releases, the now-cancelled job's own `finish` reports `Cancelled`
/// and skips delivery entirely, so no stale `AsyncResult` ever follows
/// `SESSION_DETACHED` — the two can never arrive out of order because the
/// late one never arrives at all.
#[test]
fn process_exit_cancels_outstanding_plans_before_detach_so_no_stale_result_follows() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_read_plan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.readplan.cancelbeforedetach",
        &["proc-cancelbeforedetach"],
        ReadPlanPluginConfig::default(),
    );

    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let pid = 500_100_201;
    set_session_memory_hook_for_tests(
        pid,
        Arc::new(GatedMemory {
            gate: Arc::clone(&gate),
        }),
        ProcessBitness::X64,
    );
    host.process_appeared(test_process(pid, "proc-cancelbeforedetach"));
    let handle = expect_attached(&events);

    commands
        .send(ReadPlanCommand::SubmitAsync {
            plan: 1,
            handle,
            root: PlanBase::Absolute(0),
            stages: vec![one_op_stage(1)],
        })
        .expect("sending the async submit command");
    assert_eq!(expect_status(&events), vosu_status::VOSU_OK);

    // The plan is now outstanding, blocked in `GatedMemory::read_exact`.
    // Detach the session before releasing it.
    host.process_exited(pid);

    match events.recv_timeout(RECV_TIMEOUT).expect("SESSION_DETACHED") {
        ReadPlanEvent::Detached {
            handle: detached_handle,
        } => {
            assert_eq!(detached_handle, handle);
        }
        other => panic!("expected Detached, got {other:?}"),
    }

    // Let the gated worker finish (it may still be running, racing the
    // cancellation): its own `finish` must report `Cancelled` and skip
    // delivery, so no stale `AsyncResult` ever follows.
    release_gate(&gate);
    assert!(
        events.recv_timeout(Duration::from_millis(200)).is_err(),
        "a plan cancelled by its session's own detach must never still deliver its result"
    );
}
