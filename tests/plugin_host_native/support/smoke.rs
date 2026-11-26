//! P0 smoke evidence: the prepared artifacts stage exactly, and each
//! testkit fault mode actually triggers in a real `vosu plugin-host` child
//! started by core's production launcher. These prove the fixtures, not the
//! supervisor: crash, stall and loss consequences are asserted by the X0/C0
//! modules.

use std::time::{Duration, Instant};

use vosu_core::plugin::HostError;
use vosu_plugin_api::{
    ActionCancel, ActionInvoke, Caller, CallerKind, CborValue, FocusChanged, Message, vosu_opcode,
};

use super::artifacts::{Artifact, Staged, sha256};
use super::fixture::{Fixture, Receipt};
use super::host::{DESTROY_GRACE, Host, Identity, wait_until};

const BOUND: Duration = Duration::from_secs(20);
const FOCUS: u32 = vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32;

fn focus(focused: bool) -> Message {
    Message::FocusChanged(FocusChanged {
        handle: 7,
        focused,
        foreground: None,
    })
}

/// One test's staged testkit package, run control and host.
struct Run {
    // Field order is drop order: the host (and its children) go first.
    host: Host,
    fixture: Fixture,
    staged: Staged,
    _root: tempfile::TempDir,
}

impl Run {
    fn testkit(fault: &str, trigger: Option<u32>) -> Self {
        let root = super::scratch();
        let staged = Artifact::Testkit.stage(&root.path().join("package"), &[]);
        let fixture = Fixture::new(&root.path().join("receipts"));
        fixture.control(&staged, fault, trigger, None);
        Self {
            host: Host::new(root.path()),
            fixture,
            staged,
            _root: root,
        }
    }

    /// Creates the instance and records how long startup took, the evidence
    /// for the startup-deadline budget.
    fn create(&self) -> Result<(), HostError> {
        let loaded = self.host.load(&self.staged.dir);
        let started = Instant::now();
        let result = self.host.create(1, &loaded);
        println!(
            "RECORD create {} in {:?} ({})",
            self.staged.dir.display(),
            started.elapsed(),
            if result.is_ok() { "ready" } else { "failed" }
        );
        result
    }

    fn receipt(&self, probe: &str) -> Receipt {
        self.fixture
            .wait(probe, BOUND)
            .unwrap_or_else(|| panic!("no {probe} receipt"))
    }

    /// The child the `create` receipt names: never this test process.
    fn child(&self) -> u32 {
        let pid = self.receipt("create").pid;
        assert_ne!(pid, std::process::id(), "create ran in core");
        pid
    }
}

fn startup_stage(result: Result<(), HostError>) -> String {
    match result {
        Err(HostError::ProcessStartupFailed(failure)) => failure.to_string(),
        other => panic!("expected a startup failure, got {other:?}"),
    }
}

#[test]
fn support_artifacts_stage_the_prepared_cargo_libraries_per_test() {
    let root = super::scratch();
    for artifact in [Artifact::Testkit, Artifact::Osu] {
        let source = artifact.library();
        let first = artifact.stage(&root.path().join(format!("{artifact:?}-a")), &[]);
        let second = artifact.stage(&root.path().join(format!("{artifact:?}-b")), &[]);
        assert_eq!(first.sha256, sha256(&source), "{artifact:?}");
        assert_eq!(first.sha256, second.sha256, "{artifact:?}");
        assert_ne!(
            first.library, second.library,
            "packages share a library copy"
        );
    }
    // The testkit validates as a non-bundled development package.
    let host = Host::new(&root.path().join("host"));
    let testkit = Artifact::Testkit.stage(&root.path().join("testkit"), &[]);
    drop(host.load(&testkit.dir));

    // The bundled manifest stays target-free and in-process; the external
    // osu! fixture differs from it only by its runtime and its targets.
    let bundled = vosu_osu::MANIFEST;
    assert!(bundled.contains("runtime = \"in-process\""));
    assert!(!bundled.contains("[targets"));
    let fixture = Artifact::Osu.manifest(&[]);
    let (body, targets) = fixture.split_once("\n[targets.").unwrap();
    assert_eq!(
        body.trim_end(),
        bundled
            .replacen("runtime = \"in-process\"", "runtime = \"process\"", 1)
            .trim_end()
    );
    assert!(targets.contains("osu_process_plugin"), "{targets}");
    // It keeps the official `vosu.osu` identity the parity comparison needs:
    // valid for an official source, refused as an ordinary development one.
    let package = vosu_plugin_api::manifest::PackageManifest::parse(&fixture).unwrap();
    let snapshot = vosu_core::plugin::ValidationSnapshot::default();
    let version = &vosu::BUNDLE.version();
    assert_eq!(
        snapshot
            .validate(&package, version, "development", true)
            .unwrap(),
        vosu_plugin_api::manifest::HostSupport::Supported
    );
    let refused = snapshot
        .validate(&package, version, "development", false)
        .unwrap_err();
    assert_eq!(refused.rule, "manifest.namespace");
}

