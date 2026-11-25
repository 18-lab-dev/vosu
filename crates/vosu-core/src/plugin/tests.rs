//! Host unit tests (M1 Exit list): trust refusal of `in-process` for an
//! ineligible plugin, dispatch serialization versus `concurrent_dispatch`,
//! and the `destroy` grace timeout.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vosu_plugin_api::{
    ConcurrentPlugin, FocusChanged, HostServices, Init, Message, Plugin, PluginError,
    VtableHostServices, vosu_status,
};

use super::*;

struct NullHandler;
impl PluginHostHandler for NullHandler {
    fn handle(
        &self,
        _instance: PluginInstanceId,
        _message: Message,
        _correlation_id: u64,
    ) -> vosu_status {
        vosu_status::VOSU_OK
    }
}

/// Records every message a plugin's background work sends through its own
/// `host_ctx`, so a test can assert on it from the calling thread even when
/// the plugin sends from another thread, long after `create` returned.
struct RecordingHandler {
    tx: SyncSender<(PluginInstanceId, Message, u64)>,
}
impl PluginHostHandler for RecordingHandler {
    fn handle(
        &self,
        instance: PluginInstanceId,
        message: Message,
        correlation_id: u64,
    ) -> vosu_status {
        let _ = self.tx.send((instance, message, correlation_id));
        vosu_status::VOSU_OK
    }
}

fn init_bytes(plugin_id: &str) -> Vec<u8> {
    vosu_plugin_api::encode(&Init {
        plugin_id: plugin_id.into(),
        settings: ciborium::Value::Map(Vec::new()),
        data_dir: std::env::temp_dir(),
    })
    .unwrap()
}

fn focus_changed_message(handle: u64) -> OwnedMessage {
    OwnedMessage::from_message(
        &Message::FocusChanged(FocusChanged {
            handle,
            focused: true,
            foreground: None,
        }),
        0,
    )
    .unwrap()
}

type Window = (Instant, Instant);
type Log = Arc<Mutex<Vec<Window>>>;

fn registry() -> &'static Mutex<HashMap<String, Log>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, Log>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

fn register_log(plugin_id: &str) -> Log {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    registry()
        .lock()
        .unwrap()
        .insert(plugin_id.to_string(), Arc::clone(&log));
    log
}

fn log_for(plugin_id: &str) -> Log {
    registry()
        .lock()
        .unwrap()
        .get(plugin_id)
        .cloned()
        .unwrap_or_else(|| panic!("no log registered for {plugin_id:?}"))
}

fn manifest(id: &str, extra: &str) -> String {
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='Test'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n{extra}"
    )
}

mod trust;

// --- Dispatch serialization vs. concurrent dispatch (§4) -----------------

mod serial_recorder {
    use super::{Init, Log, Message, Plugin, PluginError, log_for};

    pub struct SerialRecorder {
        log: Log,
    }
    impl Plugin for SerialRecorder {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            init: Init,
        ) -> Result<Self, PluginError> {
            Ok(Self {
                log: log_for(&init.plugin_id),
            })
        }
        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            let start = std::time::Instant::now();
            std::thread::sleep(std::time::Duration::from_millis(20));
            self.log
                .lock()
                .unwrap()
                .push((start, std::time::Instant::now()));
            Ok(())
        }
    }
    vosu_plugin_internal::export_plugin!(bundled SerialRecorder);
}

#[test]
fn serial_dispatch_runs_messages_strictly_in_order() {
    let log = register_log("acme.serial");
    let text = manifest("acme.serial", "");
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &text,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: serial_recorder::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let instance = PluginInstance::create(
        PluginInstanceId::new(1),
        &loaded[0],
        &init_bytes("acme.serial"),
        Arc::new(NullHandler),
        None,
    )
    .unwrap();

    let mut replies = Vec::new();
    for i in 0..4 {
        replies.push(instance.dispatch(focus_changed_message(i)).unwrap());
    }
    for reply in replies {
        reply.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    instance.destroy(Duration::from_secs(5)).unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 4);
    for window in entries.windows(2) {
        assert!(
            window[0].1 <= window[1].0,
            "message {:?} overlapped with the next message {:?}: serial dispatch must not run concurrently",
            window[0],
            window[1]
        );
    }
}

