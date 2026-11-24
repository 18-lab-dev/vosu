//! Focus-rule tests: attach/identify/focus ordering, loss-before-gain on
//! a foreground switch, `foreground_changed()`'s own re-run, process-exit
//! recomputation, `focus_priority`, and concurrent-recomputation safety.
//! Split out of `tests.rs` so that module stays under the maintained-file
//! line limit (`docs/architecture/source-map.md`).

use std::time::Duration;

use vosu_plugin_api::DetachReason;
use vosu_plugin_testkit::session::{OnAttach, SessionEvent, SessionPluginConfig};

use crate::plugin::focus;
use crate::plugin::host::*;

use super::helpers::{
    RECV_TIMEOUT, create_session_instance, test_host, test_host_with, test_process,
};

/// M1 Exit: "`SESSION_ATTACHED` -> `SESSION_IDENTIFY` -> first
/// `FOCUS_CHANGED`" driven end to end through a real
/// [`session::SessionPlugin`] instance, not `SessionRegistry`/
/// `FocusTracker` in isolation.
#[test]
fn attach_then_identify_then_focus_changed() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.basic",
        &["proc1.exe"],
        SessionPluginConfig::default(),
    );

    host.process_appeared(test_process(100, "proc1.exe"));

    let attached = match events.recv_timeout(RECV_TIMEOUT).expect("SESSION_ATTACHED") {
        SessionEvent::Attached(attached) => attached,
        other => panic!("expected Attached, got {other:?}"),
    };
    assert_eq!(attached.pid, 100);

    match events.recv_timeout(RECV_TIMEOUT).expect("FOCUS_CHANGED") {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, attached.handle);
            assert!(focus.focused, "the only identified session must be focused");
        }
        other => panic!("expected Focus, got {other:?}"),
    }
    assert!(events.try_recv().is_err(), "no further events expected");
}

/// `docs/v2/api-v1.md` "Focus rule" delivery ordering: "the session
/// losing focus [is told] before the session gaining it". Forces a
/// real transition — not merely the always-once promise to a newly
/// identified session — by switching the synthetic OS foreground
/// owner to the second process between the two attaches, so the
/// foreground-owner rule itself picks the second session over the
/// first's recency.
#[test]
fn focus_changed_loss_before_gain_on_foreground_switch() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let foreground = Arc::new(focus::SyntheticForegroundSource::new(None));
    let (_log_dir, host) = test_host_with(
        runtime.handle().clone(),
        foreground.clone(),
        crate::plugin::session::IDENTIFICATION_TIMEOUT,
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.switch",
        &["proc1.exe", "proc2.exe"],
        SessionPluginConfig::default(),
    );

    host.process_appeared(test_process(100, "proc1.exe"));
    let h1 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 1") {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    match events.recv_timeout(RECV_TIMEOUT).expect("focus 1") {
        SessionEvent::Focus(focus) => assert!(focus.focused && focus.handle == h1),
        other => panic!("expected Focus, got {other:?}"),
    }

    foreground.set(Some(200));
    host.process_appeared(test_process(200, "proc2.exe"));
    let h2 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 2") {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    match events.recv_timeout(RECV_TIMEOUT).expect("focus loss") {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, h1);
            assert!(!focus.focused, "the first session must lose focus first");
        }
        other => panic!("expected Focus, got {other:?}"),
    }
    match events.recv_timeout(RECV_TIMEOUT).expect("focus gain") {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, h2);
            assert!(focus.focused, "the foreground owner must gain focus second");
        }
        other => panic!("expected Focus, got {other:?}"),
    }
}

