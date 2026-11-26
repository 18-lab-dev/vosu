//! M5 E0: the `vosu plugin-host` entry and one instance's package lifecycle
//! through the real `vosu` executable, core's real launcher and a real
//! `PluginHost` (`docs/v2/plugin-abi.md` §7.3, §7.4, §7.6 "Launch",
//! "Startup", "Destroy and shutdown").

#[path = "lifecycle/fixture.rs"]
mod fixture;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fixture::Mode;
use vosu_core::plugin::process::launcher::ProcessHostLauncher;
use vosu_core::plugin::test_support::write_development_package;
use vosu_core::plugin::{self, HostError, LoadedPlugin, PluginHost, PluginInstanceId};
use vosu_plugin_api::{FocusChanged, Message, vosu_status};

const BOUND: Duration = Duration::from_secs(20);
const DESTROY_GRACE: Duration = Duration::from_secs(5);

fn vosu() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_vosu"))
}

struct Console;

impl vosu_core::logging::ConsoleSink for Console {
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

struct EmptySettings;

impl vosu_core::plugin::settings::SettingsStorage for EmptySettings {
    fn load(
        &self,
        _plugin_id: &str,
    ) -> Result<
        serde_json::Map<String, serde_json::Value>,
        vosu_core::plugin::settings::SettingsError,
    > {
        Ok(serde_json::Map::new())
    }
    fn store(
        &self,
        _plugin_id: &str,
        _values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), vosu_core::plugin::settings::SettingsError> {
        Ok(())
    }
}

/// One test's package, markers and host.
struct Scenario {
    root: tempfile::TempDir,
    markers: PathBuf,
    loaded: LoadedPlugin,
    host: Arc<PluginHost>,
    runtime: tokio::runtime::Runtime,
}

impl Scenario {
    /// A `runtime = "process"` package of the fixture in `mode`, with a host
    /// whose launcher starts the real `vosu` executable under `identity`.
    fn process(name: &str, mode: Mode, identity: &str) -> Self {
        Self::process_with(
            name,
            mode,
            ProcessHostLauncher::with_executable(vosu(), identity).unwrap(),
        )
    }

    fn process_with(name: &str, mode: Mode, launcher: ProcessHostLauncher) -> Self {
        let scenario = Self::package(name, mode, "process");
        scenario
            .host
            .install_process_launcher(Arc::new(launcher))
            .unwrap();
        scenario
    }

    fn package(name: &str, mode: Mode, runtime_kind: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let markers = fixture::dir(root.path(), "markers");
        let build = fixture::dir(root.path(), "build");
        let package = fixture::dir(root.path(), "package");
        let library = fixture::build(&build, &markers, mode);
        let plugin_id = format!("test.e0.{name}");
        write_development_package(
            &package,
            &plugin_id,
            &format!("e0-{name}"),
            runtime_kind,
            fixture::library_name(),
            &library,
        )
        .unwrap();
        let loaded = plugin::load_development(
            &vosu::BUNDLE.version(),
            &vosu_core::plugin::ValidationSnapshot::default(),
            &package,
            true,
        )
        .expect("the fixture package validates");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = {
            let _enter = runtime.enter();
            let logger = vosu_core::logging::Logger::new(
                fixture::dir(root.path(), "logs"),
                vosu_core::logging::LoggerOptions {
                    version: "0.1.0".to_owned(),
                    debug_log: false,
                    queue_capacity: 64,
                },
                Arc::new(Console),
            )
            .unwrap();
            let settings = Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
                Arc::new(EmptySettings),
            ));
            PluginHost::new(logger, settings, runtime.handle().clone())
        };
        Self {
            root,
            markers,
            loaded,
            host,
            runtime,
        }
    }

    fn create(&self) -> Result<(), HostError> {
        let _enter = self.runtime.enter();
        self.host.create_instance(
            PluginInstanceId::new(1),
            &self.loaded,
            fixture::dir(self.root.path(), "data"),
            None,
        )
    }

    fn destroy(&self) {
        let _enter = self.runtime.enter();
        self.host.destroy_all(DESTROY_GRACE);
    }

    fn dispatch_focus(&self) -> Result<(), HostError> {
        let _enter = self.runtime.enter();
        self.host
            .dispatch_to_instance(
                PluginInstanceId::new(1),
                &Message::FocusChanged(FocusChanged {
                    handle: 7,
                    focused: true,
                    foreground: None,
                }),
                0,
            )
            .map(drop)
    }

    fn marker(&self, probe: &str) -> Option<(u32, String)> {
        fixture::wait_marker(&self.markers, probe, BOUND)
    }

    fn has_marker(&self, probe: &str) -> bool {
        !fixture::markers(&self.markers, probe).is_empty()
    }
}

