//! M5 C0: real loss of a `vosu plugin-host` child, end to end
//! (`docs/v2/plugin-abi.md` §7.2 "Pressure, loss and restart", §7.6 "Loss
//! identity", "Restart" and the A5.4 action grace). Each case drives a real
//! child to one loss cause: an OS kill, a stall that fills core's ring, an
//! ignored cancellation past its grace, and a real abort. Each asserts the
//! live state before the loss, the recorded cause, the cleanup (jobs,
//! sessions, the reaped child), the restart, and a second plugin that stays
//! healthy. The grace and stall arithmetic itself is covered by
//! `vosu-core`'s `host::actions::tests::grace` and `launcher::tests`.
//!
//! [`j3`] (M5 X0) closes J3: one loss with a session, a pending plan, a
//! stream read, a job and relayed calls in both directions all live.

#[path = "crash/j3.rs"]
mod j3;

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use vosu_core::actions::{
    ActionRegistry as _, InvocationId, InvocationPolicy, InvocationSink, Principal, TerminalOutcome,
};
use vosu_core::plugin::process::supervisor::{FailureReason, ProcessPhase, ProcessStatus};
use vosu_core::plugin::{LossCause, PluginInstanceId};
use vosu_core::state::SessionKey;
use vosu_plugin_api::{
    ActionInvoke, ActionProgress, Caller, CallerKind, CborValue, FocusChanged, Message, vosu_opcode,
};

use super::support::artifacts::{Artifact, Staged};
use super::support::fixture::{Fixture, Receipt};
use super::support::host::{Host, Identity, wait_until};

const BOUND: Duration = Duration::from_secs(20);
const FOCUS: u32 = vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32;
const INVOKE: u32 = vosu_opcode::VOSU_OP_ACTION_INVOKE as u32;
/// A job that must stay running for the whole case.
const LONG: InvocationPolicy = InvocationPolicy {
    deadline: Duration::from_secs(120),
    grace: Duration::from_secs(5),
};

fn focus() -> Message {
    Message::FocusChanged(FocusChanged {
        handle: 7,
        focused: true,
        foreground: None,
    })
}

/// Collects one invocation's terminal outcome.
struct Sink(mpsc::SyncSender<TerminalOutcome>);

impl InvocationSink for Sink {
    fn progress(&self, _: ActionProgress) {}
    fn finish(&self, result: TerminalOutcome) {
        // The receiver is gone once the case ended; a second terminal is
        // caught by the case itself through `try_recv`.
        let _ = self.0.try_send(result);
    }
}

fn failure(outcome: TerminalOutcome) -> String {
    match outcome {
        TerminalOutcome::Failed(failure) => failure.code,
        other => panic!("expected a failure, got {other:?}"),
    }
}

/// One instance of the testkit under its own plugin id, process name and
/// receipt directory.
struct Plugin {
    id: u64,
    name: String,
    process: String,
    fixture: Fixture,
    _staged: Staged,
}

/// One host with several testkit instances.
struct Run {
    // Field order is drop order: the host (and its children) go first.
    host: Host,
    plugins: Vec<Plugin>,
    _root: tempfile::TempDir,
}

impl Run {
    /// Creates one instance per `(name, fault, trigger)`, with ids 1, 2, …
    fn new(plugins: &[(&str, &str, Option<u32>)]) -> Self {
        let root = super::support::scratch();
        let host = Host::new(root.path());
        let plugins = plugins
            .iter()
            .enumerate()
            .map(|(index, (name, fault, trigger))| {
                let plugin_id = format!("test.c0.crash.{name}");
                let process = format!("vosu-c0-crash-{name}");
                let staged = Artifact::Testkit.stage(
                    &root.path().join(format!("package-{name}")),
                    &[
                        ("test.p0.testkit", plugin_id.as_str()),
                        ("vosu-p0-target", process.as_str()),
                    ],
                );
                let fixture = Fixture::new(&root.path().join(format!("receipts-{name}")));
                fixture.control(&staged, fault, *trigger, None);
                let id = index as u64 + 1;
                host.create(id, &host.load(&staged.dir))
                    .unwrap_or_else(|error| panic!("{plugin_id} did not reach READY: {error}"));
                Plugin {
                    id,
                    name: plugin_id,
                    process,
                    fixture,
                    _staged: staged,
                }
            })
            .collect();
        Self {
            host,
            plugins,
            _root: root,
        }
    }

