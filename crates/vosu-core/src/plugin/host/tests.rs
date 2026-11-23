use std::sync::Condvar;
use std::time::Instant;

use serde_json::{Map, Value};
use vosu_plugin_testkit::session::{self, OnAttach, SessionEvent, SessionPluginConfig};

use super::super::focus;
use super::super::registry;
use super::super::settings::{SettingsError, SettingsStorage};
use super::*;
use crate::logging::{ConsoleSink, LoggerOptions};

pub(super) struct DummyStorage;
impl SettingsStorage for DummyStorage {
    fn load(&self, _plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
        Ok(Map::new())
    }
    fn store(&self, _plugin_id: &str, _values: &Map<String, Value>) -> Result<(), SettingsError> {
        Ok(())
    }
}

pub(super) struct SilentConsole;
impl ConsoleSink for SilentConsole {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _lines: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
}

/// Returns the backing `TempDir` too, so the caller keeps it alive
/// for exactly its own test's scope instead of leaking it into
/// `/tmp` forever. Uses [`focus::SyntheticForegroundSource`] with no
/// foreground owner (equivalent in every test outcome to
/// `OsForegroundSource` on every non-Windows build this gate runs on,
/// and deterministic instead of depending on real OS focus) and the
/// real 5-second identification timeout; tests that need a different
/// foreground or a short timeout use [`test_host_with`] instead.
pub(super) fn test_host(runtime: Handle) -> (tempfile::TempDir, Arc<PluginHost>) {
    test_host_with(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
        super::super::session::IDENTIFICATION_TIMEOUT,
    )
}

/// Same as [`test_host`], injecting `foreground` and
/// `identification_timeout` through [`PluginHost::new_for_tests`].
pub(super) fn test_host_with(
    runtime: Handle,
    foreground: Arc<dyn ForegroundSource>,
    identification_timeout: Duration,
) -> (tempfile::TempDir, Arc<PluginHost>) {
    let log_dir = tempfile::tempdir().unwrap();
    let logger = Logger::new(
        log_dir.path(),
        LoggerOptions {
            version: "0.1.0".to_owned(),
            debug_log: false,
            queue_capacity: 64,
        },
        Arc::new(SilentConsole),
    )
    .unwrap();
    let settings = Arc::new(PluginSettingsService::new(Arc::new(DummyStorage)));
    (
        log_dir,
        PluginHost::new_for_tests(
            logger,
            settings,
            runtime,
            foreground,
            identification_timeout,
        ),
    )
}

/// Bounds every `recv` this phase's session/focus tests make on a
/// [`session::SessionEvent`] channel: generous enough that a slow CI
/// machine never flakes, but still a hard failure (not a hang) when
/// the host wires something up wrong.
const RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// A minimal game-plugin manifest with a `[match]` table matching
/// every name in `process_names` exactly (§6.1).
fn session_manifest(plugin_id: &str, process_names: &[&str]) -> String {
    let names = process_names
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=[{names}]\n"
    )
}

/// Registers a [`session::SessionPlugin`] instance under `plugin_id`,
/// matching every name in `process_names`, with `config`. Creates it
/// synchronously (`PluginHost::create_instance` runs the plugin's own
/// `create` before returning), so the returned event [`Receiver`] is
/// ready to observe `SESSION_ATTACHED` the moment the caller drives a
/// matching [`ProcessRecord`] through [`PluginHost::process_appeared`].
fn create_session_instance(
    host: &Arc<PluginHost>,
    id: PluginInstanceId,
    plugin_id: &str,
    process_names: &[&str],
    config: SessionPluginConfig,
) -> std::sync::mpsc::Receiver<SessionEvent> {
    create_session_instance_from_manifest(
        host,
        id,
        plugin_id,
        config,
        &session_manifest(plugin_id, process_names),
    )
}