/// A process identity that survives PID reuse: the PID plus its start
/// time (Linux `/proc/<pid>/stat` field 22), or its live process handle
/// (Windows).
#[cfg(target_os = "linux")]
struct Identity {
    pid: u32,
    start: String,
}

#[cfg(target_os = "linux")]
impl Identity {
    fn of(pid: u32) -> Self {
        let start = Self::start_time(pid).expect("the plugin host is alive when observed");
        Self { pid, start }
    }

    fn start_time(pid: u32) -> Option<String> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_name = &stat[stat.rfind(')')? + 2..];
        after_name.split_whitespace().nth(19).map(str::to_owned)
    }

    /// Whether this exact process still exists, as an unreaped zombie
    /// included.
    fn alive(&self) -> bool {
        Self::start_time(self.pid).as_deref() == Some(self.start.as_str())
    }

    fn parent(&self) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.pid)).ok()?;
        let after_name = &stat[stat.rfind(')')? + 2..];
        after_name.split_whitespace().nth(1)?.parse().ok()
    }

    fn argv(&self) -> Vec<String> {
        std::fs::read(format!("/proc/{}/cmdline", self.pid))
            .map(|bytes| {
                bytes
                    .split(|byte| *byte == 0)
                    .filter(|part| !part.is_empty())
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(windows)]
struct Identity {
    handle: std::os::windows::io::OwnedHandle,
}

#[cfg(windows)]
impl Identity {
    fn of(pid: u32) -> Self {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        };
        // SAFETY: plain call; a null result is checked before ownership.
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        assert!(!raw.is_null(), "the plugin host is alive when observed");
        // SAFETY: `OpenProcess` returned a new handle this value owns.
        let handle = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) };
        Self { handle }
    }

    fn alive(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        // SAFETY: waits zero milliseconds on a live owned handle.
        unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 0) == WAIT_TIMEOUT }
    }
}