    fn plugin(&self, index: usize) -> &Plugin {
        &self.plugins[index]
    }

    fn status(&self, index: usize) -> ProcessStatus {
        self.host
            .host
            .process_status(PluginInstanceId::new(self.plugin(index).id))
            .expect("a supervised process instance")
    }

    /// Waits for `accept` on instance `index`'s status and returns it.
    fn wait_status(&self, index: usize, accept: impl Fn(&ProcessStatus) -> bool) -> ProcessStatus {
        assert!(
            wait_until(BOUND, || accept(&self.status(index))),
            "{} status stayed {:?}",
            self.plugin(index).name,
            self.status(index)
        );
        self.status(index)
    }

    /// The child PID of instance `index`'s first generation.
    fn child(&self, index: usize) -> u32 {
        let pid = self
            .plugin(index)
            .fixture
            .wait("create", BOUND)
            .unwrap_or_else(|| panic!("{} never ran create", self.plugin(index).name))
            .pid;
        assert_ne!(pid, std::process::id(), "create ran in core");
        pid
    }

    /// Invokes instance `index`'s `hold` action as the broker.
    fn invoke(
        &self,
        index: usize,
        policy: InvocationPolicy,
    ) -> (InvocationId, mpsc::Receiver<TerminalOutcome>) {
        let action = format!("{}.hold", self.plugin(index).name);
        let descriptor = self
            .host
            .host
            .get(&action)
            .unwrap_or_else(|| panic!("{action} is not registered"));
        let (sender, outcome) = mpsc::sync_channel(4);
        let _enter = self.host.enter();
        let id = self
            .host
            .host
            .invocation_executor_with_policy(policy)
            .invoke(
                Principal::Broker,
                descriptor,
                None,
                serde_json::json!({}),
                Arc::new(Sink(sender)),
            )
            .unwrap();
        (id, outcome)
    }

    /// Waits for `pid`'s `invoke` receipt of `invocation`.
    fn invoked(&self, index: usize, pid: u32, invocation: InvocationId) -> Receipt {
        self.plugin(index)
            .fixture
            .wait_for("invoke", BOUND, |receipt| {
                receipt.pid == pid && receipt.get("invocation") == invocation.to_string()
            })
            .unwrap_or_else(|| panic!("{pid} never received invocation {invocation}"))
    }

    /// Offers process `target` to instance `index` and returns the session
    /// handle child `pid` was attached with. The session must be live in
    /// the state store: `identify` succeeded.
    fn attach(&self, index: usize, pid: u32, target: u32) -> u64 {
        let plugin = self.plugin(index);
        self.host
            .offer_target(target, &plugin.process, target.to_le_bytes().to_vec());
        self.attach_receipt(index, pid, target)
    }

    fn attach_receipt(&self, index: usize, pid: u32, target: u32) -> u64 {
        let attach = self
            .plugin(index)
            .fixture
            .wait_for("attach", BOUND, |receipt| receipt.pid == pid)
            .unwrap_or_else(|| panic!("{pid} was never attached"));
        assert_eq!(attach.get("target"), target.to_string());
        assert_eq!(attach.get("identify"), "0", "identify was refused");
        let handle: u64 = attach.get("handle").parse().unwrap();
        assert!(
            self.session_live(handle),
            "session {handle} is not in the state store"
        );
        handle
    }

    fn session_live(&self, handle: u64) -> bool {
        self.host
            .host
            .state()
            .store
            .current(SessionKey(handle))
            .is_some()
    }

    /// The control instance after another instance's loss: still the same
    /// live child, never lost, and answering a new job.
    fn assert_healthy(&self, index: usize, pid: u32, identity: &Identity) {
        assert!(identity.alive(), "the second plugin's child died");
        let status = self.status(index);
        assert!(
            status.phase == ProcessPhase::Running
                && status.restarts == 0
                && status.crash_events == 0
                && status.last_failure.is_none(),
            "{status:?}"
        );
        let (invocation, outcome) = self.invoke(index, LONG);
        assert!(
            matches!(
                outcome.recv_timeout(BOUND),
                Ok(TerminalOutcome::Succeeded(_))
            ),
            "the second plugin stopped answering"
        );
        self.invoked(index, pid, invocation);
        assert_eq!(self.plugin(index).fixture.receipts("create").len(), 1);
    }

