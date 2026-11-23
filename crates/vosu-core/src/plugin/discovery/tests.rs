//! `DiscoveryService`/`ProcessSource` unit tests: a synthetic, in-memory
//! [`ProcessSource`] drives [`DiscoveryService`] against a real (test)
//! [`PluginHost`] with one registered testkit plugin, so these tests
//! observe discovery's effects the same way a real plugin would —
//! `SESSION_ATTACHED`/`SESSION_DETACHED` on its own event channel
//! (`vosu_plugin_testkit::session`) — rather than reaching into
//! `DiscoveryService`'s own private state. Every wait is bounded
//! (`RECV_TIMEOUT`/`SIGNAL_TIMEOUT`), never a sleep-for-ordering: the
//! synthetic source notifies [`DiscoveryService`]'s background thread the
//! moment a test mutates it, and every assertion blocks only on a channel
//! a production code path actually sends to.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::runtime::Handle;
use vosu_plugin_api::{Bitness, FocusChanged, SessionAttached, SessionDetached};
use vosu_plugin_testkit::session::{self, SessionEvent, SessionPluginConfig};

use super::*;
use crate::logging::{ConsoleSink, Logger, LoggerOptions};
use crate::plugin::focus;
use crate::plugin::host_services::PluginInstanceId;
use crate::plugin::registry;
use crate::plugin::settings::{PluginSettingsService, SettingsError, SettingsStorage};

const RECV_TIMEOUT: Duration = Duration::from_secs(5);

struct DummyStorage;
impl SettingsStorage for DummyStorage {
    fn load(&self, _plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
        Ok(Map::new())
    }
    fn store(&self, _plugin_id: &str, _values: &Map<String, Value>) -> Result<(), SettingsError> {
        Ok(())
    }
}

struct SilentConsole;
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

fn test_host(
    runtime: Handle,
    foreground: Arc<dyn super::super::focus::ForegroundSource>,
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
    let host = PluginHost::new_for_tests(
        logger,
        settings,
        runtime,
        foreground,
        crate::plugin::IDENTIFICATION_TIMEOUT,
    );
    (log_dir, host)
}

/// A minimal game-plugin manifest matching every name in `process_names`
/// exactly, with `comm_hints` (§6.1) set to the same list.
fn session_manifest(plugin_id: &str, process_names: &[&str]) -> String {
    let names = process_names
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=[{names}]\ncomm_hints=[{names}]\n"
    )
}

fn create_session_instance(
    host: &Arc<PluginHost>,
    id: PluginInstanceId,
    plugin_id: &str,
    process_names: &[&str],
) -> Receiver<SessionEvent> {
    let events = session::register_with_config(plugin_id, SessionPluginConfig::default());
    let manifest = session_manifest(plugin_id, process_names);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest,
        vtable: session::vosu_plugin_vtable,
    }])
    .expect("loading the session testkit plugin");
    let data_dir = tempfile::tempdir().unwrap();
    host.create_instance(id, &loaded[0], data_dir.path().to_path_buf(), None)
        .expect("creating the session testkit instance");
    events
}

fn test_process(pid: u32, name: &str) -> ProcessRecord {
    ProcessRecord {
        pid,
        name: name.to_owned(),
        path: format!("/games/{name}"),
        command_line: name.to_owned(),
        bitness: Bitness::X64,
    }
}

/// Blocks up to [`RECV_TIMEOUT`] total for the next event matching
/// `matches`, discarding (not failing on) every event that does not
/// match — `SESSION_IDENTIFY`'s own promised first `FOCUS_CHANGED`
/// (§6.2) arrives on the same channel and is irrelevant to most of these
/// tests.
fn expect_event(
    events: &Receiver<SessionEvent>,
    matches: impl Fn(&SessionEvent) -> bool,
) -> SessionEvent {
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "timed out waiting for a matching session event"
        );
        let event = events
            .recv_timeout(remaining)
            .expect("channel closed before a matching event arrived");
        if matches(&event) {
            return event;
        }
    }
}

