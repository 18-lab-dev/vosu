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

/// `SubmitStatus` and `AsyncResult` for one submission can race in
/// either order once the job is admitted: the submitting thread resumes
/// from `send`'s own synchronous return at the same time a worker thread
/// may already be delivering the result through the instance's own
/// dispatch thread (§5 promises a *later* message, never one strictly
/// after `send` itself returns), mirroring `host/scans/tests.rs`'s own
/// `expect_status_and_result` (P2-10). Receives exactly these two
/// events, in whichever order they arrive.
fn expect_status_and_async_result(
    events: &Receiver<ReadPlanEvent>,
) -> (vosu_status, vosu_plugin_api::ReadPlanResult) {
    let mut status = None;
    let mut result = None;
    for _ in 0..2 {
        match events
            .recv_timeout(RECV_TIMEOUT)
            .expect("submit status or READ_PLAN_RESULT")
        {
            ReadPlanEvent::SubmitStatus(value) => status = Some(value),
            ReadPlanEvent::AsyncResult(value) => result = Some(value),
            other => panic!("unexpected event {other:?}"),
        }
    }
    (
        status.expect("SubmitStatus"),
        result.expect("READ_PLAN_RESULT"),
    )
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

/// P1-3: a deadline passed mid-plan does not reject the whole call
/// (`VOSU_ERR_TIMED_OUT`) — the completed stage keeps its result, and the
/// stage that did not get to run reports `VOSU_READ_ERROR`/
/// `VOSU_READ_ERROR_BACKEND` ("plan deadline exceeded") instead, so an
/// async submission's worker would still have an answer to deliver.
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
        ReadPlanEvent::SyncResult(Ok(result)) => {
            assert_eq!(result.results.len(), 2);
            assert!(matches!(result.results[0], ReadResult::Ok(_)));
            match &result.results[1] {
                ReadResult::Error { code, message } => {
                    assert_eq!(*code, vosu_plugin_api::ReadErrorCode::Backend);
                    assert_eq!(message, "plan deadline exceeded");
                }
                other => panic!("expected a timeout Error for the second stage, got {other:?}"),
            }
        }
        other => panic!("expected an Ok SyncResult with a partial timeout, got {other:?}"),
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

    // Release every gated worker (cancelled or not): dropping `host`
    // below runs `background_pool`'s own `Drop`, which joins every
    // worker thread on this (the test's) thread — leaving one gated
    // worker blocked would make that join (correctly) block until it
    // finishes, not hang forever, but there would be nothing left to
    // release it from inside this test.
    release_gate(&gate);
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
/// starve another's admitted work by filling that shared queue —
/// `spawn_or_overflow` runs overflow work on a one-off thread instead of
/// refusing it. Three instances each submit their own full 64-outstanding
/// budget (192 total) of async plans gated on the same never-yet-released
/// memory, guaranteeing the pool's 128-capacity queue genuinely fills;
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