    /// The lost generation (`lost`, already reaped) never ran `destroy`,
    /// and the next generation started under the same instance id in a new
    /// live child. Receipt files are read in directory order, not start
    /// order, so the new child is the second `create`'s other PID, or the
    /// same PID when Windows reused it.
    fn assert_restarted(&self, index: usize, lost: u32, lost_identity: &Identity) -> u32 {
        let status = self.wait_status(index, |status| {
            status.phase == ProcessPhase::Running && status.restarts == 1
        });
        assert_eq!(status.crash_events, 1, "{status:?}");
        let fixture = &self.plugin(index).fixture;
        assert!(
            wait_until(BOUND, || fixture.receipts("create").len() == 2),
            "the next generation never ran create"
        );
        let next = fixture
            .receipts("create")
            .iter()
            .map(|receipt| receipt.pid)
            .find(|pid| *pid != lost)
            .unwrap_or(lost);
        assert!(!lost_identity.alive(), "the lost child is still running");
        assert!(
            Identity::of(next).alive(),
            "the next generation is not live"
        );
        assert!(fixture.receipts("destroy").is_empty(), "a loss ran destroy");
        next
    }
}

/// The exit status an OS-killed child reports: `-SIGKILL` on Linux, the
/// code passed to `TerminateProcess` on Windows.
#[cfg(target_os = "linux")]
const KILLED: i64 = -9;
#[cfg(windows)]
const KILLED: i64 = 0xDEAD;

/// Kills `pid` the way the OS or a user would, outside core.
#[cfg(target_os = "linux")]
fn kill(pid: u32) {
    // SAFETY: signals our own plugin host, which is still unreaped: its
    // identity was observed alive and core has not seen it exit.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) }, 0);
}

#[cfg(windows)]
fn kill(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    // SAFETY: plain call; a null result is checked before use.
    let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    assert!(!process.is_null(), "opening {pid}");
    // SAFETY: `process` is the live handle opened above with terminate
    // access, closed right after.
    unsafe {
        assert_ne!(
            TerminateProcess(process, KILLED as u32),
            0,
            "terminating {pid}"
        );
        CloseHandle(process);
    }
}

/// An OS kill of a child with a live job and session is one `exited` loss:
/// the job fails `plugin_crashed`, the session detaches, the instance
/// restarts after its 1-second backoff, and a second plugin is untouched.
#[test]
fn crash_an_os_kill_is_one_loss_that_restarts_after_the_backoff() {
    let run = Run::new(&[("killed", "cancel_ignored", None), ("kept", "none", None)]);
    let (killed, kept) = (run.child(0), run.child(1));
    let (killed_identity, kept_identity) = (Identity::of(killed), Identity::of(kept));
    let session = run.attach(0, killed, 0x7FFF_C0A1);
    let kept_session = run.attach(1, kept, 0x7FFF_C0A2);
    let (invocation, job) = run.invoke(0, LONG);
    run.invoked(0, killed, invocation);
    assert_eq!(run.status(0).phase, ProcessPhase::Running);

    kill(killed);
    assert!(
        wait_until(BOUND, || !killed_identity.alive()),
        "the killed child was never reaped"
    );
    let died = Instant::now();
    assert_eq!(failure(job.recv_timeout(BOUND).unwrap()), "plugin_crashed");
    assert!(
        wait_until(BOUND, || !run.session_live(session)),
        "the killed child's session never detached"
    );
    let backoff = run.wait_status(0, |status| {
        matches!(status.phase, ProcessPhase::Backoff { .. }) || status.restarts == 1
    });
    let cause = LossCause::ProcessExited {
        status: Some(KILLED),
    };
    assert_eq!(
        backoff.last_failure,
        Some(FailureReason::Loss(cause.clone()))
    );
    assert_eq!(cause.reason(), "exited");

    run.assert_restarted(0, killed, &killed_identity);
    let delay = died.elapsed();
    println!("RECORD os kill: restart within {delay:?} of the reap");
    assert!(
        delay >= Duration::from_millis(900),
        "restarted after {delay:?}, before the 1 s backoff"
    );
    assert!(job.try_recv().is_err(), "the job finished twice");
    assert!(run.session_live(kept_session));
    run.assert_healthy(1, kept, &kept_identity);
}