/// Same as [`create_session_instance`], with a caller-supplied
/// `[match]` table (`manifest_text`): lets a test match on
/// `process_path_patterns`/`command_line_patterns` instead of exact
/// `process_names`, e.g. to shadow another plugin's `process_names`
/// entry without tripping `MatcherEngine::register`'s own D18
/// static-overlap refusal (which only compares `process_names`/
/// `comm_hints` across plugins, not path/command-line patterns).
fn create_session_instance_from_manifest(
    host: &Arc<PluginHost>,
    id: PluginInstanceId,
    plugin_id: &str,
    config: SessionPluginConfig,
    manifest_text: &str,
) -> std::sync::mpsc::Receiver<SessionEvent> {
    let events = session::register_with_config(plugin_id, config);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: manifest_text,
        vtable: session::vosu_plugin_vtable,
    }])
    .expect("loading the session testkit plugin");
    let data_dir = tempfile::tempdir().unwrap();
    host.create_instance(id, &loaded[0], data_dir.path().to_path_buf(), None)
        .expect("creating the session testkit instance");
    events
}

/// A manifest whose `[match]` table matches by `process_path_patterns`
/// instead of `process_names` — see
/// [`create_session_instance_from_manifest`]'s doc comment.
fn session_manifest_by_path_suffix(plugin_id: &str, name: &str) -> String {
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_path_patterns=['.*/{name}']\n"
    )
}

pub(super) fn test_process(pid: u32, name: &str) -> ProcessRecord {
    ProcessRecord {
        pid,
        name: name.to_owned(),
        path: format!("/games/{name}"),
        command_line: name.to_owned(),
        bitness: Bitness::X64,
    }
}

/// M1 Exit: "`SESSION_ATTACHED` -> `SESSION_IDENTIFY` -> first
/// `FOCUS_CHANGED`" driven end to end through a real
/// [`session::SessionPlugin`] instance, not `SessionRegistry`/
/// `FocusTracker` in isolation.
#[test]
fn attach_then_identify_then_focus_changed() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.basic",
        &["proc1.exe"],
        SessionPluginConfig::default(),
    );

    host.process_appeared(test_process(100, "proc1.exe"));

    let attached = match events.recv_timeout(RECV_TIMEOUT).expect("SESSION_ATTACHED") {
        SessionEvent::Attached(attached) => attached,
        other => panic!("expected Attached, got {other:?}"),
    };
    assert_eq!(attached.pid, 100);

    match events.recv_timeout(RECV_TIMEOUT).expect("FOCUS_CHANGED") {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, attached.handle);
            assert!(focus.focused, "the only identified session must be focused");
        }
        other => panic!("expected Focus, got {other:?}"),
    }
    assert!(events.try_recv().is_err(), "no further events expected");
}

/// `docs/v2/api-v1.md` "Focus rule" delivery ordering: "the session
/// losing focus [is told] before the session gaining it". Forces a
/// real transition — not merely the always-once promise to a newly
/// identified session — by switching the synthetic OS foreground
/// owner to the second process between the two attaches, so the
/// foreground-owner rule itself picks the second session over the
/// first's recency.
#[test]
fn focus_changed_loss_before_gain_on_foreground_switch() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let foreground = Arc::new(focus::SyntheticForegroundSource::new(None));
    let (_log_dir, host) = test_host_with(
        runtime.handle().clone(),
        foreground.clone(),
        super::super::session::IDENTIFICATION_TIMEOUT,
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.switch",
        &["proc1.exe", "proc2.exe"],
        SessionPluginConfig::default(),
    );

    host.process_appeared(test_process(100, "proc1.exe"));
    let h1 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 1") {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    match events.recv_timeout(RECV_TIMEOUT).expect("focus 1") {
        SessionEvent::Focus(focus) => assert!(focus.focused && focus.handle == h1),
        other => panic!("expected Focus, got {other:?}"),
    }

    foreground.set(Some(200));
    host.process_appeared(test_process(200, "proc2.exe"));
    let h2 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 2") {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    match events.recv_timeout(RECV_TIMEOUT).expect("focus loss") {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, h1);
            assert!(!focus.focused, "the first session must lose focus first");
        }
        other => panic!("expected Focus, got {other:?}"),
    }
    match events.recv_timeout(RECV_TIMEOUT).expect("focus gain") {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, h2);
            assert!(focus.focused, "the foreground owner must gain focus second");
        }
        other => panic!("expected Focus, got {other:?}"),
    }
}

