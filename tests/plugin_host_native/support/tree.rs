//! Core-only death trees (M5 N0, `docs/v2/plugin-abi.md` §7.3–§7.4): a
//! core process, the real `vosu plugin-host` it started with the testkit
//! loaded, optionally a descendant that plugin started, and an outside
//! control process. Tests kill only the core and read every other process's
//! fate from its own pidfd or handle ([`super::tracked`]).
//!
//! Two kinds of core share every assertion:
//!
//! - [`CoreKind::Helper`]: this test binary re-executed as a *core helper
//!   process running production `PluginHost`/`ProcessHostLauncher`* code
//!   ([`super::host::Host`]), which starts the real `vosu` executable as
//!   the plugin host. It is a real core for the kill semantics, not the
//!   daemon; its outside control is its own ordinary child.
//! - [`CoreKind::Daemon`]: the real `vosu` daemon loading the package
//!   through an approved `--plugin` ([`super::daemon`]); the test starts
//!   the outside control itself.
//!
//! Helper roles run through [`HELPER_TEST`], which does nothing in an
//! ordinary run: `core`, `descendant` (started by the plugin through the
//! control's `spawn`) and `sleeper` (the outside control). Each role writes
//! its identity to a marker file and then waits to be killed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::artifacts::{Artifact, Staged};
use super::daemon::Daemon;
use super::fixture::Fixture;
use super::host::Host;
use super::tracked::{Tracked, image_of, own_start, parent_of, same_file};

/// The test whose process plays every helper role.
pub const HELPER_TEST: &str = "parent_death::helper_process_entry";
const ROLE: &str = "vosu-n0-role";
/// A first start pays the 30-second first-load deadline at most.
pub const STARTUP_BOUND: Duration = Duration::from_secs(45);
/// How long a process killed by the OS may take to be observed gone.
pub const DEATH_BOUND: Duration = Duration::from_secs(10);
/// How long the outside control must outlive the core's death.
pub const SURVIVAL_WINDOW: Duration = Duration::from_secs(1);
/// How long the core helper waits after `create` before recording its
/// status: past the first 1 s restart delay, so a retry would show.
const STATUS_AFTER: Duration = Duration::from_millis(2500);

/// Which process plays core.
#[derive(Debug, Clone)]
pub enum CoreKind {
    Helper,
    Daemon(PathBuf),
}

/// What the testkit does in this tree.
#[derive(Debug, Clone, Copy)]
pub struct Scenario {
    /// Distinguishes plugin ids, process names and directories.
    pub name: &'static str,
    /// A testkit fault name.
    pub fault: &'static str,
    /// Whether the plugin starts a descendant in `create`.
    pub descendant: bool,
    /// Linux only: start core under a seccomp filter that refuses
    /// `seccomp(2)` (the §7.3 `security_unavailable` control).
    pub refuse_seccomp: bool,
}

impl Scenario {
    pub fn plugin_id(&self) -> String {
        format!("test.n0.{}", self.name.replace('_', "-"))
    }
}

/// The core process.
pub struct Core {
    pub kind: CoreKind,
    child: Child,
    pub process: Tracked,
    pub daemon: Option<Daemon>,
    /// The seccomp refusal control's listener (`Scenario::refuse_seccomp`).
    #[cfg(target_os = "linux")]
    pub refusals: Option<super::refusal::Refusals>,
}

/// One tree. Dropping it kills every process it still tracks.
pub struct Tree {
    pub scenario: Scenario,
    pub core: Core,
    pub control: Tracked,
    pub fixture: Fixture,
    _staged: Staged,
    markers: PathBuf,
    plugin_host: Option<Tracked>,
    descendant: Option<Tracked>,
    _control_child: Option<Child>,
    // Last: the directories outlive every process above.
    _root: tempfile::TempDir,
}

/// The arguments that make this test binary play `role`.
fn helper_command(role: &[&str]) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            HELPER_TEST,
            "--nocapture",
            "--test-threads=1",
            ROLE,
        ])
        .args(role);
    command
}

