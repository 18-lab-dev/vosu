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
/// hanging silently. F6 also checks event order: every `FOCUS_CHANGED`
/// this test observes, grouped by handle, must strictly alternate
/// `focused: true`/`false` — two of the same state in a row for one
/// handle would mean a concurrent recomputation delivered (or skipped)
/// an event out of order, the exact race P2-6's `focus`-held-through-
/// delivery discipline exists to prevent.
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
    // hammerers above), and exit a second session several times over.
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
        hammerer.join().expect("a hammerer thread must not panic");
    }

    // Every `FOCUS_CHANGED` the attach/exit rounds above produced for
    // session 2's own handles (a fresh one each round), grouped by
    // handle, must strictly alternate focused/unfocused — proving
    // delivery never duplicated or reordered an event for the same
    // handle under concurrent recomputation. `h1` is excluded: the
    // hammerers call `identify_focus(h1)` directly, bypassing the normal
    // `SESSION_IDENTIFY` `AlreadyIdentified` refusal, and
    // `on_session_identified` always sends one `FOCUS_CHANGED` per call
    // "even if focus does not change" (its own doc comment) — so h1
    // legitimately receives repeated `focused: true` here, by this
    // test's own deliberate construction, not a reordering bug.
    let mut last_focused: std::collections::HashMap<u64, bool> = std::collections::HashMap::new();
    while let Ok(event) = events.try_recv() {
        if let SessionEvent::Focus(focus) = event
            && focus.handle != h1.0
        {
            if let Some(&previous) = last_focused.get(&focus.handle) {
                assert_ne!(
                    previous, focus.focused,
                    "handle {} received two consecutive FOCUS_CHANGED with the same \
                     focused={} — an event was duplicated or delivered out of order",
                    focus.handle, focus.focused
                );
            }
            last_focused.insert(focus.handle, focus.focused);
        }
    }
}