/// §6.2: `focus_priority` is held by the first session that requests
/// it, and a second request while it is held is ignored (logged, not
/// refused): the second session's own first `FOCUS_CHANGED` still
/// arrives (always sent once), but reports `focused: false` since the
/// holder keeps focus.
#[test]
fn focus_priority_hold_and_ignore() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.priority",
        &["proc1.exe", "proc2.exe"],
        SessionPluginConfig {
            focus_priority: true,
            on_attach: OnAttach::Identify,
        },
    );

    host.process_appeared(test_process(100, "proc1.exe"));
    let h1 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 1") {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    match events.recv_timeout(RECV_TIMEOUT).expect("focus 1") {
        SessionEvent::Focus(focus) => assert!(focus.focused && focus.handle == h1),
        other => panic!("expected Focus, got {other:?}"),
    }

    host.process_appeared(test_process(200, "proc2.exe"));
    let h2 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 2") {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    match events.recv_timeout(RECV_TIMEOUT).expect("focus 2") {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, h2);
            assert!(
                !focus.focused,
                "the priority holder must keep focus; the second requester is ignored, not granted"
            );
        }
        other => panic!("expected Focus, got {other:?}"),
    }
    assert_eq!(
        host.focus
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .focused(),
        Some(FocusKey(h1)),
        "focus_priority's holder keeps focus across a second ignored request"
    );
}

/// D6: a declined session is detached from the decliner
/// (`SESSION_DETACHED{reason: declined}`) and re-offered, same
/// generation, to the next candidate in install order, which then
/// identifies normally.
#[test]
fn decline_re_offers_to_next_candidate_and_detaches_decliner() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let decliner_events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.decliner",
        &["shared.exe"],
        SessionPluginConfig {
            focus_priority: false,
            on_attach: OnAttach::Decline,
        },
    );
    let accepter_events = create_session_instance_from_manifest(
        &host,
        PluginInstanceId::new(2),
        "testkit.session.accepter",
        SessionPluginConfig::default(),
        &session_manifest_by_path_suffix("testkit.session.accepter", "shared.exe"),
    );

    host.process_appeared(test_process(300, "shared.exe"));

    let declined_handle = match decliner_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("decliner attach")
    {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    match decliner_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("decliner detach")
    {
        SessionEvent::Detached(detached) => {
            assert_eq!(detached.handle, declined_handle);
            assert_eq!(detached.reason, DetachReason::Declined);
        }
        other => panic!("expected Detached, got {other:?}"),
    }
    assert!(
        decliner_events.try_recv().is_err(),
        "the decliner must not be re-offered its own declined session"
    );

    let reoffered_handle = match accepter_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("accepter attach")
    {
        SessionEvent::Attached(attached) => {
            assert_eq!(attached.pid, 300);
            attached.handle
        }
        other => panic!("expected Attached, got {other:?}"),
    };
    match accepter_events
        .recv_timeout(RECV_TIMEOUT)
        .expect("accepter focus")
    {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, reoffered_handle);
            assert!(focus.focused);
        }
        other => panic!("expected Focus, got {other:?}"),
    }
}

/// §6.2: "If identification does not arrive within 5 seconds of
/// `SESSION_ATTACHED`, core logs once and keeps the session hidden" —
/// exercised with a short injected timeout rather than a real 5-second
/// wait. The pending session is never promoted: `SessionRegistry`
/// still reports it pending, not identified, once the sweep has run.
#[test]
fn identification_timeout_sweep_keeps_an_unidentified_session_hidden() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host_with(
        runtime.handle().clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
        Duration::from_millis(50),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.silent",
        &["proc1.exe"],
        SessionPluginConfig {
            focus_priority: false,
            on_attach: OnAttach::Ignore,
        },
    );

    host.process_appeared(test_process(100, "proc1.exe"));
    let handle = match events.recv_timeout(RECV_TIMEOUT).expect("attach") {
        SessionEvent::Attached(attached) => SessionHandle(attached.handle),
        other => panic!("expected Attached, got {other:?}"),
    };

    // The sweep ticks every 1s regardless of `identification_timeout`
    // (`identification_sweep_loop`'s own fixed interval); poll with a
    // bounded deadline for it to have logged this handle's timeout,
    // rather than guessing a fixed sleep covers at least one tick.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let logged = host
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .timeout_logged(handle);
        if logged == Some(true) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "identification_sweep_loop did not log this handle's timeout within 10s"
        );
        #[expect(
            clippy::disallowed_methods,
            reason = "a short bounded poll (10s deadline above) for the background identification_sweep_loop's 1s tick to have run; there is no event to block on instead"
        )]
        std::thread::sleep(Duration::from_millis(20));
    }

    let sessions = host.sessions.lock().unwrap_or_else(|p| p.into_inner());
    assert!(
        sessions.is_pending(handle),
        "a timed-out session stays pending, not dropped"
    );
    assert!(
        !sessions.is_identified(handle),
        "a timed-out session is never promoted to identified"
    );
    drop(sessions);
    assert!(
        events.try_recv().is_err(),
        "a timed-out session receives no message at all"
    );
}

