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
use crate::identity::PluginInstanceId;
use crate::logging::{ConsoleSink, Logger, LoggerOptions};
use crate::plugin::focus;
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
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=[{names}]\ncomm_hints=[{names}]\n"
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
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
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
        argv: None,
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
    /// Consumed by the next [`ProcessSource::enumerate`] call (F2:
    /// `failed_full_scan_retries_early_and_counts_only_success`).
    fail_next_enumerate: bool,
    /// Consumed by the next [`ProcessSource::inspect`] call for this pid
    /// (F2: `failed_targeted_inspection_is_not_counted`).
    fail_inspect: HashSet<u32>,
    /// Backs [`ProcessSource::events_healthy`] (F2 review P2:
    /// `cadence_follows_tracked_set_and_event_health`); the default,
    /// `false`, matches the trait default for a source that never calls
    /// [`SyntheticSource::set_events_healthy`].
    events_healthy: bool,
}

/// An in-memory [`ProcessSource`] a test drives directly: every mutation
/// queues the matching event hint and wakes [`DiscoveryService`]'s
/// background thread immediately, so tests never wait out
/// [`DiscoveryCadence`].
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
    /// event path `appear` also exercises -- used by tests that need to
    /// tell a full reconcile's own `enumerate()` apart from
    /// `reoffer_tracked`'s re-send of an already-tracked record
    /// (`matcher_change_forces_a_full_reconcile_of_an_untracked_process`,
    /// `hint_set_change_syncs_the_source_and_reoffers_tracked_processes`).
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

    /// Queues an `exit` hint without removing `pid` from the source's own
    /// ground truth (F2 exit confirmation: simulates a push event racing
    /// a process that is dying but has not yet actually exited --
    /// `ProcessSource::is_alive`'s default fallback below still answers
    /// `true` for it, since it is still present in `processes`, until
    /// [`Self::declare_dead`] says otherwise).
    fn report_exit_hint(&self, pid: u32) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.pending_exit.push(pid);
        drop(state);
        self.notifier.notify_one();
    }

    /// Removes `pid` from the source's own ground truth without queuing
    /// any event (F2 exit confirmation: the backoff recheck, not a fresh
    /// event, must be what discovers this -- `ProcessSource::is_alive`
    /// now answers `false` for it).
    fn declare_dead(&self, pid: u32) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .processes
            .remove(&pid);
    }

    /// Queues an `inspect` hint without touching the source's own ground
    /// truth (F2: `cadence::inspect_hint_is_targeted_and_a_loss_flag_reconciles_once_keeping_healthy_cadence`
    /// -- a pure re-check hint, not an `appear`/record change).
    fn request_inspect(&self, pid: u32) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.pending_inspect.push(pid);
        drop(state);
        self.notifier.notify_one();
    }

    /// Marks the source as having lost events, forcing a full reconcile
    /// on the next [`ProcessSource::drain_events`] (F2:
    /// `cadence::inspect_hint_is_targeted_and_a_loss_flag_reconciles_once_keeping_healthy_cadence`).
    fn request_reconcile(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.needs_reconcile = true;
        drop(state);
        self.notifier.notify_one();
    }

    /// Fails the next [`ProcessSource::enumerate`] call once (F2).
    fn fail_next_enumerate(&self) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .fail_next_enumerate = true;
    }

    /// Fails the next [`ProcessSource::inspect`] call for `pid` once
    /// (F2).
    fn fail_next_inspect(&self, pid: u32) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .fail_inspect
            .insert(pid);
    }

    /// Sets what [`ProcessSource::events_healthy`] answers from then on
    /// (F2 review P2: `cadence_follows_tracked_set_and_event_health`).
    fn set_events_healthy(&self, healthy: bool) {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .events_healthy = healthy;
    }
}