#[test]
fn support_testkit_constructor_and_create_run_only_in_the_plugin_host() {
    let run = Run::testkit("none", None);
    run.create().expect("the testkit reaches READY");
    let pid = run.child();
    let identity = Identity::of(pid);
    let constructor = run.receipt("constructor");
    assert_eq!(constructor.pid, pid, "only the child mapped the library");
    assert_eq!(
        constructor.get("library"),
        run.staged.library.file_name().unwrap().to_str().unwrap()
    );
    assert_eq!(run.receipt("create").get("plugin"), "test.p0.testkit");
    #[cfg(target_os = "linux")]
    {
        // The readback the constructor took before any plugin code beyond
        // it ran (N0 asserts the packaged binary's; this is the debug one).
        assert_eq!(constructor.get("seccomp"), "2");
        assert_eq!(constructor.get("nonewprivs"), "1");
        assert_eq!(constructor.get("threads"), "1");
        for set in ["capinh", "capprm", "capeff", "capamb"] {
            assert_eq!(constructor.get(set), "0000000000000000", "{set}");
        }
        println!("RECORD capbnd={} at constructor", constructor.get("capbnd"));
    }

    run.host.dispatch(1, &focus(true)).unwrap();
    let dispatch = run.receipt("dispatch");
    assert_eq!(
        (dispatch.pid, dispatch.get("opcode")),
        (pid, FOCUS.to_string().as_str())
    );

    let started = Instant::now();
    run.host.destroy_all();
    assert!(
        started.elapsed() < DESTROY_GRACE,
        "a clean destroy needs no kill"
    );
    assert_eq!(run.receipt("destroy").pid, pid);
    assert!(
        !identity.alive(),
        "destroy returned before the child was reaped"
    );
}

#[test]
fn support_fault_constructor_crash_aborts_the_child_before_create() {
    let run = Run::testkit("constructor_crash", None);
    let reason = startup_stage(run.create());
    assert!(reason.starts_with("stage crash"), "{reason}");
    println!("RECORD constructor_crash: {reason}");
    let constructor = run.receipt("constructor");
    assert_ne!(
        constructor.pid,
        std::process::id(),
        "core mapped the library"
    );
    assert!(run.fixture.receipts("create").is_empty(), "create ran");
    #[cfg(target_os = "linux")]
    assert!(!std::path::Path::new(&format!("/proc/{}", constructor.pid)).exists());
}

#[test]
fn support_fault_create_crash_aborts_the_child_inside_create() {
    let run = Run::testkit("create_crash", None);
    let reason = startup_stage(run.create());
    assert!(reason.starts_with("stage crash"), "{reason}");
    println!("RECORD create_crash: {reason}");
    let create = run.receipt("create");
    assert_eq!(run.receipt("constructor").pid, create.pid);
    assert_ne!(create.pid, std::process::id());
}

#[test]
fn support_fault_dispatch_crash_aborts_the_child_on_its_trigger() {
    let run = Run::testkit("dispatch_crash", Some(FOCUS));
    run.create().unwrap();
    let identity = Identity::of(run.child());
    run.host.dispatch(1, &focus(true)).unwrap();
    assert_eq!(run.receipt("crash").get("opcode"), FOCUS.to_string());
    assert!(
        wait_until(BOUND, || !identity.alive()),
        "the child survived"
    );
    assert!(run.host.wait_retired(1, &focus(false), BOUND));
    assert!(
        run.fixture.receipts("destroy").is_empty(),
        "a crash ran destroy"
    );
}

