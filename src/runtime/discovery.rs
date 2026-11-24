//! Production wiring for the generic core discovery service
//! (`docs/v2/implementation-plan.md` M1 §4, phase d2b, unit d):
//! [`start_discovery`] is the one production entry point
//! [`super::lifecycle`] calls after plugins load, and the platform
//! [`ProcessSource`] it picks ([`vosu_core::plugin::LinuxSource`] with the
//! `linux-process-events` feature, [`vosu_core::plugin::WindowsProcessSource`]
//! on Windows, [`PollingLinuxSource`] otherwise — a Linux build without
//! that feature, e.g. a plain local `cargo build`/`cargo run` without
//! `--features`). `vosu_core::plugin::ForegroundPoller` has no wrapper
//! here: it takes no `ProcessSource`, so `lifecycle.rs` starts it
//! directly, under the same [`PluginHost::has_enabled_matchers`] gate
//! [`start_discovery`] applies.
//!
//! Separate from this crate's own 1.x-style discovery
//! (`crate::instance::NativeProcessProvider`, wired in
//! `ProductionRuntime::compose`): until a plugin with a `[match]` table
//! is registered (`BUNDLED` holds none until `vosu-osu` lands), this
//! module's [`start_discovery`] returns `None` and 1.x discovery stays
//! the only active one — zero added cost in production today.

#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
use std::io;
use std::sync::Arc;

use tokio::runtime::Handle;
#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
use vosu_core::plugin::Prefilter;
#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
use vosu_core::plugin::ProcessRecord;
use vosu_core::plugin::{DiscoveryService, PluginHost, ProcessSource};

/// Builds this platform's production [`ProcessSource`]
/// (`docs/v2/implementation-plan.md` M1 §4, phase d2b, unit d): Linux
/// with the `linux-process-events` feature gets the real eBPF/CN_PROC-
/// backed [`vosu_core::plugin::LinuxSource`] (degrading to polling on its
/// own when no event backend is available, that type's own doc comment);
/// Windows gets [`vosu_core::plugin::WindowsProcessSource`]; a Linux
/// build without the feature gets [`PollingLinuxSource`].
#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
fn platform_process_source() -> Arc<dyn ProcessSource> {
    Arc::new(vosu_core::plugin::LinuxSource::open())
}

#[cfg(windows)]
fn platform_process_source() -> Arc<dyn ProcessSource> {
    Arc::new(vosu_core::plugin::WindowsProcessSource::default())
}

#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
fn platform_process_source() -> Arc<dyn ProcessSource> {
    Arc::new(PollingLinuxSource::default())
}

/// Plain-polling [`ProcessSource`] for a Linux build without the
/// `linux-process-events` feature: this crate's own `default` feature set
/// already includes it (`Cargo.toml`), and so does every release/CI
/// command `AGENTS.md` documents (`--all-features`, or the release
/// command's explicit `--features linux-process-events`), so this only
/// ever runs for an explicit `--no-default-features` local build.
///
/// Honors the same unified pre-filter (`vosu_core::plugin::Prefilter`,
/// §6.1, P3) [`vosu_core::plugin::LinuxSource`] does, just applied to a
/// plain `/proc` substring scan instead of the kernel/CN_PROC side —
/// `vosu_sys::events::CommHints` (and its kernel-facing validation) is
/// gated on the same `linux-process-events` feature this module is
/// without, so matching here is a small local substring check instead of
/// reusing that type. `enumerate_processes_matching`'s own `include`
/// predicate already skips the heavier `cmdline`/`exe`/ELF-header reads
/// for a `comm` the pre-filter rejects, so this still gets the narrowing
/// benefit, not just the correctness of matching it at all.
#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
struct PollingLinuxSource {
    prefilter: std::sync::Mutex<Prefilter>,
}

#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
impl Default for PollingLinuxSource {
    fn default() -> Self {
        // Fail open, mirroring `WindowsSource`'s own starting value
        // (`discovery/windows_source.rs`): `DiscoveryService::run_pass`
        // always syncs the real set before the first `enumerate()`, so
        // this default is never actually observed in production.
        Self {
            prefilter: std::sync::Mutex::new(Prefilter::Unfiltered),
        }
    }
}

/// Substring pre-filter check mirroring `vosu_sys::events::CommHints::matches`
/// (§6.1's substring rule), without depending on that feature-gated type.
#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
fn comm_allows(filter: &Prefilter, comm: &std::ffi::OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    match filter {
        Prefilter::Unfiltered => true,
        Prefilter::Names(hints) => {
            let comm = comm.as_bytes();
            hints.iter().any(|hint| {
                let hint = hint.as_bytes();
                !hint.is_empty() && comm.windows(hint.len()).any(|window| window == hint)
            })
        }
    }
}

