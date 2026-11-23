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
        start_time: 1,
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
    prefilter: SyncSender<Prefilter>,
}

impl SyntheticSource {
    fn new() -> (Arc<Self>, Receiver<Prefilter>) {
        let (tx, rx) = sync_channel(16);
        (
            Arc::new(Self {
                state: Mutex::new(SyntheticState::default()),
                notifier: Notify::new(),
                prefilter: tx,
            }),
            rx,
        )
    }

    /// Inserts `record` into the source's own ground truth without
    /// queuing an `inspect` hint or waking the service: only a full
    /// reconcile (`enumerate()`) will ever see it, never the targeted
    /// event path `appear` also exercises. Proves a forced full
    /// reconcile actually re-derives state from `enumerate()`, not just
    /// from whatever was queued (`hint_set_change_syncs_the_source_and_forces_a_reconcile`).
    fn insert(&self, record: ProcessRecord) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .processes
            .insert(record.pid, record);
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

    fn set_prefilter(&self, filter: &Prefilter) -> io::Result<()> {
        // A full channel means a test stopped draining it; losing the
        // update would be the wrong failure mode for a bounded test
        // fixture, so block instead of the usual `try_send` a production
        // sink would use for an unbounded-backpressure-sensitive path.
        self.prefilter.send(filter.clone()).ok();
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
async fn reused_pid_with_a_new_start_time_is_reported_as_exit_then_a_fresh_appearance() {
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
    let (source, _prefilter) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    source.appear(test_process(300, "osu!.exe"));
    let first = expect_attached(&events, 300);

    // Same pid, identical name/path/command_line/bitness: only
    // `start_time` differs, as a fast osu! restart reusing the pid
    // within one reconciliation pass would look (P1 pid reuse) — the
    // service must still report the first occurrence's own exit before
    // the second's appearance, not silently swap the identity underneath
    // the first's still-live session.
    let mut reused = test_process(300, "osu!.exe");
    reused.start_time = 2;
    source.appear(reused);

    let detached = expect_detached(&events, first.handle);
    assert_eq!(detached.reason, vosu_plugin_api::DetachReason::Exited);
    let second = expect_attached(&events, 300);
    assert_ne!(second.generation, first.generation);

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn argv_rewrite_with_the_same_start_time_is_not_churned() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.argv",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    source.appear(test_process(301, "osu!.exe"));
    let first = expect_attached(&events, 301);

    // setproctitle-style argv rewrite: `command_line` changes but the
    // process never exited (same `start_time`) -- `same_occurrence` must
    // not treat this as a reused pid (P1), so no detach/re-attach may
    // follow. A second, distinct process's own attach proves the loop
    // kept running past the rewrite; every event up to it is checked for
    // a forbidden detach instead of silently discarding whatever
    // `expect_attached` alone would not care about.
    let mut rewritten = test_process(301, "osu!.exe");
    rewritten.command_line = "osu! -devserver".to_owned();
    source.appear(rewritten);
    source.appear(test_process(302, "osu!.exe"));

    loop {
        let event = events
            .recv_timeout(RECV_TIMEOUT)
            .expect("the marker process's own attach never arrived");
        match &event {
            SessionEvent::Detached(detached) => assert_ne!(
                detached.handle, first.handle,
                "an argv-only rewrite with the same start_time must not be churned"
            ),
            SessionEvent::Attached(attached) if attached.pid == 302 => break,
            _ => {}
        }
    }

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_place_rename_to_a_matching_name_attaches() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.rename-attach",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    // A loader execs under a non-matching name, then renames itself in
    // place (Wine's `set_process_name` -> `prctl(PR_SET_NAME)` is the
    // motivating case, P2): same pid, same start_time, only name/path
    // change. No plugin matches "loader.exe", so nothing attaches yet.
    source.appear(test_process(600, "loader.exe"));
    source.appear(test_process(600, "osu!.exe"));

    let attached = expect_attached(&events, 600);
    assert_eq!(attached.pid, 600);

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_place_rename_that_still_matches_does_not_churn_the_session() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.rename-keep",
        &["osu!.exe", "osu!stable.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    source.appear(test_process(601, "osu!.exe"));
    let first = expect_attached(&events, 601);

    // Renamed in place to another name the same matcher still accepts:
    // `process_appeared` re-runs the match and calls
    // `SessionRegistry::attach` again, which is a no-op for an
    // already-pending/identified pid (`session.rs::attach`'s own no-op
    // rule) -- no second `SESSION_ATTACHED`, and definitely no
    // `SESSION_DETACHED` (P2: an in-place rename must never send a
    // spurious exit).
    source.appear(test_process(601, "osu!stable.exe"));
    source.appear(test_process(602, "osu!.exe"));

    loop {
        let event = events
            .recv_timeout(RECV_TIMEOUT)
            .expect("the marker process's own attach never arrived");
        match &event {
            SessionEvent::Detached(detached) => assert_ne!(
                detached.handle, first.handle,
                "a rename that still matches must not be churned"
            ),
            SessionEvent::Attached(attached) if attached.pid == 601 => {
                panic!("a rename that still matches must not re-attach the same pid")
            }
            SessionEvent::Attached(attached) if attached.pid == 602 => break,
            _ => {}
        }
    }

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_place_rename_that_stops_matching_keeps_the_session_attached() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.rename-stop",
        &["osu!.exe"],
    );
    let (source, _prefilter) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    source.appear(test_process(603, "osu!.exe"));
    let first = expect_attached(&events, 603);

    // Renamed in place to a name this plugin's matcher no longer accepts
    // (same pid, same start_time): `process_appeared`'s own
    // `match_candidate` now finds no winner and returns before touching
    // `SessionRegistry` at all, so the already-attached session is
    // neither re-attached nor detached. `docs/v2/plugin-abi.md` §6.2's
    // `SESSION_DETACHED` reasons are exactly `"exited"|"lost"|
    // "superseded"|"declined"` -- "no longer matches a matcher" is none
    // of them, so the spec-correct behavior is to leave the session
    // running: once attached, a session is identified by its OS process
    // occurrence (pid + start_time) alone for the rest of its life,
    // exactly as 1.x's own pid-based session lifecycle.
    source.appear(test_process(603, "notes.exe"));
    source.appear(test_process(604, "osu!.exe"));

    loop {
        let event = events
            .recv_timeout(RECV_TIMEOUT)
            .expect("the marker process's own attach never arrived");
        match &event {
            SessionEvent::Detached(detached) => assert_ne!(
                detached.handle, first.handle,
                "a rename that stops matching must not detach the already-attached session (§6.2)"
            ),
            SessionEvent::Attached(attached) if attached.pid == 604 => break,
            _ => {}
        }
    }

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hint_set_change_syncs_the_source_and_reoffers_tracked_processes() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let (source, prefilter) = SyntheticSource::new();
    // Inserted directly into the source's own state, bypassing `appear`'s
    // own `inspect` hint entirely (P3: the earlier version of this test
    // could not tell a forced full reconcile apart from the ordinary
    // targeted-inspect path `appear` also exercises): only a full
    // `enumerate()` can ever see this pid.
    source.insert(test_process(400, "osu!.exe"));
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    // Registering the plugin only after the service has started proves
    // the service re-reads `PluginHost::prefilter` on every pass rather
    // than snapshotting it once at startup.
    // The initial pass (before the plugin is even registered) syncs an
    // empty `Prefilter::Names([])` first; loop past that to the widened
    // set the registration below causes, bounded the same way
    // `expect_event` is.
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.hints",
        &["osu!.exe"],
    );
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "discovery did not sync the widened pre-filter set in time"
        );
        let applied = prefilter
            .recv_timeout(remaining)
            .expect("discovery did not sync the widened pre-filter set in time");
        if applied.allows(crate::plugin::matcher::MatchPlatform::Linux, "osu!.exe") {
            break;
        }
    }

    // The forced reconcile after a pre-filter change must pick up a
    // process already present in the source at that point -- inserted
    // silently above, never through `appear`'s own inspect hint.
    expect_attached(&events, 400);

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matcher_change_forces_a_full_reconcile_of_an_untracked_process() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime.clone(),
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let (source, _prefilter) = SyntheticSource::new();
    let service = DiscoveryService::start(&host, source.clone(), runtime);

    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.reconcile-first",
        &["osu!.exe"],
    );

    // Inserted only now, directly into the source's own state, bypassing
    // `appear`'s own `inspect` hint (unlike
    // `hint_set_change_syncs_the_source_and_reoffers_tracked_processes`'s
    // own pid 400, this one was never present during any earlier pass,
    // so it cannot already sit in `DiscoveryService`'s own `tracked` map
    // -- `reoffer_tracked` (the matcher-generation-change path) can
    // never reach a pid it was never tracking; only `full_reconcile`'s
    // own `enumerate()` can discover it at all).
    source.insert(test_process(401, "osu!.exe"));

    // A second, unrelated plugin's registration bumps the matcher
    // generation (and pre-filter) again, forcing another reconciliation
    // pass (`run_pass`'s `generation_changed || filter_changed` check) --
    // proving that pass's own `full_reconcile` really re-derives ground
    // truth from `source.enumerate()`, picking up pid 401 for the first
    // time, rather than relying on a record already in `tracked`.
    create_session_instance(
        &host,
        PluginInstanceId(1),
        "vosu.testkit.reconcile-second",
        &["other.exe"],
    );

    expect_attached(&events, 401);

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
