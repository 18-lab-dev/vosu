//! Linux integration test (M1 phase d2b, unit b): drives
//! [`DiscoveryService`] with a real [`LinuxSource`] against a real OS
//! process — a copy of `/bin/sh` renamed so its `comm` matches a testkit
//! plugin's `comm_hints`/`process_names` — and asserts `SESSION_ATTACHED`/
//! `SESSION_DETACHED` arrive on the plugin's own event channel
//! (`vosu_plugin_testkit::session`), through whichever backend
//! [`LinuxSource::open`] actually resolved (CN_PROC, eBPF, or polling —
//! all unprivileged-safe: an unavailable event backend degrades to
//! polling rather than failing). Every wait is bounded
//! (`RECV_TIMEOUT`); the child is reaped on every exit path, including a
//! failed assertion, through `ChildGuard`'s `Drop`.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use serde_json::{Map, Value};
use tokio::runtime::Handle;
use vosu_plugin_testkit::session::{self, SessionEvent, SessionPluginConfig};

use super::*;
use crate::logging::{ConsoleSink, Logger, LoggerOptions};
use crate::plugin::discovery::DiscoveryService;
use crate::plugin::focus;
use crate::plugin::host::PluginHost;
use crate::plugin::host_services::PluginInstanceId;
use crate::plugin::registry;
use crate::plugin::settings::{PluginSettingsService, SettingsError, SettingsStorage};

/// Covers one full-reconcile tick (`discovery.rs`'s own
/// `DiscoveryCadence`, at most 5s while events are healthy) with
/// margin, since a backend that degraded to polling (or a CN_PROC exit
/// this source's own `tracked` bookkeeping does not catch, module doc
/// comment) only reports a change on the next full reconcile tick.
const RECV_TIMEOUT: Duration = Duration::from_secs(10);

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

fn test_host(runtime: Handle) -> (tempfile::TempDir, Arc<PluginHost>) {
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
        Arc::new(focus::SyntheticForegroundSource::new(None)),
        crate::plugin::IDENTIFICATION_TIMEOUT,
    );
    (log_dir, host)
}

fn create_session_instance(
    host: &Arc<PluginHost>,
    plugin_id: &str,
    comm: &str,
) -> Receiver<SessionEvent> {
    let events = session::register_with_config(plugin_id, SessionPluginConfig::default());
    let manifest = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=['{comm}']\ncomm_hints=['{comm}']\n"
    );
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest,
        settings_schema: None,
        vtable: session::vosu_plugin_vtable,
    }])
    .expect("loading the session testkit plugin");
    let data_dir = tempfile::tempdir().unwrap();
    host.create_instance(
        PluginInstanceId(0),
        &loaded[0],
        data_dir.path().to_path_buf(),
        None,
    )
    .expect("creating the session testkit instance");
    events
}

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

/// Unique per test-process run: several copies of this test binary can run
/// concurrently (`cargo nextest`), so the comm/plugin id must not collide
/// across them, and must fit the kernel's 15-byte `comm` field
/// (`vosu_sys::events::MAX_COMM_HINT_LEN`).
fn unique_comm() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    // "vt" + up to 7 pid digits + '-' + up to 3 counter digits <= 13 bytes.
    format!("vt{}-{n}", std::process::id() % 10_000_000)
}

/// Copies `/bin/sh` to a temp file named `comm`: the kernel derives a
/// freshly-exec'd process's `comm` from the basename of the path passed to
/// `execve`, truncated to 15 bytes, so spawning this copy directly produces
/// a real child process whose `comm` is exactly `comm` — no `PR_SET_NAME`
/// call needed. Not a copy of `sleep`: Ubuntu's uutils coreutils is one
/// multi-call binary that checks its executable name and exits at once
/// under any other name, even with `argv[0]` set to `sleep`. The copy runs
/// `read` on a piped stdin, so it lives until killed.
fn renamed_shell(dir: &std::path::Path, comm: &str) -> std::path::PathBuf {
    let dest = dir.join(comm);
    std::fs::copy("/bin/sh", &dest).expect("copying /bin/sh for a renamed test child");
    let mut permissions = std::fs::metadata(&dest).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&dest, permissions).unwrap();
    dest
}