#[cfg(all(target_os = "linux", not(feature = "linux-process-events")))]
impl ProcessSource for PollingLinuxSource {
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
        let filter = self
            .prefilter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        vosu_sys::process::enumerate_processes_matching(|comm| comm_allows(&filter, comm))
            .map(|entries| {
                entries
                    .into_iter()
                    .map(vosu_core::plugin::linux_common::map_metadata)
                    .collect()
            })
            .map_err(io::Error::other)
    }

    fn set_prefilter(&self, filter: &Prefilter) -> io::Result<()> {
        *self
            .prefilter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = filter.clone();
        Ok(())
    }
}

/// Starts production discovery (M1 §4, phase d2b, unit d) iff
/// `plugin_host.has_enabled_matchers()` — zero cost otherwise: no
/// background thread is ever spawned, matching
/// [`DiscoveryService::start`]'s own "the caller decides whether to call
/// it at all" contract (that method's doc comment). `runtime` drives the
/// returned service's one asynchronous wait, the same role it plays for
/// [`DiscoveryService::start`] directly.
#[must_use]
pub(super) fn start_discovery(
    plugin_host: &Arc<PluginHost>,
    runtime: Handle,
) -> Option<DiscoveryService> {
    if !plugin_host.has_enabled_matchers() {
        return None;
    }
    let source = platform_process_source();
    Some(DiscoveryService::start(plugin_host, source, runtime))
}

#[cfg(test)]
mod tests {
    use super::start_discovery;
    use std::sync::Arc;

    use serde_json::{Map, Value};
    use tokio::runtime::Handle;
    use vosu_core::logging::{ConsoleSink, Logger, LoggerOptions};
    use vosu_core::plugin::PluginHost;
    use vosu_core::plugin::settings::{PluginSettingsService, SettingsError, SettingsStorage};

    struct DummyStorage;
    impl SettingsStorage for DummyStorage {
        fn load(&self, _plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
            Ok(Map::new())
        }
        fn store(
            &self,
            _plugin_id: &str,
            _values: &Map<String, Value>,
        ) -> Result<(), SettingsError> {
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
        let host = PluginHost::new(logger, settings, runtime);
        (log_dir, host)
    }

    /// The zero-cost case (M1 §4 phase d2b unit d's own "production
    /// `BUNDLED` holds no matching plugin until `vosu-osu` lands"): no
    /// plugin ever registered a `[match]` table, so `start_discovery`
    /// must return `None` — no background thread spawned at all, exactly
    /// production's own state today.
    #[test]
    fn no_enabled_matchers_starts_no_discovery_thread() {
        let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
        let _enter = runtime.enter();
        let (_log_dir, host) = test_host(runtime.handle().clone());
        assert!(!host.has_enabled_matchers());
        assert!(
            start_discovery(&host, runtime.handle().clone()).is_none(),
            "no plugin registered a [match] table, so discovery must not start"
        );
    }

    /// Linux integration test (test-harness): a real testkit session
    /// plugin with a `[match]` table against a real spawned child
    /// process, driven through this unit's own `start_discovery` (not a
    /// hand-built `DiscoveryService`/`ProcessSource` pair, unlike
    /// `vosu-core`'s own discovery/`linux_source` unit tests — this test
    /// exists specifically to prove the *wiring*, already covered
    /// in isolation there) — proving discovery actually runs, and that
    /// stopping it before `PluginHost::destroy_all` (this wiring's own
    /// documented shutdown order, `lifecycle.rs`) does not hang or panic.
    #[cfg(all(feature = "test-harness", target_os = "linux"))]
    mod real_child_process {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::mpsc::Receiver;
        use std::time::Duration;

        use vosu_core::plugin::{BundledPlugin, PluginInstanceId, load_bundled};
        use vosu_plugin_testkit::session::{self, SessionEvent, SessionPluginConfig};

        use super::{start_discovery, test_host};

        const RECV_TIMEOUT: Duration = Duration::from_secs(10);

        fn unique_comm() -> String {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            // "rt" + up to 7 pid digits + '-' + up to 3 counter digits <= 13 bytes.
            format!("rt{}-{n}", std::process::id() % 10_000_000)
        }