/// Phase (d2)'s other entry point: `process_exited` detaches whatever
/// session that pid held and recomputes focus, same as any other
/// detach path.
#[test]
fn process_exited_detaches_and_recomputes_focus() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.exit",
        &["proc1.exe", "proc2.exe"],
        SessionPluginConfig::default(),
    );

    host.process_appeared(test_process(100, "proc1.exe"));
    let h1 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 1") {
        SessionEvent::Attached(attached) => SessionHandle(attached.handle),
        other => panic!("expected Attached, got {other:?}"),
    };
    events.recv_timeout(RECV_TIMEOUT).expect("focus 1");

    host.process_appeared(test_process(200, "proc2.exe"));
    let h2 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 2") {
        SessionEvent::Attached(attached) => SessionHandle(attached.handle),
        other => panic!("expected Attached, got {other:?}"),
    };
    events
        .recv_timeout(RECV_TIMEOUT)
        .expect("focus 2 (own first FOCUS_CHANGED)");

    host.process_exited(100);
    match events.recv_timeout(RECV_TIMEOUT).expect("detach on exit") {
        SessionEvent::Detached(detached) => {
            assert_eq!(detached.handle, h1.0);
            assert_eq!(detached.reason, DetachReason::Exited);
        }
        other => panic!("expected Detached, got {other:?}"),
    }
    match events
        .recv_timeout(RECV_TIMEOUT)
        .expect("focus moves to h2 once h1 exits")
    {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, h2.0);
            assert!(
                focus.focused,
                "h2 is the only remaining session after h1 exits"
            );
        }
        other => panic!("expected Focus, got {other:?}"),
    }

    assert!(
        !host
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_identified(h1),
        "the exited pid's session must no longer be tracked"
    );

    // Re-appearing the same pid before `process_exited` would have
    // been refused (`SessionRegistry::attach`'s own pid-dedup rule);
    // after it, the pid is free again — the clearest externally
    // observable proof `forget_exited_pid` actually ran.
    host.process_appeared(test_process(100, "proc1.exe"));
    match events
        .recv_timeout(RECV_TIMEOUT)
        .expect("re-attach after exit")
    {
        SessionEvent::Attached(attached) => assert_eq!(attached.pid, 100),
        other => panic!("expected Attached, got {other:?}"),
    }

    let _ = h2;
}

/// Blocks the gated plugin's own `create` until released, so a test
/// can force a `TIMER_SET` it made from inside `create` to fire —
/// and be queued, not delivered or lost — strictly before `create`
/// itself returns: an ordering this gate decides, never how fast
/// `create` happens to run on whatever machine runs this test.
struct CreateGate {
    release: Mutex<bool>,
    release_cv: Condvar,
    delivered: Mutex<bool>,
    delivered_cv: Condvar,
}

impl CreateGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            release: Mutex::new(false),
            release_cv: Condvar::new(),
            delivered: Mutex::new(false),
            delivered_cv: Condvar::new(),
        })
    }

    fn wait_for_release(&self) {
        let mut release = self.release.lock().unwrap_or_else(|p| p.into_inner());
        while !*release {
            release = self
                .release_cv
                .wait(release)
                .unwrap_or_else(|p| p.into_inner());
        }
    }

    fn release(&self) {
        *self.release.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.release_cv.notify_all();
    }

    fn mark_delivered(&self) {
        *self.delivered.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.delivered_cv.notify_all();
    }

    fn wait_for_delivery(&self, timeout: Duration) -> bool {
        let (guard, result) = self
            .delivered_cv
            .wait_timeout_while(
                self.delivered.lock().unwrap_or_else(|p| p.into_inner()),
                timeout,
                |delivered| !*delivered,
            )
            .unwrap_or_else(|p| p.into_inner());
        drop(guard);
        !result.timed_out()
    }
}