/// Kills and reaps the wrapped child on every exit path, including a
/// failed assertion unwinding through this scope.
struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_child_process_drives_attach_and_detach() {
    let runtime = Handle::current();
    let (_log_dir, host) = test_host(runtime.clone());

    let comm = unique_comm();
    let bin_dir = tempfile::tempdir().unwrap();
    let binary = renamed_shell(bin_dir.path(), &comm);

    let events = create_session_instance(&host, &format!("vosu.testkit.{comm}"), &comm);

    let mut child = ChildGuard(
        std::process::Command::new(&binary)
            .args(["-c", "read _"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("spawning the renamed shell child"),
    );
    let pid = child.0.id();

    let linux_source = LinuxSource::open();
    let backend = linux_source.backend_name();
    let source: Arc<dyn ProcessSource> = Arc::new(linux_source);
    let service = DiscoveryService::start(&host, source, runtime);

    let attached = match expect_event(
        &events,
        |event| matches!(event, SessionEvent::Attached(attached) if attached.pid == pid),
    ) {
        SessionEvent::Attached(attached) => attached,
        _ => unreachable!("expect_event's own predicate only matches Attached"),
    };
    assert_eq!(
        attached.pid, pid,
        "attached session must be the spawned child"
    );
    eprintln!("real_child_process_drives_attach_and_detach: backend={backend}");

    child.0.kill().expect("killing the test child");
    child.0.wait().expect("reaping the test child");

    match expect_event(
        &events,
        |event| matches!(event, SessionEvent::Detached(detached) if detached.handle == attached.handle),
    ) {
        SessionEvent::Detached(detached) => {
            assert_eq!(detached.handle, attached.handle);
        }
        _ => unreachable!("expect_event's own predicate only matches Detached"),
    }

    service.stop();
}

/// Real desktops routinely run more than the eBPF tracking control's own
/// bounded capacity (`vosu_sys::events::MAX_PENDING_EVENTS`, 128,
/// crate-private); this value is chosen well past it.
const PAST_EBPF_TRACKING_BOUND: u32 = 300;

#[test]
fn unfiltered_prefilter_never_mirrors_the_tracked_set_to_the_kernel() {
    use vosu_sys::events::test_support::{CountingTrackingControl, events_with_tracking_control};

    let tracking_control = Arc::new(CountingTrackingControl::new(128));
    let events = events_with_tracking_control(tracking_control.clone());
    let source = LinuxSource {
        events: Some(events),
        filter: Mutex::new(CommHints::default()),
        tracked: Mutex::new(HashSet::new()),
    };

    source
        .set_prefilter(&Prefilter::Unfiltered)
        .expect("switching to Unfiltered never touches the kernel tracking control");

    // Before the fix, each of these calls -- `Self::enumerate`'s own
    // private helper, also reached by `Self::mark_tracked`/
    // `Self::forget_tracked` -- pushed the whole set to the kernel
    // tracking control, which rejects anything past its bound, marks
    // the backend unhealthy, and latches a reconcile; `DiscoveryService`'s
    // own full reconcile then re-derives this exact set and retries
    // identically, hot-looping full `/proc` scans. Calling it several
    // times here, well past that bound, proves there is no such storm:
    // the kernel tracking control is never touched at all while
    // unfiltered.
    for _ in 0..3 {
        source.replace_all_tracked(0..PAST_EBPF_TRACKING_BOUND);
    }

    assert_eq!(
        tracking_control.calls(),
        0,
        "unfiltered must never mirror the tracked set into the bounded eBPF tracking control"
    );
}

#[test]
fn set_prefilter_updates_the_userspace_filter_before_a_failed_kernel_push() {
    use vosu_sys::events::test_support::{FailingCommFilter, events_with_failing_comm_filter};

    let comm_filter = Arc::new(FailingCommFilter::new());
    let events = events_with_failing_comm_filter(comm_filter.clone());
    let source = LinuxSource {
        events: Some(events),
        filter: Mutex::new(CommHints::default()),
        tracked: Mutex::new(HashSet::new()),
    };

    let filter = Prefilter::Names(vec!["osu!.exe".to_owned()]);
    let result = source.set_prefilter(&filter);
    assert!(
        result.is_err(),
        "the injected comm filter always fails the kernel push"
    );

    // P2 (module doc comment): the userspace filter -- what
    // `Self::enumerate`/`Self::inspect` actually match against -- must
    // reflect the attempted update regardless of the kernel push's own
    // outcome, never silently keep matching the stale set.
    let applied = source.current_filter();
    assert!(applied.matches(b"osu!.exe"));
    assert!(!applied.is_unfiltered());

    // And the kernel push really was attempted with the new set, proving
    // this exercises the ordering, not a no-op.
    assert!(
        comm_filter
            .last_attempted()
            .is_some_and(|hints| hints.matches(b"osu!.exe"))
    );
}

#[test]
fn unfiltered_cn_proc_style_source_still_delivers_a_tracked_exit() {
    use vosu_sys::events::test_support::{events_with_cn_proc_mode, push_exit_event};

    let events = events_with_cn_proc_mode();
    let source = LinuxSource {
        events: Some(events),
        filter: Mutex::new(CommHints::default()),
        tracked: Mutex::new(HashSet::new()),
    };

    source.set_prefilter(&Prefilter::Unfiltered).unwrap();
    // CN_PROC has no kernel-side tracking map to bound (N1), so
    // `sync_tracked` must keep mirroring into the event backend's own
    // userspace queue even while unfiltered -- unlike eBPF, there is
    // nothing here to protect by skipping it.
    source.replace_all_tracked([4242].into_iter());

    push_exit_event(source.events.as_ref().unwrap(), 4242);
    let batch = source.drain_events();

    // Before the N1 fix (an unconditional no-op while unfiltered),
    // `sync_tracked` would never have mirrored pid 4242 into the queue's
    // own `tracked` set, so this exit would hit `EventQueue::push`'s
    // untracked-exit path instead. CN_PROC sets `reconcile_untracked_exit
    // = false` (`LinuxProcessEvents::open`), so that path silently drops
    // the event rather than forcing a reconcile -- the exit would be
    // lost outright until the next full reconcile, up to
    // `DiscoveryCadence`'s own healthy-events interval (5s) later.
    assert_eq!(
        batch.exit,
        vec![4242],
        "a tracked pid's exit must still be delivered precisely while unfiltered on CN_PROC"
    );
    assert!(!batch.needs_reconcile);
}

#[test]
fn returning_to_a_filtered_prefilter_restores_the_kernel_mirror() {
    use vosu_sys::events::test_support::{CountingTrackingControl, events_with_tracking_control};

    let tracking_control = Arc::new(CountingTrackingControl::new(128));
    let events = events_with_tracking_control(tracking_control.clone());
    let source = LinuxSource {
        events: Some(events),
        filter: Mutex::new(CommHints::default()),
        tracked: Mutex::new(HashSet::new()),
    };

    source.set_prefilter(&Prefilter::Unfiltered).unwrap();
    source.replace_all_tracked(0..PAST_EBPF_TRACKING_BOUND);
    assert_eq!(
        tracking_control.calls(),
        0,
        "the kernel tracking control must stay untouched while unfiltered"
    );

    // Narrowing the pre-filter again must resume mirroring to the
    // kernel: a `sync_tracked` that skipped the push unconditionally,
    // regardless of the current filter (rather than only while
    // unfiltered), would leave `calls()` at 0 here too.
    source
        .set_prefilter(&Prefilter::Names(vec!["osu!.exe".to_owned()]))
        .unwrap();
    source.replace_all_tracked([10, 20, 30].into_iter());

    assert_eq!(
        tracking_control.calls(),
        1,
        "returning to a filtered prefilter must restore the kernel mirror"
    );
}