/// P2-9: `foreground_changed()` re-runs the Focus rule from the current
/// `ForegroundSource` snapshot without any other session event having to
/// happen first — the entry point phase (d2) wires a real OS
/// foreground-change source to (`mod.rs`'s own scope doc comment).
#[test]
fn foreground_changed_reruns_the_focus_rule() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let foreground = Arc::new(focus::SyntheticForegroundSource::new(None));
    let (_log_dir, host) = test_host_with(
        runtime.handle().clone(),
        foreground.clone(),
        crate::plugin::session::IDENTIFICATION_TIMEOUT,
    );
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.foregroundchanged",
        &["proc1.exe", "proc2.exe"],
        SessionPluginConfig::default(),
    );

    host.process_appeared(test_process(100, "proc1.exe"));
    let h1 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 1") {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    events.recv_timeout(RECV_TIMEOUT).expect("focus 1");

    host.process_appeared(test_process(200, "proc2.exe"));
    let h2 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 2") {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    // h1 is already the most-recently-focused session, and nothing owns
    // the foreground yet, so the recency fallback keeps it focused; h2
    // still gets its own always-sent first `FOCUS_CHANGED` (focused:
    // false).
    match events.recv_timeout(RECV_TIMEOUT).expect("focus 2") {
        SessionEvent::Focus(focus) => assert!(!focus.focused && focus.handle == h2),
        other => panic!("expected Focus, got {other:?}"),
    }

    // The OS foreground window switches to pid 200's process — no new
    // `SESSION_ATTACHED`/`SESSION_IDENTIFY` happens, so only
    // `foreground_changed()` itself can notice and re-run the rule.
    foreground.set(Some(200));
    host.foreground_changed();

    match events.recv_timeout(RECV_TIMEOUT).expect("focus loss") {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, h1);
            assert!(!focus.focused, "h1 loses focus first (loss before gain)");
        }
        other => panic!("expected Focus, got {other:?}"),
    }
    match events.recv_timeout(RECV_TIMEOUT).expect("focus gain") {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, h2);
            assert!(focus.focused, "the new foreground owner gains focus second");
        }
        other => panic!("expected Focus, got {other:?}"),
    }
}

/// §6.2: `focus_priority` is held by the first session that requests
/// it, and a second request while it is held is ignored (logged, not
/// refused): the second session's own first `FOCUS_CHANGED` still
/// arrives (always sent once), but reports `focused: false` since the
/// holder keeps focus.
#[test]
fn focus_priority_hold_and_ignore() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.priority",
        &["proc1.exe", "proc2.exe"],
        SessionPluginConfig {
            focus_priority: true,
            on_attach: OnAttach::Identify,
            ..Default::default()
        },
    );

    host.process_appeared(test_process(100, "proc1.exe"));
    let h1 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 1") {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    match events.recv_timeout(RECV_TIMEOUT).expect("focus 1") {
        SessionEvent::Focus(focus) => assert!(focus.focused && focus.handle == h1),
        other => panic!("expected Focus, got {other:?}"),
    }

    host.process_appeared(test_process(200, "proc2.exe"));
    let h2 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 2") {
        SessionEvent::Attached(attached) => attached.handle,
        other => panic!("expected Attached, got {other:?}"),
    };
    match events.recv_timeout(RECV_TIMEOUT).expect("focus 2") {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, h2);
            assert!(
                !focus.focused,
                "the priority holder must keep focus; the second requester is ignored, not granted"
            );
        }
        other => panic!("expected Focus, got {other:?}"),
    }
    assert_eq!(
        host.focus
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .focused(),
        Some(FocusKey(h1)),
        "focus_priority's holder keeps focus across a second ignored request"
    );
}

/// Phase (d2)'s other entry point: `process_exited` detaches whatever
/// session that pid held and recomputes focus, same as any other
/// detach path.
#[test]
fn process_exited_detaches_and_recomputes_focus() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.exit",
        &["proc1.exe", "proc2.exe"],
        SessionPluginConfig::default(),
    );

    host.process_appeared(test_process(100, "proc1.exe"));
    let h1 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 1") {
        SessionEvent::Attached(attached) => SessionHandle(attached.handle),
        other => panic!("expected Attached, got {other:?}"),
    };
    events.recv_timeout(RECV_TIMEOUT).expect("focus 1");

    host.process_appeared(test_process(200, "proc2.exe"));
    let h2 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 2") {
        SessionEvent::Attached(attached) => SessionHandle(attached.handle),
        other => panic!("expected Attached, got {other:?}"),
    };
    events
        .recv_timeout(RECV_TIMEOUT)
        .expect("focus 2 (own first FOCUS_CHANGED)");

    host.process_exited(100);
    match events.recv_timeout(RECV_TIMEOUT).expect("detach on exit") {
        SessionEvent::Detached(detached) => {
            assert_eq!(detached.handle, h1.0);
            assert_eq!(detached.reason, DetachReason::Exited);
        }
        other => panic!("expected Detached, got {other:?}"),
    }
    match events
        .recv_timeout(RECV_TIMEOUT)
        .expect("focus moves to h2 once h1 exits")
    {
        SessionEvent::Focus(focus) => {
            assert_eq!(focus.handle, h2.0);
            assert!(
                focus.focused,
                "h2 is the only remaining session after h1 exits"
            );
        }
        other => panic!("expected Focus, got {other:?}"),
    }

    assert!(
        !host
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_identified(h1),
        "the exited pid's session must no longer be tracked"
    );

    // Re-appearing the same pid before `process_exited` would have
    // been refused (`SessionRegistry::attach`'s own pid-dedup rule);
    // after it, the pid is free again — the clearest externally
    // observable proof `forget_exited_pid` actually ran.
    host.process_appeared(test_process(100, "proc1.exe"));
    match events
        .recv_timeout(RECV_TIMEOUT)
        .expect("re-attach after exit")
    {
        SessionEvent::Attached(attached) => assert_eq!(attached.pid, 100),
        other => panic!("expected Attached, got {other:?}"),
    }

    let _ = h2;
}