fn create_gate_registry() -> &'static Mutex<HashMap<String, Arc<CreateGate>>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, Arc<CreateGate>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

/// A plugin whose own `create` sets a one-shot timer (§6.11) and then
/// blocks on this test's [`CreateGate`] before returning, so the test
/// can hold `create` open across the real timer firing. `on_message`
/// marks the gate once it actually receives the replayed
/// `TIMER_FIRED` — the end-to-end proof this finding's fix requires,
/// not merely that something got queued.
mod gated_timer_plugin {
    use std::sync::Arc;

    use vosu_plugin_api::{
        HostServices, Init, Message, Plugin, PluginError, TimerFired, TimerSet, VtableHostServices,
        vosu_status,
    };

    use super::{CreateGate, create_gate_registry};

    pub struct GatedTimerPlugin {
        gate: Arc<CreateGate>,
    }

    impl Plugin for GatedTimerPlugin {
        fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            let status = host.send(
                &Message::TimerSet(TimerSet {
                    timer: 1,
                    interval_ms: None,
                }),
                0,
            );
            if status != vosu_status::VOSU_OK {
                return Err(status.into());
            }
            let gate = create_gate_registry()
                .lock()
                .unwrap()
                .get(&init.plugin_id)
                .cloned()
                .unwrap_or_else(|| panic!("no create gate registered for {:?}", init.plugin_id));
            gate.wait_for_release();
            Ok(Self { gate })
        }

        fn on_message(
            &mut self,
            message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            if let Message::TimerFired(TimerFired { timer: 1 }) = message {
                self.gate.mark_delivered();
            }
            Ok(())
        }
    }

    vosu_plugin_api::export_plugin!(bundled GatedTimerPlugin);
}

/// The new-finding P2 race, closed: a `TIMER_SET` the plugin makes
/// from inside its own `create` fires, and is handed to
/// `PluginHost::timer_fired`, while `create_instance` has not yet
/// replaced the `Creating` placeholder with the real instance — this
/// test gates `create` open until it observes exactly that (the
/// fire queued in `InstanceSlot::Creating`'s own `pending` list, a
/// real internal state transition this test waits on, never a fixed
/// sleep guessing the two have raced) — and then proves the queued
/// fire still reaches the plugin once `create` finally returns,
/// rather than being discarded for good.
#[test]
fn a_timer_set_during_create_that_fires_before_registration_is_queued_and_replayed() {
    let plugin_id = "acme.gated-timer";
    let gate = CreateGate::new();
    create_gate_registry()
        .lock()
        .unwrap()
        .insert(plugin_id.to_owned(), Arc::clone(&gate));

    let manifest_text = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='Gated Timer'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n"
    );
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest_text,
        vtable: gated_timer_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the gated-timer test plugin");

    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let data_dir = tempfile::tempdir().unwrap();
    let id = PluginInstanceId::new(1);

    std::thread::scope(|scope| {
        let host_ref = &host;
        let loaded_ref = &loaded[0];
        let data_dir_path = data_dir.path().to_path_buf();
        let create_thread =
            scope.spawn(move || host_ref.create_instance(id, loaded_ref, data_dir_path, None));

        // Wait for the real one-shot timer (fires after
        // `timers::MIN_TIMER_GRANULARITY`) to have already reached
        // `timer_fired` and queued itself against the still-`Creating`
        // slot — proof the race actually happened before
        // `create_instance` got anywhere near installing the real
        // instance, not an assumption resting on scheduling luck.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let queued = {
                let guard = host.instances.lock().unwrap_or_else(|p| p.into_inner());
                matches!(
                    guard.get(&id),
                    Some(InstanceSlot::Creating { pending, .. }) if !pending.is_empty()
                )
            };
            if queued {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected the timer to fire and queue against the Creating placeholder within 5s"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        gate.release();
        create_thread
            .join()
            .unwrap()
            .expect("creating the gated-timer instance");
    });

    assert!(
        gate.wait_for_delivery(Duration::from_secs(5)),
        "the timer fire queued during create must still reach the plugin once replayed"
    );

    host.destroy_all(Duration::from_secs(5));
}