fn expect_attached(events: &Receiver<SessionEvent>, pid: u32) -> SessionAttached {
    match expect_event(
        events,
        |event| matches!(event, SessionEvent::Attached(attached) if attached.pid == pid),
    ) {
        SessionEvent::Attached(attached) => attached,
        _ => unreachable!("expect_event's own predicate only matches Attached"),
    }
}

fn expect_detached(events: &Receiver<SessionEvent>, handle: u64) -> SessionDetached {
    match expect_event(
        events,
        |event| matches!(event, SessionEvent::Detached(detached) if detached.handle == handle),
    ) {
        SessionEvent::Detached(detached) => detached,
        _ => unreachable!("expect_event's own predicate only matches Detached"),
    }
}

#[derive(Default)]
struct SyntheticState {
    processes: HashMap<u32, ProcessRecord>,
    pending_inspect: Vec<u32>,
    pending_exit: Vec<u32>,
    needs_reconcile: bool,
}

/// An in-memory [`ProcessSource`] a test drives directly: every mutation
/// queues the matching event hint and wakes [`DiscoveryService`]'s
/// background thread immediately, so tests never wait out
/// [`RECONCILE_INTERVAL`].
struct SyntheticSource {
    state: Mutex<SyntheticState>,
    notifier: Notify,
    comm_hints: SyncSender<Vec<String>>,
}

impl SyntheticSource {
    fn new() -> (Arc<Self>, Receiver<Vec<String>>) {
        let (tx, rx) = sync_channel(16);
        (
            Arc::new(Self {
                state: Mutex::new(SyntheticState::default()),
                notifier: Notify::new(),
                comm_hints: tx,
            }),
            rx,
        )
    }

    fn appear(&self, record: ProcessRecord) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let pid = record.pid;
        state.processes.insert(pid, record);
        state.pending_inspect.push(pid);
        drop(state);
        self.notifier.notify_one();
    }

    fn exit(&self, pid: u32) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.processes.remove(&pid);
        state.pending_exit.push(pid);
        drop(state);
        self.notifier.notify_one();
    }
}