mod concurrent_recorder {
    use super::{ConcurrentPlugin, Init, Log, Message, PluginError, log_for};

    pub struct ConcurrentRecorder {
        log: Log,
    }
    impl ConcurrentPlugin for ConcurrentRecorder {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            init: Init,
        ) -> Result<Self, PluginError> {
            Ok(Self {
                log: log_for(&init.plugin_id),
            })
        }
        fn on_message(&self, _message: Message, _correlation_id: u64) -> Result<(), PluginError> {
            let start = std::time::Instant::now();
            std::thread::sleep(std::time::Duration::from_millis(50));
            self.log
                .lock()
                .unwrap()
                .push((start, std::time::Instant::now()));
            Ok(())
        }
    }
    vosu_plugin_internal::export_plugin!(bundled concurrent ConcurrentRecorder);
}

#[test]
fn concurrent_dispatch_runs_messages_in_overlapping_windows() {
    let log = register_log("acme.concurrent");
    let text = manifest("acme.concurrent", "concurrent_dispatch=true\n");
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &text,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: concurrent_recorder::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let pool = WorkerPool::new(4, 64);
    let instance = PluginInstance::create(
        PluginInstanceId::new(2),
        &loaded[0],
        &init_bytes("acme.concurrent"),
        Arc::new(NullHandler),
        Some(Arc::clone(&pool)),
    )
    .unwrap();

    let started = Instant::now();
    let mut replies = Vec::new();
    for _ in 0..3 {
        replies.push(instance.dispatch(focus_changed_message(0)).unwrap());
    }
    for reply in replies {
        reply.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    let total = started.elapsed();
    instance.destroy(Duration::from_secs(5)).unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 3);
    // Three 50ms jobs run one-at-a-time would take >= 150ms; overlapping
    // them on a 4-worker pool should finish well under that.
    assert!(
        total < Duration::from_millis(140),
        "expected concurrent dispatch to overlap, took {total:?}"
    );
}

// --- `host`/`host_ctx` lifetime: send long after `create` (P0) ----------

mod stack_churn_sender {
    use super::{
        FocusChanged, HostServices, Init, Message, Plugin, PluginError, VtableHostServices,
    };

    pub struct StackChurnSender {
        host: VtableHostServices,
    }
    impl Plugin for StackChurnSender {
        fn create(host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
            Ok(Self { host })
        }
        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            let host = self.host;
            std::thread::spawn(move || {
                // Disturbs whatever stack memory `create`'s own frame used
                // to hold a (pre-fix) local host-services table, before
                // this thread calls `host.send` through its copy of
                // `VtableHostServices` -- "long after create", from
                // another thread, rather than relying on luck the way the
                // review noted the existing tests did.
                super::churn_stack(400);
                std::thread::sleep(std::time::Duration::from_millis(20));
                let _ = host.send(
                    &Message::FocusChanged(FocusChanged {
                        handle: 0xC0FFEE,
                        focused: true,
                        foreground: None,
                    }),
                    99,
                );
            });
            Ok(())
        }
    }
    vosu_plugin_internal::export_plugin!(bundled StackChurnSender);
}

#[inline(never)]
fn churn_stack(depth: u32) -> u64 {
    let buffer = [0x5Au8; 512];
    let sum: u64 = buffer.iter().map(|&b| b as u64).sum();
    if depth == 0 {
        sum
    } else {
        sum.wrapping_add(churn_stack(depth - 1))
    }
}

#[test]
fn send_from_a_thread_long_after_create_reaches_the_live_host_table() {
    let (tx, rx) = sync_channel(1);
    let handler = Arc::new(RecordingHandler { tx });
    let text = manifest("acme.churn", "");
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &text,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: stack_churn_sender::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let instance = PluginInstance::create(
        PluginInstanceId::new(20),
        &loaded[0],
        &init_bytes("acme.churn"),
        handler,
        None,
    )
    .unwrap();

    // Churn this thread's own stack between `create` and the dispatch that
    // triggers the plugin's background `send`, on top of the plugin's own
    // background thread doing the same before it sends.
    for _ in 0..8 {
        vosu_proptest::wait::JoinBounded::join_bounded(std::thread::spawn(|| churn_stack(400)))
            .unwrap();
    }

    instance
        .dispatch(focus_changed_message(0))
        .unwrap()
        .recv_timeout(Duration::from_secs(5))
        .unwrap();

    let (_, message, correlation_id) = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the plugin's background thread should have reached a live host table");
    assert!(matches!(
        message,
        Message::FocusChanged(FocusChanged {
            handle: 0xC0FFEE,
            focused: true,
            ..
        })
    ));
    assert_eq!(correlation_id, 99);

    instance.destroy(Duration::from_secs(5)).unwrap();
}