/// A plugin whose own `create` blocks on this test's [`CreateGate`]
/// (reused from the timer-race fixture above) until released, and
/// whose `Drop` records that it was actually destroyed — the proof
/// `destroy_all_does_not_leak_an_instance_still_creating_when_it_races`
/// needs that losing the shutdown race tears the instance down
/// instead of merely refusing to publish it.
mod race_gated_plugin {
    use std::sync::atomic::{AtomicBool, Ordering};

    use vosu_plugin_api::{Init, Message, Plugin, PluginError, VtableHostServices};

    use super::create_gate_registry;

    pub static DESTROYED: AtomicBool = AtomicBool::new(false);

    pub struct RaceGatedPlugin;

    impl Plugin for RaceGatedPlugin {
        fn create(_host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            let gate = create_gate_registry()
                .lock()
                .unwrap()
                .get(&init.plugin_id)
                .cloned()
                .unwrap_or_else(|| panic!("no create gate registered for {:?}", init.plugin_id));
            gate.wait_for_release();
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

    impl Drop for RaceGatedPlugin {
        fn drop(&mut self) {
            DESTROYED.store(true, Ordering::SeqCst);
        }
    }

    vosu_plugin_api::export_plugin!(bundled RaceGatedPlugin);
}

/// The race this finding's fix closes: `destroy_all` must not drop a
/// still-`Creating` placeholder and declare itself done while
/// `create_instance`'s own thread is still inside the plugin's
/// `create` — otherwise that call's later swap installs a `Live`
/// entry `destroy_all` has already finished draining, leaking a
/// running instance forever. Proves both ends: `create_instance`
/// loses the race with `HostError::ShuttingDown`, and the instance it
/// built is actually destroyed (not merely unpublished), with no
/// `Live` entry left behind afterward.
#[test]
fn destroy_all_does_not_leak_an_instance_still_creating_when_it_races() {
    let plugin_id = "acme.race-gated";
    let gate = CreateGate::new();
    create_gate_registry()
        .lock()
        .unwrap()
        .insert(plugin_id.to_owned(), Arc::clone(&gate));
    race_gated_plugin::DESTROYED.store(false, Ordering::SeqCst);

    let manifest_text = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='Race Gated'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n"
    );
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest_text,
        vtable: race_gated_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the race-gated test plugin");

    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let data_dir = tempfile::tempdir().unwrap();
    let id = PluginInstanceId::new(1);

    std::thread::scope(|scope| {
        let host_ref = &host;
        let loaded_ref = &loaded[0];
        let data_dir_path = data_dir.path().to_path_buf();
        let create_thread =
            scope.spawn(move || host_ref.create_instance(id, loaded_ref, data_dir_path, None));

        // Wait for `create_instance` to register the `Creating`
        // placeholder: the plugin's own `create` is now blocked on
        // the gate.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let creating = matches!(
                host.instances
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&id),
                Some(InstanceSlot::Creating { .. })
            );
            if creating {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected the Creating placeholder to appear within 5s"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        let destroy_thread = scope.spawn(|| host_ref.destroy_all(Duration::from_secs(5)));

        // Wait for `destroy_all` to have actually set `shutting_down`
        // before releasing `create`, so this test exercises the exact
        // race window the fix closes rather than letting `create`
        // finish first by scheduling luck.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !host.shutting_down.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "expected destroy_all to observe the race within 5s"
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        gate.release();

        let create_result = create_thread.join().unwrap();
        destroy_thread.join().unwrap();

        assert!(
            matches!(create_result, Err(HostError::ShuttingDown)),
            "expected create_instance to lose the shutdown race, got {create_result:?}"
        );
    });

    assert!(
        race_gated_plugin::DESTROYED.load(Ordering::SeqCst),
        "the instance created during the shutdown race must still be destroyed, not leaked"
    );
    assert!(
        host.instances
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_empty(),
        "destroy_all must not leave a Live entry behind after racing create_instance"
    );
}