/// P2-6: `focus` is locked before `sessions`/`matcher`, and held through
/// delivery, in every focus-recomputing call site — the one lock order
/// this host ever uses for them. Hammering `recompute_focus`/
/// `identify_focus` concurrently from several threads, while the main
/// thread keeps attaching, identifying, and exiting sessions, must
/// neither deadlock nor panic.
///
/// F6: a hang is detected through a bounded channel receive, not
/// `JoinHandle::join` (which has no timeout of its own and would hang
/// this test's own thread, relying only on the test harness's process-
/// level timeout to ever notice) — each hammerer signals its own
/// completion over `done_tx` before this test ever calls `join` on it,
/// so a genuine deadlock fails with a clear assertion message instead of
/// hanging silently.
/// N5: the event-order check itself needs a handle that genuinely gains
/// and then loses focus while still a live candidate — not through its
/// own detach, which sends no loss event for the handle that just left
/// (`FocusTracker::recompute`'s own doc comment) — or "two consecutive
/// events with the same `focused`" has nothing real to compare, and the
/// check trivially passes no matter what. Under 8 threads hammering
/// `identify_focus(h1)` 1,600 times total, `h1`'s own recency entry
/// never moves (`FocusTracker::decide`'s recency fallback always finds
/// it first, since nothing ever makes it stop being the decided focus),
/// so a session 2 handle attached/exited during that storm gets exactly
/// one mandatory `focused: false` and nothing to pair it with — the
/// previous version of this test checked such pairs and so could never
/// actually fail. The deadlock-proving phase below keeps that storm
/// (and discards its own events, for the same reason). A second,
/// deterministic phase then drives a real gain-then-loss cycle through
/// the OS foreground instead of recency, on its own freshly built host
/// and instance — not phase one's, whose per-instance dispatch queue
/// could still be draining a deep backlog from the hammering/20-round
/// storm above under heavy load, which would make a bound on how long
/// this phase's own events take to arrive meaningless.
#[test]
fn concurrent_focus_recomputation_never_deadlocks() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.session.focusrace",
        &["proc-focusrace-1.exe", "proc-focusrace-2.exe"],
        SessionPluginConfig::default(),
    );

    host.process_appeared(test_process(900, "proc-focusrace-1.exe"));
    let h1 = match events.recv_timeout(RECV_TIMEOUT).expect("attach 1") {
        SessionEvent::Attached(attached) => SessionHandle(attached.handle),
        other => panic!("expected Attached, got {other:?}"),
    };
    events.recv_timeout(RECV_TIMEOUT).expect("focus 1");

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let hammerers: Vec<_> = (0..8)
        .map(|_| {
            let host = Arc::clone(&host);
            let done_tx = done_tx.clone();
            std::thread::spawn(move || {
                for _ in 0..200 {
                    host.recompute_focus();
                    host.identify_focus(h1);
                }
                let _ = done_tx.send(());
            })
        })
        .collect();
    drop(done_tx);

    // Concurrently attach, re-identify (ignored — already identified, but
    // still exercises `identify_focus`'s own callers racing the
    // hammerers above), and exit a second session several times over —
    // purely to prove the absence of a deadlock (this phase's own doc
    // comment above explains why its events are not worth checking).
    for round in 0..20u32 {
        let pid = 901 + round;
        host.process_appeared(test_process(pid, "proc-focusrace-2.exe"));
        host.process_exited(pid);
    }

    for _ in &hammerers {
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a hammerer thread did not finish within 10s — possible deadlock");
    }
    for hammerer in hammerers {
        vosu_proptest::wait::JoinBounded::join_bounded(hammerer)
            .expect("a hammerer thread must not panic");
    }

    // The deterministic phase: a fresh host/instance of its own (this
    // test's own doc comment explains why), so every `recv_until` below
    // is immune to anything still working through phase one's own
    // per-instance dispatch queue.
    fn recv_until(
        events: &std::sync::mpsc::Receiver<SessionEvent>,
        label: &str,
        mut matches: impl FnMut(&SessionEvent) -> bool,
    ) -> SessionEvent {
        loop {
            let event = events
                .recv_timeout(RECV_TIMEOUT)
                .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
            if matches(&event) {
                return event;
            }
        }
    }

    let foreground = Arc::new(focus::SyntheticForegroundSource::new(None));
    let (_log_dir2, host2) = test_host_with(
        runtime.handle().clone(),
        foreground.clone(),
        crate::plugin::session::IDENTIFICATION_TIMEOUT,
    );
    let events2 = create_session_instance(
        &host2,
        PluginInstanceId::new(1),
        "testkit.session.focuspair",
        &["proc-focuspair-1.exe", "proc-focuspair-2.exe"],
        SessionPluginConfig::default(),
    );

    host2.process_appeared(test_process(2_000, "proc-focuspair-1.exe"));
    let p1 = match events2.recv_timeout(RECV_TIMEOUT).expect("attach 1") {
        SessionEvent::Attached(attached) => SessionHandle(attached.handle),
        other => panic!("expected Attached, got {other:?}"),
    };
    events2.recv_timeout(RECV_TIMEOUT).expect("focus 1");

    let mut history: std::collections::HashMap<u64, Vec<bool>> = std::collections::HashMap::new();
    for round in 0..5u32 {
        let pid = 2_001 + round;
        // Session 2 takes the OS foreground, which outranks recency:
        // its own identify must now genuinely win focus away from `p1`.
        foreground.set(Some(pid));
        host2.process_appeared(test_process(pid, "proc-focuspair-2.exe"));
        let p2 = match recv_until(
            &events2,
            "attach 2",
            |event| matches!(event, SessionEvent::Attached(attached) if attached.pid == pid),
        ) {
            SessionEvent::Attached(attached) => attached.handle,
            _ => unreachable!("filtered above"),
        };
        match recv_until(
            &events2,
            "p1 loses focus",
            |event| matches!(event, SessionEvent::Focus(focus) if focus.handle == p1.0 && !focus.focused),
        ) {
            SessionEvent::Focus(_) => {}
            _ => unreachable!("filtered above"),
        }
        match recv_until(
            &events2,
            "p2 gains focus",
            |event| matches!(event, SessionEvent::Focus(focus) if focus.handle == p2),
        ) {
            SessionEvent::Focus(focus) => {
                assert!(focus.focused, "p2's own first Focus event must be a gain");
                history.entry(p2).or_default().push(true);
            }
            _ => unreachable!("filtered above"),
        }

        // Give the foreground back to `p1` while session 2 is still a
        // live candidate (not yet detached), forcing a real loss event
        // for session 2's own handle — the pair this test actually
        // needs.
        foreground.set(Some(2_000));
        host2.recompute_focus();
        match recv_until(
            &events2,
            "p2 loses focus",
            |event| matches!(event, SessionEvent::Focus(focus) if focus.handle == p2),
        ) {
            SessionEvent::Focus(focus) => {
                assert!(!focus.focused, "p2's own second Focus event must be a loss");
                history.entry(p2).or_default().push(false);
            }
            _ => unreachable!("filtered above"),
        }
        match recv_until(
            &events2,
            "p1 regains focus",
            |event| matches!(event, SessionEvent::Focus(focus) if focus.handle == p1.0 && focus.focused),
        ) {
            SessionEvent::Focus(_) => {}
            _ => unreachable!("filtered above"),
        }

        host2.process_exited(pid);
        match recv_until(
            &events2,
            "detach 2",
            |event| matches!(event, SessionEvent::Detached(detached) if detached.handle == p2),
        ) {
            SessionEvent::Detached(_) => {}
            _ => unreachable!("filtered above"),
        }
    }

    // Every session-2 handle this deterministic phase drove through a
    // real gain-then-loss cycle must show exactly that alternating pair
    // — the check an out-of-order or duplicated delivery could actually
    // trip, unlike the previous version's vacuous single-event case.
    assert_eq!(
        history.len(),
        5,
        "every round must have produced a real gain/loss pair"
    );
    for (handle, states) in &history {
        assert_eq!(
            states.as_slice(),
            [true, false],
            "handle {handle} must gain then lose focus exactly once"
        );
    }
}
