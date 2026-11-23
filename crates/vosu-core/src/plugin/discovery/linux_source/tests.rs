//! Linux integration test (M1 phase d2b, unit b): drives
//! [`DiscoveryService`] with a real [`LinuxSource`] against a real OS
//! process — a copy of `sleep` renamed so its `comm` matches a testkit
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

/// Covers one `RECONCILE_INTERVAL` (`discovery.rs`, 2s) with margin, since
/// a backend that degraded to polling (or a CN_PROC exit this source's
/// own `tracked` bookkeeping does not catch, module doc comment) only
/// reports a change on the next full reconcile tick.
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

/// Copies `/bin/sleep` (falling back to `/usr/bin/sleep`) to a temp file
/// named `comm`: the kernel derives a freshly-exec'd process's `comm` from
/// the basename of the path passed to `execve`, truncated to 15 bytes, so
/// spawning this copy directly produces a real child process whose `comm`
/// is exactly `comm` — no `PR_SET_NAME` call needed.
fn renamed_sleep(dir: &std::path::Path, comm: &str) -> std::path::PathBuf {
    let source = ["/bin/sleep", "/usr/bin/sleep"]
        .into_iter()
        .find(|candidate| std::path::Path::new(candidate).exists())
        .expect("sleep(1) must exist on this Linux for the test");
    let dest = dir.join(comm);
    std::fs::copy(source, &dest).expect("copying sleep for a renamed test child");
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
    let binary = renamed_sleep(bin_dir.path(), &comm);

    let events = create_session_instance(&host, &format!("vosu.testkit.{comm}"), &comm);

    let mut child = ChildGuard(
        std::process::Command::new(&binary)
            .arg("60")
            .spawn()
            .expect("spawning the renamed sleep child"),
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
