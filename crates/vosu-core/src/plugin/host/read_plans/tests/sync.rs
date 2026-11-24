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

use super::super::super::super::registry;
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
            sleep: Duration::from_millis(200),
        }),
        ProcessBitness::X64,
    );
    // At least four ~16 ms Windows ticks; the first stage deliberately
    // exceeds this plus a complete tick before the second stage is admitted.
    set_read_plan_deadline_for_tests(&host, Duration::from_millis(64));

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

    // A synthetic pid distinct from every other test's own
    // (`set_session_memory_hook_for_tests`'s own doc comment: its
    // registry is keyed by pid and process-wide, so two tests sharing
    // one pid clobber each other's hook when they run concurrently). This
    // used to collide with `pool.rs`'s own `500_100_301`
    // (`a_cancel_racing_admission_still_removes_the_queued_job_and_leaves_no_leak`),
    // which flaked natively under full-suite parallelism: whichever test's
    // `set_session_memory_hook_for_tests` call landed second silently
    // redirected the other's reads to its own `ProcessMemory`, so an
    // occupying plan here could run against `DyingMemory` instead of
    // `GatedMemory` (or vice versa) and finish instead of blocking.
    let pid = 500_100_304;
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
        let released = lock.lock().unwrap_or_else(|poison| poison.into_inner());
        let (released, _) = cv
            .wait_timeout_while(released, std::time::Duration::from_secs(10), |released| {
                !*released
            })
            .unwrap_or_else(|poison| poison.into_inner());
        assert!(*released, "sync memory gate was not released within 10s");
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

/// A tiny `ProcessMemory` whose every read trivially succeeds: the
/// nested-sync regression below only cares about the dispatch-thread
/// refusal, never about what bytes a read actually returns.
struct AlwaysOkMemory;

impl ProcessMemory for AlwaysOkMemory {
    fn read_exact(&self, _address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
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

/// A plugin that submits `VOSU_READ_PLAN_SYNC` through a raw `host.send`
/// call — never `PlanMemory` — from its own background reader thread,
/// and, the moment that submission's result reaches its own `on_message`
/// (reentrant, on that same reader thread, via
/// `PluginHost::deliver_sync_result_raw`'s `dispatch_raw` call), submits
/// a second synchronous plan the same way, immediately, nested inside
/// that very `on_message` call — exactly the case P1 of the F4 review
/// describes: any C plugin, or an SDK plugin calling
/// `HostServices::submit_read_plan_sync` directly, can reach this without
/// ever touching `PlanMemory` at all.
mod nested_sync_plugin {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
    use std::sync::{Arc, Mutex};

    use vosu_plugin_api::{
        HostServices, Init, Message, PlanBase, PlanOp, Plugin, PluginError, ReadPlanSubmit,
        SessionIdentify, Stage, StageBase, VtableHostServices, vosu_status,
    };

    #[derive(Debug)]
    pub enum NestedSyncEvent {
        Attached { handle: u64 },
        OuterSubmitStatus(vosu_status),
        NestedSubmitStatus(vosu_status),
    }

    fn events_registry() -> &'static Mutex<HashMap<String, SyncSender<NestedSyncEvent>>> {
        static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, SyncSender<NestedSyncEvent>>>> =
            std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
        &REGISTRY
    }

    type TriggerRegistry = Mutex<HashMap<String, Receiver<()>>>;
    fn triggers_registry() -> &'static TriggerRegistry {
        static REGISTRY: std::sync::LazyLock<TriggerRegistry> =
            std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
        &REGISTRY
    }

    /// Registers a fresh event channel and a one-shot trigger for
    /// `plugin_id`: call before `PluginHost::create_instance` so both
    /// exist before the plugin's own `create` can look them up. Sending
    /// on the returned `SyncSender<()>` (after observing
    /// [`NestedSyncEvent::Attached`]) tells the plugin's background
    /// reader thread to submit its own outer `VOSU_READ_PLAN_SYNC` plan.
    pub fn register(plugin_id: impl Into<String>) -> (Receiver<NestedSyncEvent>, SyncSender<()>) {
        let plugin_id = plugin_id.into();
        let (events_tx, events_rx) = sync_channel(8);
        let (trigger_tx, trigger_rx) = sync_channel(1);
        events_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(plugin_id.clone(), events_tx);
        triggers_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(plugin_id, trigger_rx);
        (events_rx, trigger_tx)
    }