fn wait_gone(identity: &Identity, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while identity.alive() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

fn startup_error(result: Result<(), HostError>) -> String {
    match result {
        Err(HostError::ProcessStartupFailed(failure)) => failure.to_string(),
        other => panic!("expected a startup failure, got {other:?}"),
    }
}

/// Runs the plugin host and expects `READY`, a working channel in both
/// directions, and a controlled destroy that ends the process with
/// `destroy` run.
#[test]
fn lifecycle_starts_dispatches_and_destroys_a_process_plugin() {
    let scenario = Scenario::process("happy", Mode::Normal, vosu::BUNDLE.plugin_host_identity);
    scenario.create().expect("the plugin host reaches READY");

    let (pid, _) = scenario.marker("create").expect("create ran in the child");
    assert_ne!(pid, std::process::id(), "create ran in core");
    let identity = Identity::of(pid);
    #[cfg(target_os = "linux")]
    {
        assert_eq!(identity.parent(), Some(std::process::id()));
        let argv = identity.argv();
        assert_eq!(argv[1], "plugin-host", "{argv:?}");
        assert_eq!(argv[2], vosu::BUNDLE.plugin_host_identity);
        assert_eq!(argv.len(), 5, "{argv:?}");
    }
    let (constructor_pid, _) = scenario.marker("constructor").unwrap();
    assert_eq!(constructor_pid, pid, "only the child mapped the library");

    // Core → child → plugin → `send` → core's handler → STATUS → plugin.
    scenario.dispatch_focus().unwrap();
    let (dispatch_pid, statuses) = scenario.marker("dispatch").expect("dispatch ran");
    assert_eq!(dispatch_pid, pid);
    let statuses = fixture::fields(&statuses);
    let status = |key: &str| {
        statuses
            .iter()
            .find(|(name, _)| name == key)
            .unwrap_or_else(|| panic!("{key} missing from {statuses:?}"))
            .1
            .parse::<u32>()
            .unwrap()
    };
    assert_eq!(
        status("opcode"),
        vosu_plugin_api::vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32
    );
    assert_eq!(status("timer"), vosu_status::VOSU_OK as u32);
    // Only core's session registry can refuse this handle.
    assert_eq!(
        status("decline"),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32
    );
    assert_eq!(status("private"), vosu_status::VOSU_ERR_UNSUPPORTED as u32);
    assert_eq!(
        status("oversize"),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32
    );

    let started = Instant::now();
    scenario.destroy();
    assert!(
        started.elapsed() < DESTROY_GRACE,
        "a clean destroy needs no kill"
    );
    let (destroy_pid, _) = scenario
        .marker("destroy")
        .expect("destroy ran in the child");
    assert_eq!(destroy_pid, pid);
    assert!(
        wait_gone(&identity, Duration::ZERO),
        "destroy returned only after the plugin host was reaped"
    );
}

/// The same fixture under both runtimes returns the same status for every
/// probe `send`: local refusals, core's own refusals and successes alike.
#[test]
fn lifecycle_send_statuses_match_the_in_process_runtime() {
    let mut observed = Vec::new();
    for runtime_kind in ["in-process", "process"] {
        let scenario = Scenario::package(
            &format!("parity-{}", runtime_kind.replace('-', "")),
            Mode::Normal,
            runtime_kind,
        );
        if runtime_kind == "process" {
            let launcher =
                ProcessHostLauncher::with_executable(vosu(), vosu::BUNDLE.plugin_host_identity)
                    .unwrap();
            scenario
                .host
                .install_process_launcher(Arc::new(launcher))
                .unwrap();
        }
        scenario.create().unwrap();
        scenario.dispatch_focus().unwrap();
        let (pid, statuses) = scenario.marker("dispatch").unwrap();
        assert_eq!(
            pid == std::process::id(),
            runtime_kind == "in-process",
            "{runtime_kind} dispatched in the wrong process"
        );
        let mut statuses = fixture::fields(&statuses);
        statuses.retain(|(key, _)| fixture::SENDS.contains(&key.as_str()));
        assert_eq!(statuses.len(), fixture::SENDS.len());
        observed.push(statuses);
        scenario.destroy();
    }
    assert_eq!(observed[0], observed[1], "in-process vs process statuses");
}

#[cfg(target_os = "linux")]
#[test]
fn lifecycle_library_constructor_runs_only_after_the_privilege_drop() {
    let scenario = Scenario::process("drop", Mode::Normal, vosu::BUNDLE.plugin_host_identity);
    scenario.create().unwrap();
    let (pid, status) = scenario.marker("constructor").unwrap();
    assert_ne!(pid, std::process::id(), "core mapped the library");
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .unwrap_or_else(|| panic!("{name} missing from the constructor's status"))
            .trim()
            .to_owned()
    };
    // `Seccomp: 2` is installed only by the child's own §7.3 drop, never by
    // the launcher: the constructor ran after the whole sequence.
    assert_eq!(field("Seccomp"), "2");
    assert_eq!(field("NoNewPrivs"), "1");
    for set in ["CapInh", "CapPrm", "CapEff", "CapAmb"] {
        assert_eq!(field(set), "0000000000000000", "{set}");
    }
    assert_eq!(
        field("Threads"),
        "1",
        "nothing started a thread before load"
    );
    assert!(
        field("Uid").split_whitespace().all(|uid| uid != "0"),
        "{}",
        field("Uid")
    );
    println!("RECORD CapBnd={} at constructor", field("CapBnd"));
    scenario.destroy();
}

