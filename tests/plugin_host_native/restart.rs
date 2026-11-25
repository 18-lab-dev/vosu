//! M5 C0: supervision of real `vosu plugin-host` children
//! (`docs/v2/plugin-abi.md` §7.6 "Restart", "Destroy and shutdown",
//! "Startup"). One real native backoff chain of real aborts up to the
//! `failed` latch and a manual restart; a stop during the backoff delay;
//! the per-instance teardown seam; and the first-load startup deadline.
//! The window arithmetic itself is covered with injected time by
//! `vosu-core`'s `plugin::process::restart` and `supervisor` tests.

use std::time::{Duration, Instant};

use vosu_core::plugin::PluginInstanceId;
use vosu_core::plugin::process::control::StartupStage;
use vosu_core::plugin::process::supervisor::{FailureReason, ProcessPhase, ProcessStatus};
use vosu_core::plugin::{HostError, LossCause};
use vosu_plugin_api::{FocusChanged, Message, vosu_opcode};

use super::support::artifacts::{Artifact, Staged};
use super::support::fixture::{Fixture, Receipt};
use super::support::host::{Host, Identity, wait_until};

const BOUND: Duration = Duration::from_secs(20);
const FOCUS: u32 = vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32;
const ID: u64 = 1;

fn focus() -> Message {
    Message::FocusChanged(FocusChanged {
        handle: 7,
        focused: true,
        foreground: None,
    })
}

fn status(host: &Host) -> ProcessStatus {
    host.host
        .process_status(PluginInstanceId::new(ID))
        .expect("a supervised process instance")
}

/// Waits for `accept` on instance [`ID`]'s status and returns it.
fn wait_status(
    host: &Host,
    bound: Duration,
    accept: impl Fn(&ProcessStatus) -> bool,
) -> ProcessStatus {
    assert!(
        wait_until(bound, || accept(&status(host))),
        "status stayed {:?}",
        status(host)
    );
    status(host)
}

struct Run {
    host: Host,
    fixture: Fixture,
    _staged: Staged,
    _root: tempfile::TempDir,
}

impl Run {
    fn testkit(fault: &str) -> Self {
        let root = super::support::scratch();
        let staged = Artifact::Testkit.stage(&root.path().join("package"), &[]);
        let fixture = Fixture::new(&root.path().join("receipts"));
        fixture.control(&staged, fault, Some(FOCUS), None);
        let host = Host::new(root.path());
        host.create(ID, &host.load(&staged.dir))
            .expect("the testkit reaches READY");
        Self {
            host,
            fixture,
            _staged: staged,
            _root: root,
        }
    }

    /// The child of generation `generation` (0-based): the one PID with
    /// more `create` receipts than `abort` receipts. Counting per PID keeps
    /// this right when Windows reuses a dead child's PID.
    fn child(&self, generation: usize) -> u32 {
        assert!(
            wait_until(BOUND, || self.fixture.receipts("create").len() > generation),
            "generation {generation} never ran create"
        );
        let creates = self.fixture.receipts("create");
        let aborts = self.fixture.receipts("abort");
        let count = |receipts: &[Receipt], pid: u32| {
            receipts.iter().filter(|receipt| receipt.pid == pid).count()
        };
        let live: Vec<u32> = creates
            .iter()
            .map(|receipt| receipt.pid)
            .filter(|pid| count(&creates, *pid) > count(&aborts, *pid))
            .collect();
        assert!(!live.is_empty(), "no live generation among {creates:?}");
        assert!(
            live.iter().all(|pid| *pid == live[0]),
            "two live generations: {live:?}"
        );
        live[0]
    }
}