// --- Concurrent destroy drains in-flight jobs first (P1) -----------------

struct ConcurrentDestroyState {
    active: AtomicUsize,
    violation: AtomicBool,
}

fn concurrent_destroy_registry() -> &'static Mutex<HashMap<String, Arc<ConcurrentDestroyState>>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, Arc<ConcurrentDestroyState>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

fn register_concurrent_destroy_state(plugin_id: &str) -> Arc<ConcurrentDestroyState> {
    let state = Arc::new(ConcurrentDestroyState {
        active: AtomicUsize::new(0),
        violation: AtomicBool::new(false),
    });
    concurrent_destroy_registry()
        .lock()
        .unwrap()
        .insert(plugin_id.to_string(), Arc::clone(&state));
    state
}

fn concurrent_destroy_state_for(plugin_id: &str) -> Arc<ConcurrentDestroyState> {
    concurrent_destroy_registry()
        .lock()
        .unwrap()
        .get(plugin_id)
        .cloned()
        .unwrap_or_else(|| panic!("no concurrent-destroy state registered for {plugin_id:?}"))
}

mod concurrent_destroy_guard {
    use super::{
        ConcurrentDestroyState, ConcurrentPlugin, Init, Message, PluginError,
        concurrent_destroy_state_for,
    };
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    pub struct ConcurrentDestroyGuard {
        state: Arc<ConcurrentDestroyState>,
    }
    impl ConcurrentPlugin for ConcurrentDestroyGuard {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            init: Init,
        ) -> Result<Self, PluginError> {
            Ok(Self {
                state: concurrent_destroy_state_for(&init.plugin_id),
            })
        }
        fn on_message(&self, _message: Message, _correlation_id: u64) -> Result<(), PluginError> {
            self.state.active.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(80));
            self.state.active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
    }
    impl Drop for ConcurrentDestroyGuard {
        fn drop(&mut self) {
            // If `destroy` ever runs (this struct drops) while a job is
            // still active, the P1 bug (tearing the plugin down while a
            // `concurrent_dispatch` job is still in flight) would have
            // raced that job's own use of `self`.
            if self.state.active.load(Ordering::SeqCst) != 0 {
                self.state.violation.store(true, Ordering::SeqCst);
            }
        }
    }
    vosu_plugin_internal::export_plugin!(bundled concurrent ConcurrentDestroyGuard);
}

#[test]
fn destroy_waits_for_in_flight_concurrent_jobs_before_tearing_down() {
    let state = register_concurrent_destroy_state("acme.concurrent-destroy");
    let text = manifest("acme.concurrent-destroy", "concurrent_dispatch=true\n");
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &text,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: concurrent_destroy_guard::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let pool = WorkerPool::new(4, 64);
    let instance = PluginInstance::create(
        PluginInstanceId::new(21),
        &loaded[0],
        &init_bytes("acme.concurrent-destroy"),
        Arc::new(NullHandler),
        Some(Arc::clone(&pool)),
    )
    .unwrap();

    // Submit several jobs and call `destroy` without waiting for their
    // replies first: before the P1 fix, `destroy` only shut down
    // `Dispatch::Serial`, so these still-running `concurrent_dispatch`
    // jobs raced the plugin's own teardown instead of being drained first.
    let mut replies = Vec::new();
    for _ in 0..4 {
        replies.push(instance.dispatch(focus_changed_message(0)).unwrap());
    }
    instance.destroy(Duration::from_secs(5)).unwrap();

    assert!(
        !state.violation.load(Ordering::SeqCst),
        "destroy tore the plugin down while a concurrent job was still running"
    );
    for reply in replies {
        reply
            .recv_timeout(Duration::from_secs(5))
            .expect("every in-flight job should have finished before destroy returned");
    }
}

// --- Destroy grace timeout (§4) ------------------------------------------