/// Every child-detected startup failure reaches core as its §7.6 stage, the
/// instance is never published, and nothing keeps running.
#[test]
fn lifecycle_startup_failures_name_their_stage_and_publish_nothing() {
    for (name, mode, expected, created) in [
        (
            "reject",
            Mode::CreateRejected,
            "stage create (create_rejected)",
            true,
        ),
        ("abi", Mode::WrongAbi, "stage abi (abi_mismatch)", false),
        ("entry", Mode::NoEntry, "stage library (load_failed)", false),
    ] {
        let scenario = Scenario::process(name, mode, vosu::BUNDLE.plugin_host_identity);
        let reason = startup_error(scenario.create());
        assert!(reason.starts_with(expected), "{name}: {reason}");
        assert_eq!(scenario.has_marker("create"), created, "{name}");
        // The child mapped the library (its constructor ran there), core
        // did not.
        let (pid, _) = scenario.marker("constructor").unwrap();
        assert_ne!(pid, std::process::id(), "{name}");
        assert!(matches!(
            scenario.dispatch_focus(),
            Err(HostError::UnknownInstance)
        ));
        #[cfg(target_os = "linux")]
        assert!(
            Identity::start_time(pid).is_none(),
            "{name}: the failed plugin host is still running or unreaped"
        );
    }
}

#[test]
fn lifecycle_a_create_that_hangs_is_killed_at_the_startup_deadline() {
    // The compiled fixture is a never-seen library, whose first start gets
    // the longer first-load deadline (`restart` proves that choice); this
    // case pins both to the regular 5 s to observe the kill itself.
    let deadline = vosu_core::plugin::process::launcher::STARTUP_DEADLINE;
    let scenario = Scenario::process_with(
        "hang",
        Mode::CreateHangs,
        ProcessHostLauncher::with_executable(vosu(), vosu::BUNDLE.plugin_host_identity)
            .unwrap()
            .with_deadlines(deadline, deadline),
    );
    let started = Instant::now();
    let reason = startup_error(scenario.create());
    let elapsed = started.elapsed();
    assert!(reason.starts_with("stage timeout"), "{reason}");
    assert!(
        elapsed >= Duration::from_millis(4_900) && elapsed < BOUND,
        "{elapsed:?}"
    );
    let (pid, _) = scenario.marker("create").unwrap();
    #[cfg(target_os = "linux")]
    assert!(
        Identity::start_time(pid).is_none(),
        "the hung child survived"
    );
    #[cfg(windows)]
    let _ = pid;
}

#[test]
fn lifecycle_a_destroy_that_hangs_is_killed_after_its_grace() {
    let scenario = Scenario::process(
        "stuck",
        Mode::DestroyHangs,
        vosu::BUNDLE.plugin_host_identity,
    );
    scenario.create().unwrap();
    let (pid, _) = scenario.marker("create").unwrap();
    let identity = Identity::of(pid);
    let started = Instant::now();
    scenario.destroy();
    let elapsed = started.elapsed();
    assert!(scenario.marker("destroy").is_some(), "destroy started");
    assert!(
        elapsed >= Duration::from_millis(4_500) && elapsed < BOUND,
        "{elapsed:?}"
    );
    assert!(
        wait_gone(&identity, Duration::ZERO),
        "the stuck plugin host was killed and reaped"
    );
}

/// A child of another executable generation refuses before mapping
/// anything, and core reports the update instead of a crash.
#[test]
fn lifecycle_an_updated_executable_is_refused_as_a_generation_mismatch() {
    let scenario = Scenario::process("update", Mode::Normal, "vosu/0.0.0-previous/plugin-host-1");
    let reason = startup_error(scenario.create());
    assert!(
        reason.starts_with("stage generation (executable_updated)"),
        "{reason}"
    );
    assert!(reason.contains("restart vosu"), "{reason}");
    assert!(
        !scenario.has_marker("constructor"),
        "the library was mapped"
    );
}

/// An exit core did not request is one loss: the instance is retired.
#[test]
fn lifecycle_an_unexpected_exit_retires_the_instance() {
    let scenario = Scenario::process("killed", Mode::Normal, vosu::BUNDLE.plugin_host_identity);
    scenario.create().unwrap();
    let (pid, _) = scenario.marker("create").unwrap();
    let identity = Identity::of(pid);
    kill(pid);
    assert!(wait_gone(&identity, BOUND));
    let deadline = Instant::now() + BOUND;
    while scenario.dispatch_focus().is_ok() {
        assert!(Instant::now() < deadline, "the lost instance is still live");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!scenario.has_marker("destroy"), "a loss never runs destroy");
    scenario.destroy();
}

#[cfg(target_os = "linux")]
fn kill(pid: u32) {
    // SAFETY: signals our own plugin host, which is still unreaped (the
    // marker was written by it and the watcher has not observed an exit).
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
}