/// A child that stops reading its ring while core keeps writing: the
/// write that finds no space for the whole 2-second backpressure window is
/// one `pressure_timeout` loss. Core kills and reaps the stopped child and
/// restarts it; a second plugin is untouched.
#[test]
fn crash_a_ring_stall_that_fills_the_ring_is_one_pressure_timeout() {
    let run = Run::new(&[
        ("stalled", "ring_stall", Some(FOCUS)),
        ("kept", "none", None),
    ]);
    let (stalled, kept) = (run.child(0), run.child(1));
    let (stalled_identity, kept_identity) = (Identity::of(stalled), Identity::of(kept));
    run.host.dispatch(1, &focus()).unwrap();
    assert!(run.plugin(0).fixture.wait("stall", BOUND).is_some());
    assert!(
        wait_until(BOUND, || stalled_identity.stopped()),
        "the child never stopped"
    );

    // Real core writes into the real 2 MiB ring: two 768 KiB frames fit,
    // the third waits for space the stopped child never frees.
    let _enter = run.host.enter();
    let started = Instant::now();
    let writes: Vec<_> = (0..3)
        .map(|invocation| {
            run.host
                .host
                .dispatch_to_instance(
                    PluginInstanceId::new(1),
                    &Message::ActionInvoke(ActionInvoke {
                        invocation: 9_000 + invocation,
                        action: "test.c0.crash.stalled.hold".to_owned(),
                        session: None,
                        caller: Caller {
                            kind: CallerKind::Tool,
                            id: "c0-stall".to_owned(),
                        },
                        args: CborValue::Bytes(vec![0xC0; 768 * 1024]),
                    }),
                    0,
                )
                .expect("the stalled instance is still live")
        })
        .collect();
    let failed = run.wait_status(0, |status| status.last_failure.is_some());
    let elapsed = started.elapsed();
    println!("RECORD ring stall: pressure loss after {elapsed:?}");
    assert_eq!(
        failed.last_failure,
        Some(FailureReason::Loss(LossCause::Stalled))
    );
    assert_eq!(LossCause::Stalled.reason(), "pressure_timeout");
    assert!(
        elapsed >= Duration::from_millis(1_900),
        "lost after {elapsed:?}, inside the 2 s backpressure window"
    );
    let statuses: Vec<_> = writes
        .into_iter()
        .map(|reply| reply.recv_timeout(BOUND).unwrap())
        .collect();
    assert_eq!(
        statuses,
        [
            vosu_plugin_api::vosu_status::VOSU_OK,
            vosu_plugin_api::vosu_status::VOSU_OK,
            vosu_plugin_api::vosu_status::VOSU_ERR_NOT_READY,
        ]
    );
    assert!(
        wait_until(BOUND, || !stalled_identity.alive()),
        "the stalled child was not killed and reaped"
    );
    run.assert_restarted(0, stalled, &stalled_identity);
    run.assert_healthy(1, kept, &kept_identity);
}

/// A5.4 on real children: a timed-out job's callee ignores the
/// `ACTION_CANCEL` it gets, so core kills it when the grace ends. The
/// timed-out job stays `timed_out`, the callee's other running job fails
/// `plugin_crashed`, and another plugin's running job and child are
/// untouched.
#[test]
fn crash_an_ignored_cancel_is_killed_when_the_grace_expires() {
    let grace = Duration::from_millis(1_500);
    let run = Run::new(&[
        ("callee", "cancel_ignored", None),
        ("unrelated", "cancel_ignored", None),
    ]);
    let (callee, unrelated) = (run.child(0), run.child(1));
    let (callee_identity, unrelated_identity) = (Identity::of(callee), Identity::of(unrelated));
    let (collateral_id, collateral) = run.invoke(0, LONG);
    let (unrelated_id, unrelated_job) = run.invoke(1, LONG);
    let (timed_out_id, timed_out) = run.invoke(
        0,
        InvocationPolicy {
            deadline: Duration::from_millis(300),
            grace,
        },
    );
    for (index, pid, invocation) in [
        (0, callee, collateral_id),
        (1, unrelated, unrelated_id),
        (0, callee, timed_out_id),
    ] {
        run.invoked(index, pid, invocation);
    }

    assert_eq!(failure(timed_out.recv_timeout(BOUND).unwrap()), "timed_out");
    let deadline = Instant::now();
    let cancel = run
        .plugin(0)
        .fixture
        .wait("cancel", BOUND)
        .expect("ACTION_CANCEL reached the callee");
    assert_eq!(
        (cancel.pid, cancel.get("invocation"), cancel.get("ignored")),
        (callee, timed_out_id.to_string().as_str(), "true")
    );

    assert_eq!(
        failure(collateral.recv_timeout(BOUND).unwrap()),
        "plugin_crashed"
    );
    let killed = deadline.elapsed();
    println!("RECORD grace kill {killed:?} after the deadline (grace {grace:?})");
    assert!(
        killed >= grace - Duration::from_millis(200),
        "killed {killed:?} after the deadline, inside the grace"
    );
    assert!(
        wait_until(BOUND, || !callee_identity.alive()),
        "the callee was not killed and reaped"
    );
    let status = run.wait_status(0, |status| status.last_failure.is_some());
    assert_eq!(
        status.last_failure,
        Some(FailureReason::Loss(LossCause::ActionGraceExpired))
    );
    assert_eq!(
        LossCause::ActionGraceExpired.reason(),
        "action_grace_expired"
    );
    assert!(
        timed_out.try_recv().is_err(),
        "the timed-out job changed after its timeout"
    );
    assert_eq!(
        run.plugin(0).fixture.receipts("cancel").len(),
        1,
        "only the timed-out job was cancelled"
    );
    run.assert_restarted(0, callee, &callee_identity);

    // The other plugin: its job still runs, its child was never cancelled
    // or replaced.
    assert!(unrelated_identity.alive());
    assert!(unrelated_job.try_recv().is_err(), "the unrelated job ended");
    assert!(run.plugin(1).fixture.receipts("cancel").is_empty());
    let other = run.status(1);
    assert_eq!(
        (other.phase, other.restarts, other.crash_events),
        (ProcessPhase::Running, 0, 0)
    );
}