        /// Copies `/bin/sh` to `comm`, so the spawned child's `comm` is
        /// `comm`. Not a copy of `sleep`: Ubuntu's uutils coreutils is one
        /// multi-call binary that checks its executable name and exits at
        /// once under any other name. The copy runs `read` on a piped stdin,
        /// so it lives until killed.
        fn renamed_shell(dir: &std::path::Path, comm: &str) -> std::path::PathBuf {
            let dest = dir.join(comm);
            std::fs::copy("/bin/sh", &dest).expect("copying /bin/sh for a renamed test child");
            let mut permissions = std::fs::metadata(&dest).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&dest, permissions).unwrap();
            dest
        }

        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
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

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn discovery_runs_and_is_stopped_before_destroy_all() {
            let runtime = tokio::runtime::Handle::current();
            let (_log_dir, host) = test_host(runtime.clone());

            let comm = unique_comm();
            let events = session::register_with_config(
                format!("vosu.testkit.{comm}"),
                SessionPluginConfig::default(),
            );
            let manifest = format!(
                "kind='plugin'\nmanifest_version=1\nid='vosu.testkit.{comm}'\nname='vosu.testkit.{comm}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=['{comm}']\ncomm_hints=['{comm}']\n"
            );
            let loaded = load_bundled(&[BundledPlugin {
                manifest: &manifest,
                vtable: session::vosu_plugin_vtable,
            }])
            .expect("loading the session testkit plugin");
            let data_dir = tempfile::tempdir().unwrap();
            host.create_instance(
                PluginInstanceId::new(0),
                &loaded[0],
                data_dir.path().to_path_buf(),
                None,
            )
            .expect("creating the session testkit instance");

            let bin_dir = tempfile::tempdir().unwrap();
            let binary = renamed_shell(bin_dir.path(), &comm);
            let mut child = ChildGuard(
                std::process::Command::new(&binary)
                    .args(["-c", "read _"])
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .expect("spawning the renamed shell child"),
            );
            let pid = child.0.id();

            assert!(host.has_enabled_matchers());
            let discovery = start_discovery(&host, runtime)
                .expect("a registered [match] table must start discovery");

            let attached = match expect_event(
                &events,
                |event| matches!(event, SessionEvent::Attached(attached) if attached.pid == pid),
            ) {
                SessionEvent::Attached(attached) => attached,
                _ => unreachable!("expect_event's own predicate only matches Attached"),
            };
            assert_eq!(
                attached.pid, pid,
                "the production start_discovery wiring must attach the spawned child"
            );

            child.0.kill().expect("killing the test child");
            child.0.wait().expect("reaping the test child");
            drop(child);

            // Shutdown order this wiring documents (`lifecycle.rs`):
            // discovery stops first, then plugins are destroyed. Run it
            // here and prove neither step hangs or panics.
            discovery.stop();
            host.destroy_all(Duration::from_secs(5));
        }

        /// P1 end-to-end: a manifest whose `[match]` table has only
        /// `process_names` (the §8 manifest example's own shape) and an
        /// empty `comm_hints` must still attach a real spawned child
        /// through the production Linux pre-filter wiring
        /// (`MatcherEngine::prefilter`/`ProcessSource::set_prefilter`,
        /// which production's CN_PROC/kernel path consults before any
        /// userspace match ever runs) -- not just the synthetic-source
        /// unit coverage in `vosu-core`'s own `discovery`/`linux_source`
        /// tests. Before the P1 fix an empty `comm_hints` left the
        /// pre-filter matching nothing, so this process would never have
        /// been read at all.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn discovery_attaches_via_process_names_without_comm_hints() {
            let runtime = tokio::runtime::Handle::current();
            let (_log_dir, host) = test_host(runtime.clone());

            let comm = unique_comm();
            let events = session::register_with_config(
                format!("vosu.testkit.{comm}"),
                SessionPluginConfig::default(),
            );
            let manifest = format!(
                "kind='plugin'\nmanifest_version=1\nid='vosu.testkit.{comm}'\nname='vosu.testkit.{comm}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=['{comm}']\ncomm_hints=[]\n"
            );
            let loaded = load_bundled(&[BundledPlugin {
                manifest: &manifest,
                vtable: session::vosu_plugin_vtable,
            }])
            .expect("loading the session testkit plugin");
            let data_dir = tempfile::tempdir().unwrap();
            host.create_instance(
                PluginInstanceId::new(0),
                &loaded[0],
                data_dir.path().to_path_buf(),
                None,
            )
            .expect("creating the session testkit instance");

            let bin_dir = tempfile::tempdir().unwrap();
            let binary = renamed_shell(bin_dir.path(), &comm);
            let mut child = ChildGuard(
                std::process::Command::new(&binary)
                    .args(["-c", "read _"])
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .expect("spawning the renamed shell child"),
            );
            let pid = child.0.id();

            assert!(host.has_enabled_matchers());
            let discovery = start_discovery(&host, runtime)
                .expect("a registered [match] table must start discovery");

            let attached = match expect_event(
                &events,
                |event| matches!(event, SessionEvent::Attached(attached) if attached.pid == pid),
            ) {
                SessionEvent::Attached(attached) => attached,
                _ => unreachable!("expect_event's own predicate only matches Attached"),
            };
            assert_eq!(
                attached.pid, pid,
                "process_names alone (no comm_hints) must still widen the pre-filter to this child"
            );

            child.0.kill().expect("killing the test child");
            child.0.wait().expect("reaping the test child");
            drop(child);