impl Tree {
    /// Stages the testkit for `scenario`, starts core of `kind`, and waits
    /// for the outside control. The plugin host may still be starting.
    pub fn launch(kind: CoreKind, scenario: Scenario) -> Self {
        Self::launch_with_manifest(kind, scenario, &[])
    }

    pub fn launch_with_manifest(
        kind: CoreKind,
        scenario: Scenario,
        substitutions: &[(&str, &str)],
    ) -> Self {
        let root = super::scratch();
        let markers = root.path().join("markers");
        std::fs::create_dir_all(&markers).unwrap();
        let plugin_id = scenario.plugin_id();
        let process_name = format!("vosu-n0-{}", scenario.name);
        let mut replacements = vec![
            ("test.p0.testkit", plugin_id.as_str()),
            ("vosu-p0-target", process_name.as_str()),
        ];
        replacements.extend_from_slice(substitutions);
        let staged = Artifact::Testkit.stage(&root.path().join("package"), &replacements);
        let fixture = Fixture::new(&root.path().join("receipts"));
        fixture.control(&staged, scenario.fault, None, None);
        if scenario.descendant {
            let exe = std::env::current_exe().unwrap();
            let markers = markers.to_str().unwrap();
            let argv = [
                exe.to_str().unwrap(),
                "--exact",
                HELPER_TEST,
                "--nocapture",
                "--test-threads=1",
                ROLE,
                "descendant",
                markers,
            ];
            fixture.append_control(&staged, "spawn", &argv.join("\u{1f}"));
        }

        let (mut command, daemon) = match &kind {
            CoreKind::Helper => {
                let mut command = helper_command(&[
                    "core",
                    staged.dir.to_str().unwrap(),
                    root.path().to_str().unwrap(),
                ]);
                command.stdin(Stdio::null()).stdout(Stdio::null());
                (command, None)
            }
            CoreKind::Daemon(binary) => {
                let (command, daemon) = super::daemon::command(binary, root.path(), &staged);
                (command, Some(daemon))
            }
        };
        if scenario.refuse_seccomp {
            #[cfg(target_os = "linux")]
            super::refusal::refuse_seccomp(&mut command);
            #[cfg(windows)]
            panic!("the seccomp refusal control is Linux-only");
        }
        let child = command.spawn().expect("starting core");
        let process = Tracked::open(child.id(), None);
        #[cfg(target_os = "linux")]
        let refusals = scenario
            .refuse_seccomp
            .then(|| super::refusal::Refusals::start(&process));
        let core = Core {
            kind,
            child,
            process,
            daemon,
            #[cfg(target_os = "linux")]
            refusals,
        };
        // A daemon core starts no control of its own: the test does.
        let control_child = matches!(core.kind, CoreKind::Daemon(_)).then(|| {
            let mut sleeper = helper_command(&["sleeper", markers.to_str().unwrap()]);
            sleeper.stdin(Stdio::null()).stdout(Stdio::null());
            sleeper.spawn().unwrap()
        });
        let control = wait_marker(&markers, "sleeper", STARTUP_BOUND)
            .map(|marker| tracked_from(&marker))
            .expect("the outside control reported itself");
        Self {
            scenario,
            core,
            control,
            fixture,
            _staged: staged,
            markers,
            plugin_host: None,
            descendant: None,
            _control_child: control_child,
            _root: root,
        }
    }

    /// The plugin host: the core's child that ran `create` (its receipt
    /// names its PID), checked to be core's child running the expected
    /// executable before it is held.
    pub fn plugin_host(&mut self) -> &Tracked {
        if self.plugin_host.is_none() {
            let create = self
                .fixture
                .wait("create", STARTUP_BOUND)
                .expect("the plugin's create ran in a plugin host");
            let host = Tracked::open(create.pid, None);
            assert_eq!(
                parent_of(host.pid),
                Some(self.core.process.pid),
                "the plugin host is core's own child"
            );
            assert!(
                same_file(&image_of(&host), &self.expected_image()),
                "the plugin host runs core's executable"
            );
            #[cfg(target_os = "linux")]
            assert_eq!(
                super::tracked::argv1_of(host.pid).as_deref(),
                Some("plugin-host")
            );
            self.plugin_host = Some(host);
        }
        self.plugin_host.as_ref().unwrap()
    }