/// A real abort (`std::process::abort`) inside the delivery of a live job,
/// with a live session: the job fails `plugin_crashed`, the session
/// detaches and its state is closed, the next generation attaches to the
/// same target under a new handle, and a second plugin's child, session and
/// jobs are untouched.
#[test]
fn crash_a_real_abort_with_a_live_job_and_session_detaches_only_its_own() {
    let run = Run::new(&[
        ("aborted", "dispatch_abort", Some(INVOKE)),
        ("healthy", "none", None),
    ]);
    let (aborted, healthy) = (run.child(0), run.child(1));
    let (aborted_identity, healthy_identity) = (Identity::of(aborted), Identity::of(healthy));
    let target = 0x7FFF_C0B1;
    let session = run.attach(0, aborted, target);
    let healthy_session = run.attach(1, healthy, 0x7FFF_C0B2);
    assert!(run.plugin(0).fixture.wait("read", BOUND).is_some());

    // The job's own delivery aborts the child before it answers.
    let (_, job) = run.invoke(0, LONG);
    let abort = run
        .plugin(0)
        .fixture
        .wait("abort", BOUND)
        .expect("the child aborted");
    assert_eq!(
        (abort.pid, abort.get("opcode")),
        (aborted, INVOKE.to_string().as_str())
    );
    let aborted_at = Instant::now();
    assert!(
        wait_until(BOUND, || !aborted_identity.alive()),
        "the aborted child survived"
    );
    println!(
        "RECORD dispatch_abort reaped {:?} after the abort",
        aborted_at.elapsed()
    );
    assert_eq!(failure(job.recv_timeout(BOUND).unwrap()), "plugin_crashed");
    assert!(
        wait_until(BOUND, || !run.session_live(session)),
        "the aborted child's session never detached"
    );
    let status = run.wait_status(0, |status| status.last_failure.is_some());
    let Some(FailureReason::Loss(cause @ LossCause::ProcessExited { status: exit })) =
        status.last_failure.clone()
    else {
        panic!("{status:?}");
    };
    println!("RECORD dispatch_abort exit status {exit:?}");
    assert_eq!(cause.reason(), "exited");
    #[cfg(target_os = "linux")]
    assert_eq!(exit, Some(-6), "not SIGABRT");
    #[cfg(windows)]
    assert_eq!(exit, Some(0xC000_0409), "not the fail-fast status");

    // The next generation can attach to the same target afresh: the lost
    // generation's session is gone, not still owned.
    let next = run.assert_restarted(0, aborted, &aborted_identity);
    run.host.offer_process(
        target,
        &run.plugin(0).process,
        vosu_plugin_api::Bitness::X64,
    );
    let reattached = run.attach_receipt(0, next, target);
    assert_ne!(reattached, session, "the lost session handle was reused");

    // The second plugin kept its child, its session and its state.
    assert!(run.session_live(healthy_session));
    assert!(run.plugin(1).fixture.receipts("detach").is_empty());
    run.assert_healthy(1, healthy, &healthy_identity);
}