            discovery.stop();
            host.destroy_all(Duration::from_secs(5));
        }
    }

    /// Windows integration test (test-harness): mirrors the Linux
    /// `real_child_process` module above, but against the real
    /// `WindowsProcessSource` (`platform_process_source`, `#[cfg(windows)]`)
    /// instead of a synthetic `ProcessSource`. Proves the checklist items
    /// `vosu-core`'s own synthetic `windows_source` unit tests cannot: a
    /// real spawned, uniquely renamed child is attached through
    /// `start_discovery`'s production wiring, detaches when it exits, and
    /// a restart under a brand-new pid produces a fresh attach rather than
    /// being folded into the old session.
    #[cfg(all(feature = "test-harness", windows))]
    mod real_child_process {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::mpsc::Receiver;
        use std::time::Duration;

        use vosu_core::plugin::{BundledPlugin, PluginInstanceId, load_bundled};
        use vosu_plugin_testkit::session::{self, SessionEvent, SessionPluginConfig};

        use super::{start_discovery, test_host};

        const RECV_TIMEOUT: Duration = Duration::from_secs(20);

        fn unique_name() -> String {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            format!("rt{}-{n}.exe", std::process::id() % 1_000_000)
        }

        fn cmd_source() -> std::path::PathBuf {
            std::env::var_os("ComSpec")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows\System32\cmd.exe"))
        }

        /// Copies `cmd.exe` to `name` under `dir` and spawns it running a
        /// ~60s `ping`, so the real process list carries this test's own
        /// chosen image name instead of a shared `cmd.exe`.
        fn spawn_renamed(dir: &std::path::Path, name: &str) -> std::process::Child {
            let dest = dir.join(name);
            std::fs::copy(cmd_source(), &dest).expect("copying cmd.exe for a renamed test child");
            std::process::Command::new(&dest)
                .args(["/C", "ping", "-n", "60", "127.0.0.1"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawning the renamed test child")
        }

        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
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

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn discovery_attaches_detaches_and_reattaches_a_restarted_child() {
            let runtime = tokio::runtime::Handle::current();
            let (_log_dir, host) = test_host(runtime.clone());

            let name = unique_name();
            let events = session::register_with_config(
                format!("vosu.testkit.{name}"),
                SessionPluginConfig::default(),
            );
            let manifest = format!(
                "kind='plugin'\nmanifest_version=1\nid='vosu.testkit.{name}'\nname='vosu.testkit.{name}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=['{name}']\n"
            );
            let loaded = load_bundled(&[BundledPlugin {
                manifest: &manifest,
                vtable: session::vosu_plugin_vtable,
            }])
            .expect("loading the session testkit plugin");
            let data_dir = tempfile::tempdir().unwrap();
            host.create_instance(
                PluginInstanceId::new(0),
                &loaded[0],
                data_dir.path().to_path_buf(),
                None,
            )
            .expect("creating the session testkit instance");

            let bin_dir = tempfile::tempdir().unwrap();
            let mut child = ChildGuard(spawn_renamed(bin_dir.path(), &name));
            let pid = child.0.id();

            assert!(host.has_enabled_matchers());
            let discovery = start_discovery(&host, runtime.clone())
                .expect("a registered [match] table must start discovery");

            let attached = match expect_event(
                &events,
                |event| matches!(event, SessionEvent::Attached(attached) if attached.pid == pid),
            ) {
                SessionEvent::Attached(attached) => attached,
                _ => unreachable!("expect_event's own predicate only matches Attached"),
            };
            assert_eq!(
                attached.pid, pid,
                "the real WindowsProcessSource must attach the spawned child"
            );

            child.0.kill().expect("killing the first test child");
            child.0.wait().expect("reaping the first test child");

            expect_event(
                &events,
                |event| matches!(event, SessionEvent::Detached(detached) if detached.handle == attached.handle),
            );

            // Restart under a new pid: respawn the same renamed binary.
            let mut second = ChildGuard(spawn_renamed(bin_dir.path(), &name));
            let second_pid = second.0.id();
            assert_ne!(
                second_pid, pid,
                "the OS must hand out a different pid for the restart"
            );

            let reattached = match expect_event(
                &events,
                |event| matches!(event, SessionEvent::Attached(attached) if attached.pid == second_pid),
            ) {
                SessionEvent::Attached(attached) => attached,
                _ => unreachable!("expect_event's own predicate only matches Attached"),
            };
            assert_eq!(
                reattached.pid, second_pid,
                "the restarted child under a new pid must be freshly attached"
            );

            second.0.kill().expect("killing the second test child");
            second.0.wait().expect("reaping the second test child");
            drop(second);
            drop(child);

            discovery.stop();
            host.destroy_all(Duration::from_secs(5));
        }
    }
}