impl ProcessSource for SyntheticSource {
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .processes
            .values()
            .cloned()
            .collect())
    }

    fn inspect(&self, pid: u32) -> io::Result<Option<ProcessRecord>> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .processes
            .get(&pid)
            .cloned())
    }

    fn drain_events(&self) -> ProcessEventBatch {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        ProcessEventBatch {
            inspect: std::mem::take(&mut state.pending_inspect),
            exit: std::mem::take(&mut state.pending_exit),
            needs_reconcile: std::mem::take(&mut state.needs_reconcile),
        }
    }

    fn set_comm_hints(&self, hints: &[String]) -> io::Result<()> {
        // A full channel means a test stopped draining it; losing the
        // update would be the wrong failure mode for a bounded test
        // fixture, so block instead of the usual `try_send` a production
        // sink would use for an unbounded-backpressure-sensitive path.
        self.comm_hints.send(hints.to_vec()).ok();
        Ok(())
    }

    fn notifier(&self) -> Option<&Notify> {
        Some(&self.notifier)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn appear_delivers_session_attached() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.appear",
        &["osu!.exe"],
    );
    let (source, _comm_hints) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    source.appear(test_process(100, "osu!.exe"));
    let attached = expect_attached(&events, 100);
    assert_eq!(attached.pid, 100);

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_delivers_session_detached() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.exit",
        &["osu!.exe"],
    );
    let (source, _comm_hints) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    source.appear(test_process(200, "osu!.exe"));
    let attached = expect_attached(&events, 200);

    source.exit(200);
    let detached = expect_detached(&events, attached.handle);
    assert_eq!(detached.reason, vosu_plugin_api::DetachReason::Exited);

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reused_pid_is_reported_as_exit_then_a_fresh_appearance() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.reuse",
        &["osu!.exe"],
    );
    let (source, _comm_hints) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    source.appear(test_process(300, "osu!.exe"));
    let first = expect_attached(&events, 300);

    // Same pid, a different executable path: a different OS process
    // occupies pid 300 now, never a genuine update to the first one —
    // the service must report the first occurrence's own exit before the
    // second's appearance, not silently swap the identity underneath the
    // first's still-live session.
    let mut reused = test_process(300, "osu!.exe");
    reused.path = "/games/osu!.exe-reused".to_owned();
    source.appear(reused);

    let detached = expect_detached(&events, first.handle);
    assert_eq!(detached.reason, vosu_plugin_api::DetachReason::Exited);
    let second = expect_attached(&events, 300);
    assert_ne!(second.generation, first.generation);

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hint_set_change_syncs_the_source_and_forces_a_reconcile() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let (source, comm_hints) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    // Registering the plugin only after the service has started proves
    // the service re-reads `PluginHost::comm_hints` on every pass rather
    // than snapshotting it once at startup.
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.hints",
        &["osu!.exe"],
    );
    let applied = comm_hints
        .recv_timeout(RECV_TIMEOUT)
        .expect("discovery did not sync the widened comm-hint set in time");
    assert!(applied.iter().any(|hint| hint == "osu!.exe"));

    // The forced reconcile after a hint-set change must pick up a
    // process already present in the source at that point, not only one
    // appearing afterward.
    source.appear(test_process(400, "osu!.exe"));
    expect_attached(&events, 400);

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_change_recomputes_focus_deterministically() {
    let runtime = Handle::current();
    // Preset to the first session's own pid: the foreground-owner rule
    // then decides focus deterministically at both identifications below,
    // regardless of the Focus rule's own most-recent-fallback/first-ever
    // tie-break (irrelevant here, `focus.rs`'s own module doc comment) —
    // this test only needs `PluginHost::foreground_changed` itself to be
    // the thing that moves focus the second time.
    let foreground = Arc::new(focus::SyntheticForegroundSource::new(Some(500)));
    let (_log_dir, host) = test_host(runtime.clone(), foreground.clone());
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.foreground",
        &["osu!.exe"],
    );
    let (source, _comm_hints) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    source.appear(test_process(500, "osu!.exe"));
    let first = expect_attached(&events, 500);
    let first_focus = match expect_event(
        &events,
        |event| matches!(event, SessionEvent::Focus(FocusChanged { handle, .. }) if *handle == first.handle),
    ) {
        SessionEvent::Focus(focus) => focus,
        _ => unreachable!("expect_event's own predicate only matches Focus"),
    };
    assert!(
        first_focus.focused,
        "the foreground owner's own session must start focused"
    );

    source.appear(test_process(501, "osu!.exe"));
    let second = expect_attached(&events, 501);
    let second_focus = match expect_event(
        &events,
        |event| matches!(event, SessionEvent::Focus(FocusChanged { handle, .. }) if *handle == second.handle),
    ) {
        SessionEvent::Focus(focus) => focus,
        _ => unreachable!("expect_event's own predicate only matches Focus"),
    };
    assert!(
        !second_focus.focused,
        "the non-foreground session must start unfocused"
    );

    // Nothing else changes the session set here: this `FOCUS_CHANGED`
    // pair can only come from `DiscoveryService`-independent
    // `PluginHost::foreground_changed` itself re-running the Focus rule.
    foreground.set(Some(501));
    host.foreground_changed();
    let loss = match expect_event(
        &events,
        |event| matches!(event, SessionEvent::Focus(FocusChanged { handle, .. }) if *handle == first.handle),
    ) {
        SessionEvent::Focus(focus) => focus,
        _ => unreachable!("expect_event's own predicate only matches Focus"),
    };
    assert!(!loss.focused);
    let gain = match expect_event(
        &events,
        |event| matches!(event, SessionEvent::Focus(FocusChanged { handle, .. }) if *handle == second.handle),
    ) {
        SessionEvent::Focus(focus) => focus,
        _ => unreachable!("expect_event's own predicate only matches Focus"),
    };
    assert!(gain.focused);

    service.stop();
}