    fn read_plan_submit(handle: u64, plan: u64) -> Message {
        Message::ReadPlanSubmit(ReadPlanSubmit {
            plan,
            handle,
            root: PlanBase::Absolute(0),
            stages: vec![Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![PlanOp {
                    offset: 0,
                    length: 1,
                }],
            }],
            sync: true,
        })
    }

    pub struct NestedSyncPlugin {
        host: VtableHostServices,
        events: SyncSender<NestedSyncEvent>,
        handle: Arc<AtomicU64>,
    }

    impl Plugin for NestedSyncPlugin {
        fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            let events = events_registry()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .get(&init.plugin_id)
                .cloned()
                .unwrap_or_else(|| {
                    panic!(
                        "no nested-sync event channel registered for {:?}",
                        init.plugin_id
                    )
                });
            let trigger = triggers_registry()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&init.plugin_id)
                .unwrap_or_else(|| {
                    panic!(
                        "no nested-sync trigger channel registered for {:?}",
                        init.plugin_id
                    )
                });

            let handle = Arc::new(AtomicU64::new(0));
            let reader_host = host;
            let reader_events = events.clone();
            let reader_handle = Arc::clone(&handle);
            std::thread::spawn(move || {
                if trigger
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .is_err()
                {
                    return;
                }
                let status = reader_host.send(
                    &read_plan_submit(reader_handle.load(Ordering::SeqCst), 1),
                    0,
                );
                let _ = reader_events.send(NestedSyncEvent::OuterSubmitStatus(status));
            });

            Ok(Self {
                host,
                events,
                handle,
            })
        }

        fn on_message(
            &mut self,
            message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            match message {
                Message::SessionAttached(attached) => {
                    self.handle.store(attached.handle, Ordering::SeqCst);
                    let _ = self.events.send(NestedSyncEvent::Attached {
                        handle: attached.handle,
                    });
                    let identify = SessionIdentify {
                        handle: attached.handle,
                        game: "testkit".to_owned(),
                        variant: "nested_sync".to_owned(),
                        name: "Nested Sync Testkit".to_owned(),
                        version: "0.1.0".to_owned(),
                        focus_priority: false,
                    };
                    let _ = self.host.send(&Message::SessionIdentify(identify), 0);
                }
                Message::ReadPlanResult(_) => {
                    // Reentrant on the same thread `dispatch_raw` just
                    // delivered this on: `DispatchThreadScope` (held
                    // around that call by `deliver_sync_result_raw`, the
                    // P1 fix this test proves) must make this nested
                    // submission see `on_dispatch_thread() == true` and
                    // refuse it before ever touching `SYNC_RESULT_BUF`.
                    let status = self
                        .host
                        .send(&read_plan_submit(self.handle.load(Ordering::SeqCst), 2), 0);
                    let _ = self
                        .events
                        .send(NestedSyncEvent::NestedSubmitStatus(status));
                }
                _ => {}
            }
            Ok(())
        }
    }

    vosu_plugin_api::export_plugin!(bundled NestedSyncPlugin);
}

#[test]
fn nested_sync_submit_from_result_dispatch_is_refused_not_panicked() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());

    let plugin_id = "testkit.readplan.nestedsync";
    let (events, trigger) = nested_sync_plugin::register(plugin_id);
    let manifest = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=['proc-nestedsync']\n"
    );
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest,
        settings_schema: None,
        namespace_schemas: &[],
        vtable: nested_sync_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the nested-sync testkit plugin");
    let data_dir = tempfile::tempdir().unwrap();
    host.create_instance(
        PluginInstanceId::new(1),
        &loaded[0],
        data_dir.path().to_path_buf(),
        None,
    )
    .expect("creating the nested-sync testkit instance");

    // Unique across the read-plan tests: basics.rs's open-retry test owns
    // 500_100_501, and the session-memory hook registry is process-wide.
    let pid = 500_100_601;
    set_session_memory_hook_for_tests(pid, Arc::new(AlwaysOkMemory), ProcessBitness::X64);
    host.process_appeared(test_process(pid, "proc-nestedsync"));

    let handle = match events.recv_timeout(RECV_TIMEOUT).expect("SESSION_ATTACHED") {
        nested_sync_plugin::NestedSyncEvent::Attached { handle } => handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    let _ = handle;

    trigger
        .send(())
        .expect("triggering the reader thread's outer sync submit");

    let mut outer_status = None;
    let mut nested_status = None;
    for _ in 0..2 {
        match events.recv_timeout(RECV_TIMEOUT).expect("submit status") {
            nested_sync_plugin::NestedSyncEvent::OuterSubmitStatus(status) => {
                outer_status = Some(status);
            }
            nested_sync_plugin::NestedSyncEvent::NestedSubmitStatus(status) => {
                nested_status = Some(status);
            }
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(
        nested_status.expect("NestedSubmitStatus"),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        "a SYNC submission made from inside the result's own on_message must be refused, \
         never panic on SYNC_RESULT_BUF"
    );
    assert_eq!(
        outer_status.expect("OuterSubmitStatus"),
        vosu_status::VOSU_OK,
        "the outer submission itself (from the reader thread, not nested) must still succeed"
    );
}

#[test]
fn detach_generation_and_destroy_revoke_warm_lookup() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let (_directory, host) = test_host(runtime.handle().clone());
    let instance = PluginInstanceId::new(6301);
    let (events, _commands) = create_read_plan_instance(
        &host,
        instance,
        "testkit.readplan.cache-generation",
        &["cache-generation"],
        ReadPlanPluginConfig::default(),
    );
    let pid = 500_163_001;
    set_session_memory_hook_for_tests(pid, Arc::new(AlwaysOkMemory), ProcessBitness::X64);
    host.process_appeared(test_process(pid, "cache-generation"));
    let handle = SessionHandle(expect_attached(&events));
    let first = host.sync_session_memory(instance, handle).unwrap();
    assert!(Arc::ptr_eq(
        &first,
        &host.sync_session_memory(instance, handle).unwrap()
    ));
    host.process_exited(pid);
    assert!(matches!(
        host.sync_session_memory(instance, handle),
        Err(vosu_status::VOSU_ERR_TARGET_GONE)
    ));
    assert!(
        matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), ReadPlanEvent::Detached { handle: retired } if retired == handle.0)
    );
    host.process_appeared(test_process(pid, "cache-generation"));
    let next = SessionHandle(expect_attached(&events));
    assert_ne!(handle, next);
    assert!(host.sync_session_memory(instance, next).is_ok());
    assert!(matches!(
        host.sync_session_memory(instance, handle),
        Err(vosu_status::VOSU_ERR_TARGET_GONE)
    ));
    // Prime the new handle again immediately before tearing down its instance.
    assert!(host.sync_session_memory(instance, next).is_ok());
    host.destroy_all(Duration::from_secs(1));
    assert!(matches!(
        host.sync_session_memory(instance, next),
        Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT)
    ));
    clear_session_memory_hook_for_tests(pid);
}