impl ProcessSource for SyntheticSource {
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if std::mem::take(&mut state.fail_next_enumerate) {
            return Err(io::Error::other("synthetic enumerate failure"));
        }
        Ok(state.processes.values().cloned().collect())
    }

    fn inspect(&self, pid: u32) -> io::Result<Option<ProcessRecord>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if state.fail_inspect.remove(&pid) {
            return Err(io::Error::other("synthetic inspect failure"));
        }
        Ok(state.processes.get(&pid).cloned())
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

    /// Answers from the source's own ground truth (F2 exit confirmation):
    /// alive iff still present in `processes` -- `report_exit_hint`
    /// leaves it present (still alive), `declare_dead`/`exit` remove it
    /// (no longer alive), matching the real
    /// `vosu_sys::process::is_process_alive` default's own semantics
    /// without touching the real OS.
    fn is_alive(&self, pid: u32) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .processes
            .contains_key(&pid)
    }

    /// Answers [`Self::set_events_healthy`]'s last value (F2 review P2:
    /// `cadence_follows_tracked_set_and_event_health`); the trait
    /// default (`false`) until a test calls it.
    fn events_healthy(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .events_healthy
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
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.rename-attach",
        &["osu!.exe"],
    );

    // Drives `apply_appeared` directly, synchronously, instead of through
    // `DiscoveryService`'s background thread plus `SyntheticSource::appear`
    // (N2): `SyntheticSource::inspect` always reads the *current* state,
    // so two `appear` calls issued back to back almost always collapse
    // into one drained observation of the already-renamed record before
    // any pass runs, exercising the pre-existing first-sighting (`None`)
    // branch instead of the in-place-rename branch this test means to
    // cover -- and the test would still pass either way, which is not a
    // reliable regression check. Calling `apply_appeared` twice against
    // one `tracked` map in hand makes the loader sighting and the rename
    // land as two distinct calls deterministically, exercising exactly
    // `apply_appeared`'s `Some(previous) if same_occurrence(..)` branch
    // with a changed name on the second call.
    let mut tracked = HashMap::new();
    let mut pending_exits = HashMap::new();

    // A loader execs under a non-matching name, then renames itself in
    // place (Wine's `set_process_name` -> `prctl(PR_SET_NAME)` is the
    // motivating case, P2): same pid, same start_time, only name/path
    // change. No plugin matches "loader.exe", so nothing attaches yet.
    apply_appeared(
        &host,
        &mut tracked,
        &mut pending_exits,
        test_process(600, "loader.exe"),
    );
    apply_appeared(
        &host,
        &mut tracked,
        &mut pending_exits,
        test_process(600, "osu!.exe"),
    );

    let attached = expect_attached(&events, 600);
    assert_eq!(attached.pid, 600);
}

/// Behaviour pin (N2): on *both* the pre-fix and the fixed `apply_appeared`,
/// a same-`start_time` rename that still matches never churns the
/// session -- the pre-fix code skipped every repeat same-`start_time`
/// sighting outright, so this alone cannot distinguish the two. It
/// documents the intended steady-state behaviour rather than catching a
/// regression in the rename branch itself (`in_place_rename_to_a_matching_name_attaches`
/// covers that).
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