/// One real native backoff chain: every generation really aborts
/// (`dispatch_abort`), is retired, and the next starts only after its
/// backoff (1, 2, 4, 8 s); the fifth crash inside the window latches
/// `failed` with no sixth start. A manual restart then resets the window
/// and starts a new generation.
#[test]
fn restart_a_real_abort_chain_backs_off_then_latches_failed() {
    let run = Run::testkit("dispatch_abort");
    let mut previous = None;
    let mut observed = Vec::new();
    for generation in 0..5 {
        let pid = run.child(generation);
        if let Some(died) = previous {
            let delay = Instant::now().duration_since(died);
            observed.push(delay);
        }
        let running = wait_status(&run.host, BOUND, |status| {
            status.phase == ProcessPhase::Running && status.restarts == generation as u64
        });
        assert_eq!(running.crash_events, generation);
        let identity = Identity::of(pid);
        run.host.dispatch(ID, &focus()).unwrap();
        assert!(
            run.fixture
                .wait_for("abort", BOUND, |receipt| receipt.pid == pid)
                .is_some()
        );
        assert!(
            wait_until(BOUND, || !identity.alive()),
            "generation {generation} survived"
        );
        previous = Some(Instant::now());
    }
    // The fifth event inside the window: `failed`, no further start.
    let failed = wait_status(&run.host, BOUND, |status| {
        status.phase == ProcessPhase::Failed
    });
    assert_eq!((failed.crash_events, failed.restarts), (5, 4));
    assert!(matches!(
        failed.last_failure,
        Some(FailureReason::Loss(LossCause::ProcessExited { .. }))
    ));
    println!("RECORD restart delays {observed:?}");
    for (delay, backoff) in observed.iter().zip([1, 2, 4, 8]) {
        let backoff = Duration::from_secs(backoff);
        assert!(
            *delay >= backoff - Duration::from_millis(100)
                && *delay < backoff + Duration::from_secs(5),
            "restart after {delay:?}, backoff {backoff:?}"
        );
    }
    let started = run.fixture.receipts("create").len();
    assert!(!wait_until(Duration::from_secs(3), || run
        .fixture
        .receipts("create")
        .len()
        > started));
    assert_eq!(started, 5, "a latched instance restarted");

    // A manual restart resets the window and the latch and starts a new
    // generation at once.
    run.host
        .host
        .restart_process_instance(PluginInstanceId::new(ID))
        .unwrap();
    let manual = status(&run.host);
    assert_eq!(
        (manual.phase, manual.restarts, manual.crash_events),
        (ProcessPhase::Running, 5, 0)
    );
    assert_eq!(run.fixture.receipts("create").len(), 6);
}

/// A stop during the backoff delay cancels it: nothing respawns, and the
/// teardown seam reports a supervised instance that is not running as done.
#[test]
fn restart_a_teardown_during_the_backoff_never_respawns() {
    let run = Run::testkit("dispatch_abort");
    let pid = run.child(0);
    let identity = Identity::of(pid);
    run.host.dispatch(ID, &focus()).unwrap();
    assert!(wait_until(BOUND, || !identity.alive()));
    wait_status(&run.host, BOUND, |status| {
        matches!(status.phase, ProcessPhase::Backoff { .. })
    });
    run.host
        .host
        .teardown_instance(PluginInstanceId::new(ID))
        .unwrap();
    assert_eq!(status(&run.host).phase, ProcessPhase::Stopped);
    assert!(
        !wait_until(Duration::from_secs(3), || run
            .fixture
            .receipts("create")
            .len()
            > 1),
        "a stopped instance respawned"
    );
}