mod slow_destroy {
    use super::{Init, Message, Plugin, PluginError};

    pub struct SlowDestroy;
    impl Plugin for SlowDestroy {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            _init: Init,
        ) -> Result<Self, PluginError> {
            Ok(Self)
        }
        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }
    impl Drop for SlowDestroy {
        fn drop(&mut self) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    vosu_plugin_internal::export_plugin!(bundled SlowDestroy);
}

#[test]
fn destroy_times_out_when_the_plugin_does_not_return_in_time() {
    let text = manifest("acme.slow", "");
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &text,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: slow_destroy::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let instance = PluginInstance::create(
        PluginInstanceId::new(3),
        &loaded[0],
        &init_bytes("acme.slow"),
        Arc::new(NullHandler),
        None,
    )
    .unwrap();

    match instance.destroy(Duration::from_millis(10)) {
        Err(HostError::DestroyTimedOut) => {}
        Err(other) => panic!("expected DestroyTimedOut, got {other}"),
        Ok(()) => panic!("expected destroy to time out"),
    }
}

mod slow_destroy_sends_late {
    use super::{
        FocusChanged, HostServices, Init, Message, Plugin, PluginError, VtableHostServices,
    };

    pub struct SlowDestroySendsLate {
        host: VtableHostServices,
    }
    impl Plugin for SlowDestroySendsLate {
        fn create(host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
            Ok(Self { host })
        }
        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }
    impl Drop for SlowDestroySendsLate {
        fn drop(&mut self) {
            // Outlives the short grace the matching test gives `destroy`:
            // exercises `host_ctx` staying valid -- leaked, not freed --
            // past the timeout (§4's accepted cost, the P1 fix), rather
            // than this `send` touching memory `destroy`'s caller already
            // freed the moment it saw `DestroyTimedOut`.
            std::thread::sleep(std::time::Duration::from_millis(150));
            let _ = self.host.send(
                &Message::FocusChanged(FocusChanged {
                    handle: 0xDEAD,
                    focused: false,
                    foreground: None,
                }),
                0,
            );
        }
    }
    vosu_plugin_internal::export_plugin!(bundled SlowDestroySendsLate);
}

#[test]
fn destroy_timeout_leaks_host_ctx_so_a_late_send_from_the_plugin_stays_valid() {
    let (tx, rx) = sync_channel(1);
    let handler = Arc::new(RecordingHandler { tx });
    let text = manifest("acme.slow-late", "");
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &text,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: slow_destroy_sends_late::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let instance = PluginInstance::create(
        PluginInstanceId::new(22),
        &loaded[0],
        &init_bytes("acme.slow-late"),
        handler,
        None,
    )
    .unwrap();

    match instance.destroy(Duration::from_millis(10)) {
        Err(HostError::DestroyTimedOut) => {}
        Err(other) => panic!("expected DestroyTimedOut, got {other}"),
        Ok(()) => panic!("expected destroy to time out"),
    }

    let (_, message, _) = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the plugin's late send should still reach a live host_ctx, not freed memory");
    assert!(matches!(
        message,
        Message::FocusChanged(FocusChanged { handle: 0xDEAD, .. })
    ));
}

// --- Dispatch queue backpressure (§4: bounded, non-blocking `try_send`) --
//
// `docs/v2/plugin-abi.md` §4/§5 are silent on what a *host-internal* full
// dispatch queue should do (the ABI's own error codes answer a plugin's
// `send` calls, not the host's own `dispatch` scheduling); `AGENTS.md`'s
// "no unbounded channels" rule and the general rule against ever blocking
// a hot path indefinitely settle it here: `SerialWorker`/`WorkerPool` use
// a bounded `std::sync::mpsc::sync_channel` and `try_send`
// (`super::dispatch`), so a full queue fails the call immediately with
// `HostError::DispatchQueueFull` rather than blocking the caller or
// growing the queue without bound.