    /// The executable the plugin host must run: the same file as core's
    /// (the helper core launches the built `vosu` executable).
    fn expected_image(&self) -> PathBuf {
        match &self.core.daemon {
            Some(daemon) => daemon.executable.clone(),
            None => PathBuf::from(env!("CARGO_BIN_EXE_vosu")),
        }
    }

    /// The descendant the plugin started, with its own readback, checked
    /// to be the plugin host's child.
    pub fn descendant(&mut self) -> (&Tracked, HashMap<String, String>) {
        let host = self.plugin_host().pid;
        let receipt = self
            .fixture
            .wait("descendant", STARTUP_BOUND)
            .expect("the plugin started its descendant");
        assert_eq!(receipt.pid, host);
        let pid = receipt.get("child").to_owned();
        let marker = wait_marker(&self.markers, &format!("descendant-{pid}"), STARTUP_BOUND)
            .expect("the descendant reported itself");
        if self.descendant.is_none() {
            let descendant = tracked_from(&marker);
            assert_eq!(parent_of(descendant.pid), Some(host));
            self.descendant = Some(descendant);
        }
        (self.descendant.as_ref().unwrap(), marker)
    }

    /// Waits until the plugin reached `READY` and was published.
    pub fn wait_ready(&self) {
        match &self.core.daemon {
            None => {
                let created = wait_marker(&self.markers, "core-created", STARTUP_BOUND)
                    .expect("the core helper's create returned");
                assert_eq!(created["result"], "ok", "{created:?}");
            }
            Some(daemon) => {
                daemon.wait_plugin(&self.scenario.plugin_id(), STARTUP_BOUND, |view| {
                    view["state"] == "running"
                });
            }
        }
    }

    /// The core helper's `create` result, once it returned.
    pub fn created(&self) -> Option<HashMap<String, String>> {
        read_marker(&self.markers.join("core-created"))
    }

    /// The core helper's supervision status, recorded [`STATUS_AFTER`]
    /// after `create` returned (the Linux refusal control).
    #[cfg(target_os = "linux")]
    pub fn status_after_create(&self) -> HashMap<String, String> {
        wait_marker(&self.markers, "core-status", STARTUP_BOUND).expect("the core helper status")
    }

    /// Kills only the core and reaps it; returns when the kill was sent.
    pub fn kill_core_only(&mut self) -> Instant {
        let killed = Instant::now();
        self.core.process.kill();
        self.core.child.wait().unwrap();
        killed
    }

    /// Stops every thread of core (`SIGSTOP`, or `NtSuspendProcess`).
    pub fn suspend_core(&self) {
        self.core.process.suspend();
    }

    /// Kills the descendant left alive by design (the Linux residual).
    #[cfg(target_os = "linux")]
    pub fn kill_descendant(&self) {
        if let Some(descendant) = &self.descendant {
            descendant.kill();
            assert!(descendant.wait_exit(DEATH_BOUND).is_some());
        }
    }

    /// Asserts the outside control outlives the core's death by
    /// [`SURVIVAL_WINDOW`].
    pub fn assert_control_survives(&self) {
        assert!(
            self.control.wait_exit(SURVIVAL_WINDOW).is_none(),
            "the outside control died with core: the kill was not confined to core's own tree"
        );
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        for process in [&self.plugin_host, &self.descendant]
            .into_iter()
            .flatten()
            .chain([&self.control, &self.core.process])
        {
            process.kill();
        }
        let _ = self.core.child.kill();
        let _ = self.core.child.wait();
        if let Some(control) = &mut self._control_child {
            let _ = control.wait();
        }
    }
}

fn tracked_from(marker: &HashMap<String, String>) -> Tracked {
    Tracked::open(
        marker["pid"].parse().unwrap(),
        Some(marker["start"].parse().unwrap()),
    )
}

/// Reads a complete `key=value` marker.
fn read_marker(path: &Path) -> Option<HashMap<String, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect(),
    )
}