/// The per-instance teardown seam on a running real child: its `destroy`
/// runs, the child exits and is reaped, it never restarts, and a second
/// instance keeps running.
#[test]
fn restart_teardown_destroys_one_real_child_and_keeps_the_other() {
    let root = super::support::scratch();
    let host = Host::new(root.path());
    let mut runs = Vec::new();
    for (index, name) in ["gone", "kept"].into_iter().enumerate() {
        let id = format!("test.c0.{name}");
        let process = format!("vosu-c0-target-{name}");
        let staged = Artifact::Testkit.stage(
            &root.path().join(format!("package-{name}")),
            &[
                ("test.p0.testkit", id.as_str()),
                ("vosu-p0-target", process.as_str()),
            ],
        );
        let fixture = Fixture::new(&root.path().join(format!("receipts-{name}")));
        fixture.control(&staged, "none", None, None);
        host.create(index as u64 + 1, &host.load(&staged.dir))
            .unwrap();
        let pid = fixture.wait("create", BOUND).unwrap().pid;
        runs.push((fixture, Identity::of(pid), staged));
    }
    host.host
        .teardown_instance(PluginInstanceId::new(1))
        .expect("a clean destroy needs no kill");
    let (gone, gone_identity, _) = &runs[0];
    assert!(gone.wait("destroy", BOUND).is_some(), "destroy never ran");
    assert!(!gone_identity.alive(), "teardown returned before the reap");
    assert!(matches!(
        host.dispatch(1, &focus()),
        Err(HostError::UnknownInstance)
    ));
    assert!(
        !wait_until(Duration::from_secs(2), || gone.receipts("create").len() > 1),
        "a torn-down instance restarted"
    );
    let (kept, kept_identity, _) = &runs[1];
    assert!(kept_identity.alive());
    host.dispatch(2, &focus()).unwrap();
    assert!(kept.wait("dispatch", BOUND).is_some());
}

/// The first start of a library digest in a core run gets the first-load
/// deadline (§7.6 "First load"); once the digest reached `READY`, the next
/// start gets the regular deadline. Shortened deadlines (2 s and 6 s) keep
/// the case fast; the production values are 5 s and 30 s.
#[test]
fn restart_a_never_seen_library_gets_the_first_load_deadline() {
    let regular = Duration::from_secs(2);
    let first_load = Duration::from_secs(6);
    let root = super::support::scratch();
    let host = Host::with_launcher(root.path(), |launcher| {
        launcher.with_deadlines(regular, first_load)
    });
    let stage = |name: &str| {
        let id = format!("test.c0.{name}");
        let staged = Artifact::Testkit.stage(
            &root.path().join(format!("package-{name}")),
            &[("test.p0.testkit", id.as_str())],
        );
        let fixture = Fixture::new(&root.path().join(format!("receipts-{name}")));
        fixture.control(&staged, "create_block", None, None);
        (staged, fixture)
    };

    // First start: `create` is held past the regular deadline, then
    // released, and the start still succeeds.
    let (first, first_fixture) = stage("first");
    let loaded = host.load(&first.dir);
    let started = Instant::now();
    std::thread::scope(|scope| {
        let releaser = scope.spawn(|| {
            let pid = first_fixture.wait("create", BOUND).expect("create ran").pid;
            // Hold `create` past the regular deadline before releasing it.
            std::thread::sleep(regular + Duration::from_millis(500));
            first_fixture.release(pid);
        });
        host.create(1, &loaded)
            .expect("the first load outlived the regular deadline");
        releaser.join().unwrap();
    });
    let elapsed = started.elapsed();
    println!("RECORD first-load start with a held create took {elapsed:?}");
    assert!(elapsed > regular, "{elapsed:?}");
    assert_eq!(
        first_fixture
            .wait("create_released", BOUND)
            .unwrap()
            .get("released"),
        "true"
    );

    // Same digest, now seen: the regular deadline applies.
    let (second, second_fixture) = stage("second");
    let started = Instant::now();
    let error = host.create(2, &host.load(&second.dir)).unwrap_err();
    let elapsed = started.elapsed();
    let HostError::ProcessStartupFailed(failure) = error else {
        panic!("{error:?}");
    };
    assert_eq!(failure.stage, StartupStage::Timeout);
    assert!(
        elapsed >= regular - Duration::from_millis(100) && elapsed < first_load,
        "{elapsed:?}"
    );
    assert!(
        second_fixture.wait("create", BOUND).is_some(),
        "the second start never reached create"
    );
    assert!(second_fixture.receipts("create_released").is_empty());
}