/// Behaviour pin (N2): a session, once attached, has no detach reason for
/// "no longer matches" (`docs/v2/plugin-abi.md` SS6.2), so it stays
/// attached through a rename that stops matching. The pre-fix code also
/// left it attached (it skipped every repeat same-`start_time` sighting
/// outright, rename or not), so this documents the spec-correct steady
/// state rather than catching a regression in the rename branch itself
/// (`in_place_rename_to_a_matching_name_attaches` covers that).
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

    // `sync_generation`'s own re-offer of this already-tracked pid (not a
    // forced full reconcile's `enumerate()`, which
    // `matcher_change_forces_a_full_reconcile_of_an_untracked_process`
    // covers) must pick up a process already present in the source at
    // that point -- inserted silently above, never through `appear`'s
    // own inspect hint.
    expect_attached(&events, 400);

    service.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matcher_change_forces_a_full_reconcile_of_an_untracked_process() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let (source, _prefilter) = SyntheticSource::new();

    let events = create_session_instance(
        &host,
        PluginInstanceId(0),
        "vosu.testkit.reconcile-first",
        &["osu!.exe"],
    );

    // Drives `run_pass` directly instead of `DiscoveryService`'s
    // background thread (N3): nothing wakes that thread when a plugin
    // registers (`SyntheticSource::insert` never notifies, and `wake` is
    // only notified by `stop`), so the pass that would actually discover
    // pid 401 through the real service is the periodic `DiscoveryCadence`
    // (1s active/5s healthy) tick, not the pre-filter/generation change --
    // `RECV_TIMEOUT` (5s) is longer than the fast tick, so the test
    // would still pass with `filter_changed || generation_changed`
    // deleted from `run_pass` entirely. Pinning `next_reconcile` far in
    // the future removes that tick as a possible cause: only the
    // forced-reconcile condition can make the second `run_pass` call
    // below find pid 401.
    let mut state = DiscoveryState::default();
    let cadence = DiscoveryCadence::default();
    let counters = DiscoveryCounters::default();
    let far_future = Instant::now() + Duration::from_secs(3600);

    // First pass: syncs the pre-filter/generation to the plugin just
    // registered above; nothing in the source yet.
    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        Instant::now(),
        far_future,
    );

    // Inserted only now, directly into the source's own state, bypassing
    // `appear`'s own `inspect` hint: never present during the pass above,
    // so it cannot already sit in `tracked` -- `reoffer_tracked` (the
    // matcher-generation-change path) can never reach a pid it was never
    // tracking; only `full_reconcile`'s own `enumerate()` can discover it
    // at all.
    source.insert(test_process(401, "osu!.exe"));

    // A second, unrelated plugin's registration bumps the matcher
    // generation (and pre-filter) again.
    create_session_instance(
        &host,
        PluginInstanceId(1),
        "vosu.testkit.reconcile-second",
        &["other.exe"],
    );

    run_pass(
        &host,
        source.as_ref(),
        &mut state,
        &cadence,
        &counters,
        Instant::now(),
        far_future,
    );

    expect_attached(&events, 401);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sync_prefilter_advances_last_filter_after_a_failure() {
    struct FailingPrefilterSource {
        calls: std::sync::atomic::AtomicU64,
    }
    impl ProcessSource for FailingPrefilterSource {
        fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
            Ok(Vec::new())
        }
        fn set_prefilter(&self, _filter: &Prefilter) -> io::Result<()> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Err(io::Error::other("set_prefilter always fails"))
        }
    }

    let runtime = Handle::current();
    let (_log_dir, host) = test_host(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
    );
    let source = FailingPrefilterSource {
        calls: std::sync::atomic::AtomicU64::new(0),
    };
    let mut last_filter: Option<Prefilter> = None;

    // The first pass always has a new filter to apply (`None` -> the
    // host's current, empty `Prefilter::Names([])`), so `set_prefilter`
    // is attempted and fails; the caller is told no reconcile was forced
    // (`sync_prefilter`'s own return value is whether the update
    // actually applied), but `last_filter` must still advance to the
    // attempted value regardless (P2).
    let forced = sync_prefilter(&host, &source, &mut last_filter);
    assert!(!forced, "a failed update must not force a reconcile");
    assert_eq!(last_filter, Some(host.prefilter()));
    assert_eq!(source.calls.load(std::sync::atomic::Ordering::Relaxed), 1);

    // A second call against the identical (still-failing) filter must
    // not retry `set_prefilter` at all: proof `last_filter` genuinely
    // holds the attempted value instead of being left at `None` to
    // retry -- and hot-loop -- every pass (`sync_prefilter`'s own doc
    // comment).
    let forced_again = sync_prefilter(&host, &source, &mut last_filter);
    assert!(!forced_again);
    assert_eq!(source.calls.load(std::sync::atomic::Ordering::Relaxed), 1);
}

/// F2 cadence/exit-confirmation/stats tests (parity rows 13-17, `m1-remainder-plan.md`
/// §7): split out to keep this file under the 1,000-line limit.
mod cadence;