/// Waits up to `bound` for marker `name` in `dir`.
fn wait_marker(dir: &Path, name: &str, bound: Duration) -> Option<HashMap<String, String>> {
    let path = dir.join(name);
    let deadline = Instant::now() + bound;
    loop {
        if let Some(marker) = read_marker(&path) {
            return Some(marker);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Writes marker `name` atomically (a rename), so a reader sees all of it.
fn write_marker(dir: &Path, name: &str, fields: &[(&str, String)]) {
    let text: String = fields
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect();
    let partial = dir.join(format!(".{name}.partial"));
    std::fs::write(&partial, text).unwrap();
    std::fs::rename(partial, dir.join(name)).unwrap();
}

fn identity_fields() -> Vec<(&'static str, String)> {
    vec![
        ("pid", std::process::id().to_string()),
        ("start", own_start().to_string()),
    ]
}

/// The entry of every helper role; returns at once in an ordinary run.
pub fn run_role() {
    let args: Vec<String> = std::env::args().collect();
    let Some(at) = args.iter().position(|arg| arg == ROLE) else {
        return;
    };
    let role = &args[at + 1..];
    match role[0].as_str() {
        "core" => core(Path::new(&role[1]), Path::new(&role[2])),
        "descendant" => {
            let mut fields = identity_fields();
            fields.extend(descendant_readback());
            let name = format!("descendant-{}", std::process::id());
            write_marker(Path::new(&role[1]), &name, &fields);
        }
        "sleeper" => write_marker(Path::new(&role[1]), "sleeper", &identity_fields()),
        other => panic!("unknown helper role {other}"),
    }
    loop {
        std::thread::park();
    }
}

/// The core helper: starts its outside control, then creates the staged
/// package's instance through the production launcher, records the
/// result and, [`STATUS_AFTER`] later, its supervision status.
fn core(package: &Path, root: &Path) {
    let markers = root.join("markers");
    let mut sleeper = helper_command(&["sleeper", markers.to_str().unwrap()]);
    sleeper
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Never waited on: it must outlive this process.
    std::mem::forget(sleeper.spawn().unwrap());
    let host = Host::new(&root.join("core"));
    let loaded = host.load(package);
    let result = match host.create(1, &loaded) {
        Ok(()) => vec![("result", "ok".to_owned())],
        Err(vosu_core::plugin::HostError::ProcessStartupFailed(failure)) => vec![
            ("result", "failed".to_owned()),
            ("stage", failure.stage.as_str().to_owned()),
            ("code", failure.code.clone()),
            ("message", failure.message.replace('\n', " ")),
        ],
        Err(other) => vec![
            ("result", "error".to_owned()),
            ("error", other.to_string().replace('\n', " ")),
        ],
    };
    write_marker(&markers, "core-created", &result);
    std::thread::sleep(STATUS_AFTER);
    let status = host
        .host
        .process_status(vosu_core::plugin::PluginInstanceId::new(1));
    let fields = match status {
        Some(status) => vec![
            ("phase", format!("{:?}", status.phase)),
            ("restarts", status.restarts.to_string()),
            ("crash_events", status.crash_events.to_string()),
        ],
        None => vec![("phase", "unsupervised".to_owned())],
    };
    write_marker(&markers, "core-status", &fields);
    // `host` stays alive until this process is killed.
    loop {
        std::thread::park();
    }
}

/// What the descendant reads about itself: on Linux its security state
/// and cgroup, on Windows its immediate job.
fn descendant_readback() -> Vec<(&'static str, String)> {
    let mut fields = vec![(
        "parent",
        parent_of(std::process::id()).unwrap_or(0).to_string(),
    )];
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        for key in ["NoNewPrivs", "Seccomp", "CapEff"] {
            let value = status
                .lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
                .map(|value| value.trim().to_owned())
                .unwrap_or_default();
            fields.push((key, value));
        }
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
        fields.push(("cgroup", cgroup.trim().to_owned()));
    }
    #[cfg(windows)]
    {
        let job = super::jobs::current_job();
        fields.push(("job_flags", format!("{:#x}", job.limit_flags)));
        fields.push(("job_pids", job.pids));
    }
    fields
}
