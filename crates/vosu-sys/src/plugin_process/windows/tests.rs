//! Native Windows launch tests (`docs/v2/plugin-abi.md` §7.4).
//!
//! Children are this same test binary, re-executed with `--exact
//! HELPER_TEST` and a role, the way `vosu plugin-host` re-executes the
//! daemon: [`helper_process_entry`] does nothing in an ordinary run and
//! acts out the role in a child. Roles:
//!
//! - `probe`: a plugin host stand-in launched by `spawn_plugin_host`. At its
//!   first test-code instruction it adopts its endpoints, records its own
//!   immediate job, arguments and handle flags, optionally starts a
//!   descendant, then rings plugin → host and waits.
//! - `descendant`: a process the probe starts; it only stays alive.
//! - `core`: stands in for core in the core-death tests: it is itself
//!   launched into a job (so every probe it starts is nested), launches a
//!   probe with a descendant, and reports both to the test.
//!
//! Every barrier is a doorbell ring or a process exit with a bounded wait;
//! identities are process handles opened while alive and checked against
//! the creation time the process recorded, never a PID alone.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{self, BufRead};
use std::os::windows::io::{
    AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, IntoRawHandle, OwnedHandle,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessHandleInformation};
use windows_sys::Win32::Foundation::{
    CompareObjectHandles, DUPLICATE_SAME_ACCESS, DuplicateHandle, FILETIME, HANDLE, WAIT_FAILED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateEventW, GetCurrentProcess, GetProcessTimes, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, TerminateProcess, WaitForMultipleObjects,
};

use super::channel::{EndpointError, EndpointValues, adopt_inherited};
use super::job::{PLUGIN_JOB_LIMIT_FLAGS, PluginJob, current_job, is_inheritable};
use super::spawn::{
    PluginHostProcess, SpawnHooks, SpawnStep, UNWIND_EXIT_CODE, command_line, spawn_plugin_host,
    spawn_with, wait_exit,
};
use crate::plugin_process::RingObjects;

mod support;
use support::*;

const HELPER_TEST: &str = "plugin_process::windows::tests::helper_process_entry";
const ROLE: &str = "vosu-w0-role";
/// A small region; the launch code is size-agnostic.
const LEN: usize = 64 * 1024 + 256;
const BOUND: Duration = Duration::from_secs(30);
/// How long a negative control must stay alive after core died.
const SURVIVAL_WINDOW: Duration = Duration::from_secs(2);
/// Exit code of a probe told to exit through its host → plugin doorbell.
const PROBE_EXIT: i32 = 23;
/// A leading argument the probe must receive byte for byte.
const AWKWARD_ARG: &str = r#"a "quoted" path\ with\\ tail\"#;
const SYNCHRONIZE: u32 = 0x0010_0000;

// ---------------------------------------------------------------- roles

/// Not a test of its own: the entry point of every helper process. In an
/// ordinary run (no role argument) it returns at once.
#[test]
fn helper_process_entry() {
    let args: Vec<String> = std::env::args().collect();
    let Some(at) = args.iter().position(|arg| arg == ROLE) else {
        return;
    };
    let role = &args[at + 1..];
    let code = match role[0].as_str() {
        "probe" => probe(&args, Path::new(&role[1]), role[2] == "descendant"),
        "descendant" => descendant(),
        "core" => core(&args, Path::new(&role[1]), &role[2]),
        other => panic!("unknown helper role {other}"),
    };
    std::process::exit(code);
}

fn endpoint_arg(args: &[String]) -> &str {
    args.iter()
        .rev()
        .find(|arg| arg.starts_with("h2p="))
        .expect("helper launched without an endpoint argument")
}