/// The real `std::process::abort` mode C0/X0 use to prove crash handling
/// (on Windows, a fail-fast that the plugin host's error mode keeps Windows
/// Error Reporting from holding, plugin-abi.md §7.4). The exit delay is
/// recorded.
#[test]
fn support_fault_dispatch_abort_really_aborts_the_child() {
    let run = Run::testkit("dispatch_abort", Some(FOCUS));
    run.create().unwrap();
    let identity = Identity::of(run.child());
    run.host.dispatch(1, &focus(true)).unwrap();
    assert_eq!(run.receipt("abort").get("opcode"), FOCUS.to_string());
    let aborted = Instant::now();
    assert!(
        wait_until(BOUND, || !identity.alive()),
        "the aborted child survived"
    );
    println!("RECORD dispatch_abort exit after {:?}", aborted.elapsed());
    assert!(run.host.wait_retired(1, &focus(false), BOUND));
    assert!(
        run.fixture.receipts("destroy").is_empty(),
        "an abort ran destroy"
    );
}

#[test]
fn support_fault_dispatch_block_holds_later_deliveries_until_released() {
    let run = Run::testkit("dispatch_block", Some(FOCUS));
    run.create().unwrap();
    let pid = run.child();
    run.host.dispatch(1, &focus(true)).unwrap();
    run.receipt("blocked");
    // Queued behind the blocked dispatch.
    run.host.dispatch(1, &focus(false)).unwrap();
    assert!(Identity::of(pid).alive());
    run.fixture.release(pid);
    assert_eq!(run.receipt("released").get("released"), "true");
    assert!(
        wait_until(BOUND, || run.fixture.receipts("dispatch").len() == 2),
        "the queued delivery never ran"
    );
    let order: Vec<String> = run
        .fixture
        .journal(pid)
        .into_iter()
        .map(|receipt| receipt.probe)
        .filter(|probe| ["dispatch", "blocked", "released"].contains(&probe.as_str()))
        .collect();
    assert_eq!(order, ["dispatch", "blocked", "released", "dispatch"]);
}

#[test]
fn support_fault_cancel_ignored_leaves_the_invocation_running() {
    for (fault, ignored) in [("cancel_ignored", "true"), ("none", "false")] {
        let run = Run::testkit(fault, None);
        run.create().unwrap();
        let pid = run.child();
        run.host
            .dispatch(
                1,
                &Message::ActionInvoke(ActionInvoke {
                    invocation: 41,
                    action: "test.p0.testkit.hold".to_owned(),
                    session: None,
                    caller: Caller {
                        kind: CallerKind::Tool,
                        id: "p0-smoke".to_owned(),
                    },
                    args: CborValue::Map(Vec::new()),
                }),
            )
            .unwrap();
        run.host
            .dispatch(1, &Message::ActionCancel(ActionCancel { invocation: 41 }))
            .unwrap();
        assert_eq!(run.receipt("invoke").get("invocation"), "41", "{fault}");
        let cancel = run.receipt("cancel");
        assert_eq!(
            (cancel.pid, cancel.get("ignored")),
            (pid, ignored),
            "{fault}"
        );
        assert!(Identity::of(pid).alive(), "{fault}");
    }
}

#[test]
fn support_fault_destroy_ignored_is_killed_after_the_grace() {
    let run = Run::testkit("destroy_ignored", None);
    run.create().unwrap();
    let identity = Identity::of(run.child());
    let started = Instant::now();
    run.host.destroy_all();
    let elapsed = started.elapsed();
    run.receipt("destroy");
    assert!(
        elapsed >= DESTROY_GRACE - Duration::from_millis(500) && elapsed < BOUND,
        "{elapsed:?}"
    );
    assert!(
        !identity.alive(),
        "the ignoring child was not killed and reaped"
    );
}

#[test]
fn support_fault_ring_stall_stops_the_whole_child() {
    let run = Run::testkit("ring_stall", Some(FOCUS));
    run.create().unwrap();
    let identity = Identity::of(run.child());
    run.host.dispatch(1, &focus(true)).unwrap();
    run.receipt("stall");
    assert!(
        wait_until(BOUND, || identity.stopped()),
        "the child's threads are not stopped"
    );
    run.host.destroy_all();
    assert!(
        !identity.alive(),
        "the stalled child was not killed and reaped"
    );
}

#[test]
fn support_fault_corrupt_frame_ends_the_channel() {
    let run = Run::testkit("corrupt_frame", Some(FOCUS));
    run.create().unwrap();
    let identity = Identity::of(run.child());
    run.host.dispatch(1, &focus(true)).unwrap();
    run.receipt("corrupt");
    assert!(
        run.fixture.receipts("corrupt_failed").is_empty(),
        "{:?}",
        run.fixture.receipts("corrupt_failed")
    );
    assert!(
        run.host.wait_retired(1, &focus(false), BOUND),
        "core accepted the forged frame"
    );
    assert!(
        wait_until(BOUND, || !identity.alive()),
        "the corrupt child survived"
    );
    assert!(
        run.fixture.receipts("destroy").is_empty(),
        "a loss ran destroy"
    );
}