/// A one-shot gate: `on_message` sets `started` and blocks on `proceed`
/// until this test releases it, so the test can deterministically wait
/// until the worker thread has pulled exactly one job out of its channel
/// (and is blocked executing it) before filling the channel's buffer to
/// its exact capacity — without this, a message could still be sitting in
/// the buffer instead of "in flight", making the exact capacity racy.
struct Gate {
    started: Mutex<bool>,
    started_cv: std::sync::Condvar,
    proceed: Mutex<bool>,
    proceed_cv: std::sync::Condvar,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Mutex::new(false),
            started_cv: std::sync::Condvar::new(),
            proceed: Mutex::new(false),
            proceed_cv: std::sync::Condvar::new(),
        })
    }

    fn wait_until_started(&self) {
        let mut started = self.started.lock().unwrap();
        while !*started {
            let (guard, timeout) = self
                .started_cv
                .wait_timeout(started, Duration::from_secs(5))
                .unwrap();
            started = guard;
            assert!(
                !timeout.timed_out(),
                "the worker thread should have started the first message within 5s"
            );
        }
    }

    fn release(&self) {
        *self.proceed.lock().unwrap() = true;
        self.proceed_cv.notify_all();
    }
}

fn gate_registry() -> &'static Mutex<HashMap<String, Arc<Gate>>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, Arc<Gate>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

fn register_gate(plugin_id: &str) -> Arc<Gate> {
    let gate = Gate::new();
    gate_registry()
        .lock()
        .unwrap()
        .insert(plugin_id.to_string(), Arc::clone(&gate));
    gate
}

fn gate_for(plugin_id: &str) -> Arc<Gate> {
    gate_registry()
        .lock()
        .unwrap()
        .get(plugin_id)
        .cloned()
        .unwrap_or_else(|| panic!("no gate registered for {plugin_id:?}"))
}

mod blocking_plugin {
    use super::{Gate, Init, Message, Plugin, PluginError, gate_for};
    use std::sync::Arc;

    pub struct BlockingPlugin {
        gate: Arc<Gate>,
    }
    impl Plugin for BlockingPlugin {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            init: Init,
        ) -> Result<Self, PluginError> {
            Ok(Self {
                gate: gate_for(&init.plugin_id),
            })
        }
        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            *self.gate.started.lock().unwrap() = true;
            self.gate.started_cv.notify_all();
            let proceed = self.gate.proceed.lock().unwrap();
            let (proceed, _) = self
                .gate
                .proceed_cv
                .wait_timeout_while(proceed, std::time::Duration::from_secs(10), |proceed| {
                    !*proceed
                })
                .unwrap();
            assert!(*proceed, "plugin dispatch gate was not released within 10s");
            Ok(())
        }
    }
    vosu_plugin_internal::export_plugin!(bundled BlockingPlugin);
}

#[test]
fn dispatch_queue_full_reports_backpressure_without_blocking() {
    let gate = register_gate("acme.blocking");
    let text = manifest("acme.blocking", "");
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &text,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: blocking_plugin::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    let instance = PluginInstance::create(
        PluginInstanceId::new(10),
        &loaded[0],
        &init_bytes("acme.blocking"),
        Arc::new(NullHandler),
        None,
    )
    .unwrap();

    // One message is picked up by the worker thread and blocks there until
    // released; wait for that to actually happen before relying on the
    // channel buffer's exact remaining capacity.
    let first_reply = instance.dispatch(focus_changed_message(0)).unwrap();
    gate.wait_until_started();

    // The buffer is now empty (the worker already pulled the one job it is
    // blocked on): exactly `DEFAULT_QUEUE_CAPACITY` more must fit without
    // blocking or failing.
    let mut replies = vec![first_reply];
    for i in 1..=DEFAULT_QUEUE_CAPACITY as u64 {
        replies.push(
            instance
                .dispatch(focus_changed_message(i))
                .unwrap_or_else(|error| {
                    panic!("message {i} should still fit in the queue, got {error}")
                }),
        );
    }

    // The next one must be rejected immediately rather than block the
    // caller or silently grow the queue past its bound.
    let started = Instant::now();
    match instance.dispatch(focus_changed_message(u64::MAX)) {
        Err(HostError::DispatchQueueFull) => {}
        Err(other) => panic!("expected DispatchQueueFull, got {other}"),
        Ok(_) => panic!("expected the full queue to reject this dispatch"),
    }
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "a full dispatch queue must report backpressure immediately, never block the caller"
    );

    // Let every queued job (and `destroy` below) run to completion instead
    // of leaking a thread blocked forever.
    gate.release();
    for reply in replies {
        reply.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    instance.destroy(Duration::from_secs(5)).unwrap();
}