fn probe(args: &[String], dir: &Path, with_descendant: bool) -> i32 {
    // First: the immediate job, before this process does anything else.
    let job = current_job().unwrap();
    let endpoints = adopt_inherited(endpoint_arg(args), LEN).unwrap();
    let mut marker = vec![
        ("pid".to_owned(), std::process::id().to_string()),
        ("created".to_owned(), creation_time(current()).to_string()),
        ("args".to_owned(), args[1..].join("\u{1f}")),
    ];
    match job {
        None => marker.push(("job".to_owned(), "none".to_owned())),
        Some(job) => {
            marker.push(("job".to_owned(), format!("{:#x}", job.limit_flags)));
            marker.push((
                "job_assigned".to_owned(),
                job.process_ids.assigned.to_string(),
            ));
            marker.push(("job_listed".to_owned(), join(&job.process_ids.listed)));
        }
    }
    let inheritable = [&endpoints.host_to_plugin, &endpoints.plugin_to_host]
        .iter()
        .flat_map(|objects| [objects.mapping_handle(), objects.doorbell_handle()])
        .filter(|&handle| is_inheritable(handle).unwrap())
        .count();
    marker.push(("inheritable".to_owned(), inheritable.to_string()));
    // Kept alive (and its pipe open) until this process ends.
    let _descendant = with_descendant.then(|| {
        use std::os::windows::process::CommandExt as _;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(helper_args(&["descendant"]))
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut lines = io::BufReader::new(child.stdout.take().unwrap()).lines();
        // The harness prints "test <name> ... " without a newline before the
        // test body runs, so the report can follow it on the same line.
        let line = lines
            .find_map(|line| {
                line.unwrap()
                    .split_once("vosu-w0-descendant ")
                    .map(|(_, report)| report.to_owned())
            })
            .expect("descendant reported itself");
        let (pid, created) = line.split_once(' ').unwrap();
        marker.push(("descendant_pid".to_owned(), pid.to_owned()));
        marker.push(("descendant_created".to_owned(), created.to_owned()));
        (child, lines)
    });
    write_marker(dir, &format!("probe-{}", std::process::id()), &marker);
    endpoints.plugin_to_host.doorbell().ring().unwrap();
    // Exit only when told to (or when killed).
    endpoints.host_to_plugin.doorbell().wait(None).unwrap();
    PROBE_EXIT
}

fn descendant() -> i32 {
    use std::io::Write as _;
    // `io::stdout()` directly: the test harness may capture `println!`.
    let mut stdout = io::stdout();
    writeln!(
        stdout,
        "vosu-w0-descendant {} {}",
        std::process::id(),
        creation_time(current())
    )
    .unwrap();
    stdout.flush().unwrap();
    loop {
        std::thread::park();
    }
}

fn core(args: &[String], dir: &Path, mode: &str) -> i32 {
    let upward = adopt_inherited(endpoint_arg(args), LEN).unwrap();
    let to_probe = RingObjects::create(LEN).unwrap();
    let from_probe = RingObjects::create(LEN).unwrap();
    let mut hooks = match mode {
        "strict" => Hooks::default(),
        "unassigned" => Hooks {
            skip_assign: true,
            ..Hooks::default()
        },
        "leaked-job" => Hooks {
            leak_job: true,
            ..Hooks::default()
        },
        other => panic!("unknown core mode {other}"),
    };
    let probe = spawn_with(
        &std::env::current_exe().unwrap(),
        &os_args(&helper_args(&[
            "probe",
            dir.to_str().unwrap(),
            "descendant",
        ])),
        [&to_probe, &from_probe],
        &mut hooks,
    )
    .unwrap();
    wait_ready(&from_probe, probe.process(), dir);
    write_marker(
        dir,
        "core",
        &[("probe_pid".to_owned(), probe.pid().to_string())],
    );
    upward.plugin_to_host.doorbell().ring().unwrap();
    // Core stays alive, holding `probe` and its job, until the test kills it.
    loop {
        std::thread::park();
    }
}

// ---------------------------------------------------------------- hooks

#[derive(Default)]
struct Hooks {
    skip_assign: bool,
    leak_job: bool,
    fail_created: bool,
    fail_before_resume: bool,
    /// The suspended child, duplicated for the test (wait and exit code).
    process: Option<OwnedHandle>,
    /// The suspended child's handle table, read just before resume.
    table: Option<Vec<ChildHandle>>,
    /// `IsProcessInJob` against the job's own handle, just before resume.
    in_job_before_resume: Option<bool>,
    /// Objects to compare the child's handles against, by label.
    probes: Vec<(&'static str, OwnedHandle)>,
}

impl SpawnHooks for Hooks {
    fn extra_inherited(&mut self, job: &PluginJob) -> io::Result<Vec<OwnedHandle>> {
        if !self.leak_job {
            return Ok(Vec::new());
        }
        Ok(vec![duplicate(
            job.as_handle(),
            0,
            true,
            DUPLICATE_SAME_ACCESS,
        )?])
    }