/// Two instances, each matching only its own synthetic target, read their
/// own target's distinct bytes through core.
#[test]
fn support_distinct_data_source_targets_read_their_own_bytes() {
    let root = super::scratch();
    let host = Host::new(root.path());
    let targets = [
        ("a", 0x7FFF_F0A1_u32, 0x1111_2222_3333_4444_u64),
        ("b", 0x7FFF_F0B2_u32, 0x5555_6666_7777_8888_u64),
    ];
    let mut runs = Vec::new();
    for (index, (name, pid, marker)) in targets.into_iter().enumerate() {
        let id = format!("test.p0.testkit.{name}");
        let process = format!("vosu-p0-target-{name}");
        let staged = Artifact::Testkit.stage(
            &root.path().join(format!("package-{name}")),
            &[
                ("test.p0.testkit", id.as_str()),
                ("vosu-p0-target", process.as_str()),
            ],
        );
        let fixture = Fixture::new(&root.path().join(format!("receipts-{name}")));
        fixture.control(&staged, "none", None, Some((0, 8)));
        host.create(index as u64 + 1, &host.load(&staged.dir))
            .unwrap();
        runs.push((fixture, process, pid, marker));
    }
    for (_, process, pid, marker) in &runs {
        host.offer_target(*pid, process, marker.to_le_bytes().to_vec());
    }
    let mut children = Vec::new();
    for (fixture, _, pid, marker) in &runs {
        let attach = fixture.wait("attach", BOUND).expect("SESSION_ATTACHED");
        assert_eq!(attach.get("target"), pid.to_string());
        assert_eq!(attach.get("submit"), "0", "the read plan was refused");
        let read = fixture.wait("read", BOUND).expect("READ_PLAN_RESULT");
        assert_eq!(read.pid, attach.pid);
        assert_eq!(read.get("result"), "ok");
        let expected: String = marker
            .to_le_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(read.get("bytes"), expected, "instance read another target");
        children.push(read.pid);
    }
    assert_ne!(
        children[0], children[1],
        "two instances shared one plugin host"
    );
}

/// The external osu! library maps, creates its producer and reaches
/// `READY` in a plugin host, and only there. Its fixture package keeps the
/// official `vosu.osu` identity and internal-visibility schema, which the
/// ordinary development loader refuses (see the artifacts test), so this
/// launch smoke wraps the same prepared library in core's generic
/// development process manifest. Parity (X0) needs an approved official
/// loading path for the fixture package itself.
#[test]
fn support_osu_process_plugin_reaches_ready_in_the_plugin_host() {
    let root = super::scratch();
    let library = Artifact::Osu.library();
    let file_name = library.file_name().unwrap().to_str().unwrap();
    let package = root.path().join("package");
    vosu_core::plugin::test_support::write_development_package(
        &package,
        "test.p0.osu",
        "vosu-p0-osu-target",
        "process",
        file_name,
        &std::fs::read(&library).unwrap(),
    )
    .unwrap();
    let staged = package.join("lib").join(file_name);
    println!(
        "RECEIPT artifact={} sha256={} path={}",
        Artifact::Osu.name(),
        sha256(&staged),
        staged.display()
    );
    let host = Host::new(root.path());
    let loaded = host.load(&package);
    assert!(
        matches!(loaded.execution(), vosu_core::plugin::Execution::Process(_)),
        "the osu! package is not a process package"
    );
    host.create(1, &loaded)
        .expect("the external osu! producer reaches READY in a plugin host");
    #[cfg(target_os = "linux")]
    {
        let mapped_by = |pid: &str| {
            std::fs::read_to_string(format!("/proc/{pid}/maps"))
                .is_ok_and(|maps| maps.contains(staged.to_str().unwrap()))
        };
        assert!(!mapped_by("self"), "core mapped the osu! library");
        let children: Vec<String> = std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|pid| pid.bytes().all(|byte| byte.is_ascii_digit()) && mapped_by(pid))
            .collect();
        assert_eq!(children.len(), 1, "{children:?}");
        assert_ne!(children[0], std::process::id().to_string());
    }
    let started = Instant::now();
    host.destroy_all();
    assert!(
        started.elapsed() < DESTROY_GRACE,
        "the osu! plugin host needed a kill"
    );
}