#[cfg(windows)]
fn kill(pid: u32) {
    let status = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
}

/// Runs `vosu plugin-host ...` directly with an isolated home, returning its
/// output and whether it created anything there.
fn run_plugin_host(args: &[&OsStr], configure: impl FnOnce(&mut Command)) -> (Output, bool) {
    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(vosu());
    command
        .arg("plugin-host")
        .args(args)
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_DATA_HOME", home.path().join("data"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("XDG_RUNTIME_DIR", home.path().join("runtime"))
        .env("APPDATA", home.path().join("appdata"))
        .env("LOCALAPPDATA", home.path().join("localappdata"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure(&mut command);
    let started = Instant::now();
    let output = command.output().unwrap();
    assert!(started.elapsed() < BOUND, "the refusal was not immediate");
    let touched = std::fs::read_dir(home.path()).unwrap().next().is_some();
    (output, touched)
}

fn assert_refused(output: &Output, touched: bool, code: i32, case: &str) {
    assert_eq!(output.status.code(), Some(code), "{case}: {output:?}");
    assert!(output.stdout.is_empty(), "{case}: {output:?}");
    assert!(output.stderr.is_empty(), "{case}: {output:?}");
    assert!(
        !touched,
        "{case}: the refused plugin host created daemon state"
    );
}

/// Missing, malformed or foreign launch descriptors exit with their stage
/// before mapping anything, and never start the daemon: no listener, lock,
/// log, configuration or data file appears.
#[test]
fn lifecycle_missing_or_invalid_descriptors_start_no_daemon() {
    let identity = OsStr::new(vosu::BUNDLE.plugin_host_identity);
    let our_pid = std::process::id().to_string();
    let our_pid = OsStr::new(&our_pid);
    #[cfg(target_os = "linux")]
    let endpoints = OsStr::new("3,4,5,6");
    #[cfg(windows)]
    let endpoints = OsStr::new("h2p=a4:a8,p2h=ac:b0");
    #[cfg(target_os = "linux")]
    let full: Vec<&OsStr> = vec![identity, our_pid, endpoints];
    #[cfg(windows)]
    let full: Vec<&OsStr> = {
        let _ = our_pid;
        vec![identity, endpoints]
    };

    let cases: Vec<(&str, Vec<&OsStr>, i32)> = vec![
        ("no descriptors", vec![], 70),
        ("identity only", vec![identity], 70),
        (
            "extra argument",
            [full.clone(), vec![endpoints]].concat(),
            70,
        ),
        (
            "foreign generation",
            [
                vec![OsStr::new("vosu/9.9.9/plugin-host-1")],
                full[1..].to_vec(),
            ]
            .concat(),
            71,
        ),
        // Nothing is inherited: the named descriptors are not open.
        ("uninherited endpoints", full.clone(), 73),
    ];
    for (case, args, code) in cases {
        let (output, touched) = run_plugin_host(&args, |_| {});
        assert_refused(&output, touched, code, case);
    }

    #[cfg(target_os = "linux")]
    {
        // A parent other than the named core.
        let (output, touched) = run_plugin_host(&[identity, OsStr::new("1"), endpoints], |_| {});
        assert_refused(&output, touched, 72, "foreign parent");

        // Guessable numbers naming foreign objects: four inherited regular
        // files at the endpoint positions are refused by kind.
        let scratch = tempfile::NamedTempFile::new().unwrap();
        let path = std::ffi::CString::new(scratch.path().to_str().unwrap()).unwrap();
        let (output, touched) = run_plugin_host(&full, |command| {
            use std::os::unix::process::CommandExt;
            // SAFETY: the hook runs in the forked child before `exec` and
            // only calls async-signal-safe `open`/`dup2`/`close` on memory
            // prepared before the fork.
            unsafe {
                command.pre_exec(move || {
                    let fd = libc::open(path.as_ptr(), libc::O_RDWR);
                    if fd < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    for target in 3..=6 {
                        if libc::dup2(fd, target) != target {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    if fd > 6 {
                        libc::close(fd);
                    }
                    Ok(())
                });
            }
        });
        assert_refused(&output, touched, 73, "foreign descriptors");
    }
}