    fn created(&mut self, process: BorrowedHandle<'_>) -> io::Result<()> {
        self.process = Some(duplicate(
            process,
            SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            false,
            0,
        )?);
        if self.fail_created {
            return Err(io::Error::other("injected failure before assignment"));
        }
        Ok(())
    }

    fn assign(&mut self) -> bool {
        !self.skip_assign
    }

    fn before_resume(&mut self, job: &PluginJob, process: BorrowedHandle<'_>) -> io::Result<()> {
        self.in_job_before_resume = Some(job.contains(process)?);
        self.probes.push((
            "job",
            duplicate(job.as_handle(), 0, false, DUPLICATE_SAME_ACCESS)?,
        ));
        self.table = Some(child_handles(process)?);
        if self.fail_before_resume {
            return Err(io::Error::other("injected failure before resume"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- tests

#[test]
fn spawn_assigns_the_strict_job_before_resume_and_inherits_exactly_the_ring_objects() {
    let dir = tempfile::tempdir().unwrap();
    let to_plugin = RingObjects::create(LEN).unwrap();
    let to_host = RingObjects::create(LEN).unwrap();
    // An unrelated inheritable handle (a credential stand-in) and the
    // parent's own process must never reach the child.
    let sentinel = inheritable_event();
    let mut hooks = Hooks {
        probes: vec![
            (
                "sentinel",
                duplicate(sentinel.as_handle(), 0, false, DUPLICATE_SAME_ACCESS).unwrap(),
            ),
            (
                "parent",
                duplicate(current(), 0, false, DUPLICATE_SAME_ACCESS).unwrap(),
            ),
        ],
        ..Hooks::default()
    };
    let leading = helper_args(&["probe", dir.path().to_str().unwrap(), "alone", AWKWARD_ARG]);
    let child = spawn_with(
        &std::env::current_exe().unwrap(),
        &os_args(&leading),
        [&to_plugin, &to_host],
        &mut hooks,
    )
    .unwrap();

    assert_eq!(hooks.in_job_before_resume, Some(true));
    let table = hooks.table.take().unwrap();
    let rings = [
        (
            "h2p section",
            to_plugin.mapping_handle(),
            super::spawn::SECTION_ACCESS,
        ),
        (
            "h2p event",
            to_plugin.doorbell_handle(),
            super::spawn::EVENT_ACCESS,
        ),
        (
            "p2h section",
            to_host.mapping_handle(),
            super::spawn::SECTION_ACCESS,
        ),
        (
            "p2h event",
            to_host.doorbell_handle(),
            super::spawn::EVENT_ACCESS,
        ),
    ];
    let mut inherited_values = Vec::new();
    for (label, original, access) in rings {
        let matches: Vec<_> = table
            .iter()
            .filter(|entry| entry.same_object(original))
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "{label} inherited exactly once: {table:?}"
        );
        assert_eq!(matches[0].access, access, "{label} keeps only its access");
        inherited_values.push(matches[0].value);
    }
    for (label, object) in &hooks.probes {
        assert!(
            !table
                .iter()
                .any(|entry| entry.same_object(object.as_handle())),
            "{label} handle reached the child: {table:?}"
        );
    }
    assert_eq!(table.len(), 4, "nothing but the ring objects: {table:?}");

    wait_ready(&to_host, child.process(), dir.path());
    let marker = read_marker(dir.path(), &format!("probe-{}", child.pid()));
    // The child's first test-code instruction already ran inside a job of
    // its own with exactly the plugin-host limits.
    assert_eq!(marker["job"], format!("{PLUGIN_JOB_LIMIT_FLAGS:#x}"));
    assert_eq!(marker["job_assigned"], "1");
    assert_eq!(marker["job_listed"], child.pid().to_string());
    assert_eq!(
        marker["inheritable"], "0",
        "adopted handles stop inheriting"
    );
    let received: Vec<&str> = marker["args"].split('\u{1f}').collect();
    let endpoint = endpoint_values(&marker["args"]);
    let mut expected: Vec<&str> = leading.iter().map(String::as_str).collect();
    expected.push(endpoint);
    assert_eq!(received, expected, "argv round-trips exactly");
    // The values on the command line are the handles the child inherited.
    let named = EndpointValues::parse(endpoint).unwrap();
    assert_eq!(
        inherited_values,
        [named.host_to_plugin, named.plugin_to_host].concat()
    );

    // Parent-side, job-specific evidence.
    assert!(child.job().contains(child.process()).unwrap());
    let ids = child.job().process_ids().unwrap();
    assert_eq!((ids.assigned, ids.listed), (1, vec![child.pid()]));
    assert_eq!(child.job().limit_flags().unwrap(), PLUGIN_JOB_LIMIT_FLAGS);
    assert!(!is_inheritable(child.job().as_handle()).unwrap());

    to_plugin.doorbell().ring().unwrap();
    assert_eq!(child.wait(Some(BOUND)).unwrap(), Some(PROBE_EXIT as u32));
}

#[test]
fn failure_before_assignment_terminates_and_reaps_the_suspended_child() {
    assert_unwound(
        Hooks {
            fail_created: true,
            ..Hooks::default()
        },
        SpawnStep::AssignJob,
    );
}

#[test]
fn failure_after_assignment_before_resume_terminates_and_reaps_the_child() {
    assert_unwound(
        Hooks {
            fail_before_resume: true,
            ..Hooks::default()
        },
        SpawnStep::Resume,
    );
}

/// A spawn failing at `step` returns only after its child was terminated
/// with [`UNWIND_EXIT_CODE`] and reaped; the child never ran (no marker),
/// and the parent holds no inheritable duplicate any more.
fn assert_unwound(mut hooks: Hooks, step: SpawnStep) {
    let dir = tempfile::tempdir().unwrap();
    let to_plugin = RingObjects::create(LEN).unwrap();
    let to_host = RingObjects::create(LEN).unwrap();
    let error = spawn_with(
        &std::env::current_exe().unwrap(),
        &os_args(&helper_args(&[
            "probe",
            dir.path().to_str().unwrap(),
            "alone",
        ])),
        [&to_plugin, &to_host],
        &mut hooks,
    )
    .unwrap_err();
    assert_eq!(error.step, step);
    let unwound = error.unwound.expect("a created child was unwound");
    assert_eq!(unwound.exit_code, Some(UNWIND_EXIT_CODE));
    let process = hooks.process.expect("the created hook saw the child");
    assert_eq!(
        wait_exit(process.as_handle(), Some(Duration::ZERO)).unwrap(),
        Some(UNWIND_EXIT_CODE),
        "the child had already exited when spawn returned"
    );
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "the child never ran"
    );
    // The hook's copies of the child's handles are not spawn's duplicates.
    drop(hooks.table.take());
    assert_only_originals(&to_plugin, &to_host);
}

#[test]
fn missing_program_fails_at_creation_and_closes_every_duplicate() {
    let to_plugin = RingObjects::create(LEN).unwrap();
    let to_host = RingObjects::create(LEN).unwrap();
    let missing = std::env::temp_dir().join("vosu-w0-missing-program.exe");
    let error = spawn_plugin_host(&missing, &[], &to_plugin, &to_host).unwrap_err();
    assert_eq!(error.step, SpawnStep::CreateProcess);
    assert_eq!(error.step.startup_stage(), "spawn");
    assert!(error.unwound.is_none());
    assert_only_originals(&to_plugin, &to_host);
}

#[test]
fn dropping_the_process_kills_the_plugin_host_and_its_descendant() {
    let dir = tempfile::tempdir().unwrap();
    let to_plugin = RingObjects::create(LEN).unwrap();
    let to_host = RingObjects::create(LEN).unwrap();
    let child = spawn_plugin_host(
        &std::env::current_exe().unwrap(),
        &os_args(&helper_args(&[
            "probe",
            dir.path().to_str().unwrap(),
            "descendant",
        ])),
        &to_plugin,
        &to_host,
    )
    .unwrap();
    wait_ready(&to_host, child.process(), dir.path());
    let marker = read_marker(dir.path(), &format!("probe-{}", child.pid()));
    let plugin_host = open_verified(&marker["pid"], &marker["created"]);
    let descendant = open_verified(&marker["descendant_pid"], &marker["descendant_created"]);
    // The descendant (and the hidden console host its console needs) were
    // created inside the plugin host's job.
    let ids = child.job().process_ids().unwrap();
    for pid in [&marker["pid"], &marker["descendant_pid"]] {
        let pid: u32 = pid.parse().unwrap();
        assert!(ids.listed.contains(&pid), "{pid} in the job: {ids:?}");
    }
    drop(child);
    for process in [&plugin_host, &descendant] {
        assert!(
            wait_exit(process.as_handle(), Some(BOUND))
                .unwrap()
                .is_some(),
            "closing core's job handle ends the whole tree"
        );
    }
}

#[test]
fn nested_job_core_death_kills_plugin_host_and_descendant() {
    let tree = CoreTree::launch("strict");
    // At its first instruction the probe's immediate job was its own
    // plugin job holding only itself ...
    assert_eq!(
        tree.probe_marker["job"],
        format!("{PLUGIN_JOB_LIMIT_FLAGS:#x}")
    );
    assert_eq!(tree.probe_marker["job_assigned"], "1");
    // ... nested inside the core's job, which also holds the probe and its
    // descendant: the core's job must not be what kills them below.
    for process in [&tree.probe, &tree.descendant] {
        assert!(tree.core.job().contains(process.as_handle()).unwrap());
    }
    tree.kill_core_only();
    for process in [&tree.probe, &tree.descendant] {
        assert!(
            wait_exit(process.as_handle(), Some(BOUND))
                .unwrap()
                .is_some(),
            "core-only death ends the nested plugin-host tree"
        );
    }
}

#[test]
fn nested_job_negative_control_without_assignment_survives_core_death() {
    let tree = CoreTree::launch("unassigned");
    // Without assignment the probe's immediate job is the core's own,
    // which also lists the core.
    let listed: Vec<&str> = tree.probe_marker["job_listed"].split(',').collect();
    assert!(
        listed.contains(&tree.core.pid().to_string().as_str()),
        "{listed:?}"
    );
    tree.kill_core_only();
    tree.assert_survives();
}

#[test]
fn nested_job_negative_control_with_leaked_job_handle_survives_core_death() {
    let tree = CoreTree::launch("leaked-job");
    tree.kill_core_only();
    tree.assert_survives();
}

#[test]
fn endpoint_argument_round_trips_and_refuses_every_noncanonical_form() {
    let values = EndpointValues {
        host_to_plugin: [0x1a4, 0x1a8],
        plugin_to_host: [0x2c, 0x7fff_fff0],
    };
    let arg = values.to_arg();
    assert_eq!(arg, "h2p=1a4:1a8,p2h=2c:7ffffff0");
    assert_eq!(EndpointValues::parse(&arg).unwrap(), values);
    for bad in [
        "",
        "h2p=1a4:1a8",
        "h2p=1a4:1a8,p2h=2c",
        "h2p=1a4:1a8,p2h=2c:30,",
        "p2h=2c:30,h2p=1a4:1a8",
        "h2p=01a4:1a8,p2h=2c:30",
        "h2p=1A4:1a8,p2h=2c:30",
        "h2p=0x1a4:1a8,p2h=2c:30",
        "h2p=+1a4:1a8,p2h=2c:30",
        "h2p=1a4:1a8,p2h=2c:30 ",
        "h2p=1a4:1a8,p2h=2c:11111111111111111",
    ] {
        assert!(
            matches!(EndpointValues::parse(bad), Err(EndpointError::Malformed)),
            "{bad:?}"
        );
    }
    for bad in [
        "h2p=1a4:1a4,p2h=2c:30",
        "h2p=1a4:1a8,p2h=2c:1a8",
        "h2p=1a5:1a8,p2h=2c:30",
        "h2p=1a4:1a8,p2h=0:30",
    ] {
        assert!(
            matches!(EndpointValues::parse(bad), Err(EndpointError::InvalidValue)),
            "{bad:?}"
        );
    }
}

#[test]
fn adoption_refuses_wrong_kinds_closed_handles_and_wrong_sizes() {
    let to_plugin = RingObjects::create(LEN).unwrap();
    let to_host = RingObjects::create(LEN).unwrap();
    let other = RingObjects::create(LEN + 8192).unwrap();
    let copies = |objects: &RingObjects| {
        [
            duplicate(objects.mapping_handle(), 0, false, DUPLICATE_SAME_ACCESS).unwrap(),
            duplicate(objects.doorbell_handle(), 0, false, DUPLICATE_SAME_ACCESS).unwrap(),
        ]
    };
    let [a, b] = copies(&to_plugin);
    let [c, d] = copies(&to_host);
    let [wrong_size, _] = copies(&other);
    let value = |handle: &OwnedHandle| handle.as_raw_handle() as usize;
    let arg = |h2p: [usize; 2], p2h: [usize; 2]| {
        EndpointValues {
            host_to_plugin: h2p,
            plugin_to_host: p2h,
        }
        .to_arg()
    };
    // Event where a section belongs, and the reverse.
    assert!(matches!(
        adopt_inherited(&arg([value(&b), value(&a)], [value(&c), value(&d)]), LEN),
        Err(EndpointError::WrongKind {
            expected: "Section",
            ..
        })
    ));
    assert!(matches!(
        adopt_inherited(&arg([value(&a), value(&c)], [value(&b), value(&d)]), LEN),
        Err(EndpointError::WrongKind {
            expected: "Event",
            ..
        })
    ));
    assert!(matches!(
        adopt_inherited(
            &arg([value(&wrong_size), value(&b)], [value(&c), value(&d)]),
            LEN
        ),
        Err(EndpointError::WrongSize { .. })
    ));
    // A value naming no handle: beyond the largest possible handle table.
    let closed = 0x7fff_fff0;
    assert!(matches!(
        adopt_inherited(&arg([closed, value(&b)], [value(&c), value(&d)]), LEN),
        Err(EndpointError::WrongKind { .. })
    ));
    // Every refusal above left all four copies open and unowned.
    let good = arg([value(&a), value(&b)], [value(&c), value(&d)]);
    let adopted = {
        // Ownership passes to the adopted objects.
        let _released = [a, b, c, d].map(IntoRawHandle::into_raw_handle);
        adopt_inherited(&good, LEN).unwrap()
    };
    for objects in [&adopted.host_to_plugin, &adopted.plugin_to_host] {
        assert!(!is_inheritable(objects.mapping_handle()).unwrap());
        assert!(!is_inheritable(objects.doorbell_handle()).unwrap());
    }
    adopted.plugin_to_host.doorbell().ring().unwrap();
    assert!(
        to_host.doorbell().consume().unwrap(),
        "adopted the same event"
    );
}

#[test]
fn command_line_quotes_every_argument_for_the_runtime_parser() {
    let values = EndpointValues {
        host_to_plugin: [4, 8],
        plugin_to_host: [12, 16],
    };
    let line = command_line(
        Path::new(r"C:\Program Files\vosu\vosu.exe"),
        &[
            OsStr::new("plugin-host"),
            OsStr::new(""),
            OsStr::new(r"has space"),
            OsStr::new(r#"say "hi""#),
            OsStr::new(r"trailing\"),
            OsStr::new(r#"slash\"quote"#),
        ],
        &values,
    )
    .unwrap();
    let text = String::from_utf16(&line[..line.len() - 1]).unwrap();
    assert_eq!(
        text,
        r#""C:\Program Files\vosu\vosu.exe" plugin-host "" "has space" "say \"hi\"" trailing\ "slash\\\"quote" h2p=4:8,p2h=c:10"#
    );
    assert_eq!(line.last(), Some(&0));
    assert!(command_line(Path::new("a\"b"), &[], &values).is_err());
    assert!(command_line(Path::new("a"), &[OsStr::new("x\0y")], &values).is_err());
}
