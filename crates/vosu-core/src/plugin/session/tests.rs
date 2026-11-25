use super::*;

fn discovered(pid: u32) -> DiscoveredProcess {
    DiscoveredProcess {
        pid,
        bitness: Bitness::X64,
        name: format!("p{pid}.exe"),
        path: format!("/games/p{pid}.exe"),
        command_line: format!("p{pid}.exe"),
        argv: None,
    }
}

fn process() -> DiscoveredProcess {
    discovered(1234)
}

fn identify(game: &str, focus_priority: bool) -> SessionIdentify {
    SessionIdentify {
        handle: 0,
        game: game.into(),
        variant: "stable".into(),
        name: "player".into(),
        version: "1.0".into(),
        focus_priority,
    }
}
#[test]
fn replies_racing_detach_report_target_gone_but_unissued_handles_are_invalid() {
    let mut registry = SessionRegistry::new();
    let attached = registry
        .attach(process(), vec!["test.stale".into()], Instant::now())
        .unwrap();
    assert!(
        registry
            .detach(attached.handle, DetachReason::Exited)
            .is_some()
    );
    let identify_error = registry
        .identify(
            "test.stale",
            attached.handle,
            identify("osu", false),
            Instant::now(),
        )
        .unwrap_err();
    let decline_error = registry
        .decline("test.stale", attached.handle, Instant::now())
        .unwrap_err();
    assert_eq!(
        vosu_status::from(identify_error),
        vosu_status::VOSU_ERR_TARGET_GONE
    );
    assert_eq!(
        vosu_status::from(decline_error),
        vosu_status::VOSU_ERR_TARGET_GONE
    );
    for handle in [SessionHandle(0), SessionHandle(attached.handle.0 + 1)] {
        let identify_error = registry
            .identify("test.stale", handle, identify("osu", false), Instant::now())
            .unwrap_err();
        let decline_error = registry
            .decline("test.stale", handle, Instant::now())
            .unwrap_err();
        assert_eq!(
            vosu_status::from(identify_error),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            vosu_status::from(decline_error),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        );
    }
}

#[test]
fn attach_offers_the_first_candidate_in_install_order() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let attached = registry
        .attach(
            process(),
            vec!["vosu.first".into(), "vosu.second".into()],
            now,
        )
        .unwrap();
    assert_eq!(attached.message.pid, 1234);
    assert_eq!(attached.message.generation, 1);
    assert!(registry.is_pending(attached.handle));
}

#[test]
fn attach_with_no_candidates_creates_no_session() {
    let mut registry = SessionRegistry::new();
    assert!(registry.attach(process(), vec![], Instant::now()).is_none());
}

#[test]
fn decline_reoffers_the_same_generation_to_the_next_candidate_in_install_order() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let first = registry
        .attach(
            process(),
            vec!["vosu.first".into(), "vosu.second".into()],
            now,
        )
        .unwrap();
    let generation = registry.generation_of(first.handle).unwrap();

    let outcome = registry.decline("vosu.first", first.handle, now).unwrap();
    let DeclineOutcome::ReOffered {
        detached,
        reoffered,
    } = outcome
    else {
        panic!("expected a re-offer");
    };
    assert_eq!(
        detached,
        SessionDetached {
            handle: first.handle.0,
            reason: DetachReason::Declined,
        }
    );
    assert_ne!(reoffered.handle, first.handle);
    assert_eq!(reoffered.message.generation, generation.0);
    assert_eq!(reoffered.message.session, first.message.session);
    assert!(!registry.is_pending(first.handle));
    assert!(registry.is_pending(reoffered.handle));
}

#[test]
fn decline_with_no_remaining_candidate_drops_the_generation() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let only = registry
        .attach(process(), vec!["vosu.only".into()], now)
        .unwrap();
    let outcome = registry.decline("vosu.only", only.handle, now).unwrap();
    assert_eq!(
        outcome,
        DeclineOutcome::Dropped {
            detached: SessionDetached {
                handle: only.handle.0,
                reason: DetachReason::Declined,
            }
        }
    );
    assert!(!registry.is_pending(only.handle));
}

#[test]
fn attach_refuses_a_duplicate_pid_while_pending() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    registry
        .attach(process(), vec!["vosu.first".into()], now)
        .unwrap();
    // Same PID, a second matching plugin: still refused while the
    // first offer is still pending.
    assert!(
        registry
            .attach(process(), vec!["vosu.second".into()], now)
            .is_none()
    );
}

#[test]
fn attach_refuses_a_duplicate_pid_while_identified() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let attached = registry
        .attach(process(), vec!["vosu.only".into()], now)
        .unwrap();
    registry
        .identify("vosu.only", attached.handle, identify("osu", false), now)
        .unwrap();
    assert!(
        registry
            .attach(process(), vec!["vosu.other".into()], now)
            .is_none()
    );
}

#[test]
fn attach_refuses_a_duplicate_pid_dropped_by_decline_until_the_process_exits() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let attached = registry
        .attach(process(), vec!["vosu.only".into()], now)
        .unwrap();
    let outcome = registry.decline("vosu.only", attached.handle, now).unwrap();
    assert!(matches!(outcome, DeclineOutcome::Dropped { .. }));
    assert!(registry.is_dropped(process().pid));

    // A rescan of the same still-running PID must not mint a new
    // session, even offered to a different plugin than the one that
    // declined it.
    assert!(
        registry
            .attach(process(), vec!["vosu.only".into()], now)
            .is_none()
    );

    // Once the process actually exits, the PID is free to attach
    // again.
    registry.forget_exited_pid(process().pid);
    assert!(!registry.is_dropped(process().pid));
    assert!(
        registry
            .attach(process(), vec!["vosu.only".into()], now)
            .is_some()
    );
}

#[test]
fn decline_after_own_identify_is_refused() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let attached = registry
        .attach(process(), vec!["vosu.only".into()], now)
        .unwrap();
    registry
        .identify("vosu.only", attached.handle, identify("osu", false), now)
        .unwrap();
    assert_eq!(
        registry.decline("vosu.only", attached.handle, now),
        Err(SessionError::AlreadyIdentified)
    );
}

#[test]
fn decline_from_a_non_owning_plugin_is_refused() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let attached = registry
        .attach(process(), vec!["vosu.only".into()], now)
        .unwrap();
    assert_eq!(
        registry.decline("vosu.imposter", attached.handle, now),
        Err(SessionError::NotOwner)
    );
}

#[test]
fn identify_exposes_the_session() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let attached = registry
        .attach(process(), vec!["vosu.only".into()], now)
        .unwrap();
    assert!(!registry.is_identified(attached.handle));
    let outcome = registry
        .identify("vosu.only", attached.handle, identify("osu", false), now)
        .unwrap();
    assert!(!outcome.focus_priority_granted);
    assert!(!outcome.focus_priority_ignored);
    assert!(registry.is_identified(attached.handle));
    assert_eq!(registry.identify_of(attached.handle).unwrap().game, "osu");
}

#[test]
fn first_focus_priority_identify_is_granted() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let attached = registry
        .attach(process(), vec!["vosu.only".into()], now)
        .unwrap();
    let outcome = registry
        .identify("vosu.only", attached.handle, identify("osu", true), now)
        .unwrap();
    assert!(outcome.focus_priority_granted);
    assert!(!outcome.focus_priority_ignored);
    assert_eq!(registry.priority_holder(), Some(attached.handle));
}

#[test]
fn second_focus_priority_identify_is_ignored_not_refused() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let first = registry
        .attach(process(), vec!["vosu.a".into()], now)
        .unwrap();
    registry
        .identify("vosu.a", first.handle, identify("osu", true), now)
        .unwrap();

    let second = registry
        .attach(discovered(5555), vec!["vosu.b".into()], now)
        .unwrap();
    let outcome = registry
        .identify("vosu.b", second.handle, identify("osu", true), now)
        .unwrap();
    assert!(!outcome.focus_priority_granted);
    assert!(outcome.focus_priority_ignored);
    assert_eq!(registry.priority_holder(), Some(first.handle));
}

#[test]
fn focus_priority_is_picked_up_by_the_next_session_to_set_it_after_the_holder_detaches() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let first = registry
        .attach(process(), vec!["vosu.a".into()], now)
        .unwrap();
    registry
        .identify("vosu.a", first.handle, identify("osu", true), now)
        .unwrap();

    let second = registry
        .attach(discovered(5555), vec!["vosu.b".into()], now)
        .unwrap();
    registry
        .identify("vosu.b", second.handle, identify("osu", true), now)
        .unwrap();
    assert_eq!(registry.priority_holder(), Some(first.handle));

    registry.detach(first.handle, DetachReason::Exited);
    assert_eq!(registry.priority_holder(), None);

    let third = registry
        .attach(discovered(7777), vec!["vosu.c".into()], now)
        .unwrap();
    registry
        .identify("vosu.c", third.handle, identify("osu", true), now)
        .unwrap();
    assert_eq!(registry.priority_holder(), Some(third.handle));
}

#[test]
fn identification_timeout_fires_once_and_the_session_stays_identifiable() {
    let mut registry = SessionRegistry::with_identification_timeout(Duration::from_secs(5));
    let start = Instant::now();
    let attached = registry
        .attach(process(), vec!["vosu.only".into()], start)
        .unwrap();

    assert!(
        registry
            .sweep_identification_timeouts(start + Duration::from_secs(4))
            .is_empty()
    );

    let fired = registry.sweep_identification_timeouts(start + Duration::from_secs(5));
    assert_eq!(fired, vec![attached.handle]);

    // Logged once: a later sweep past the deadline reports nothing new.
    assert!(
        registry
            .sweep_identification_timeouts(start + Duration::from_secs(10))
            .is_empty()
    );

    // Still hidden, not detached, and still identifiable afterward.
    assert!(registry.is_pending(attached.handle));
    assert!(!registry.is_identified(attached.handle));
    registry
        .identify(
            "vosu.only",
            attached.handle,
            identify("osu", false),
            start + Duration::from_secs(20),
        )
        .unwrap();
    assert!(registry.is_identified(attached.handle));
}

#[test]
fn detach_releases_focus_priority_only_when_the_holder_detaches() {
    let mut registry = SessionRegistry::new();
    let now = Instant::now();
    let holder = registry
        .attach(process(), vec!["vosu.a".into()], now)
        .unwrap();
    registry
        .identify("vosu.a", holder.handle, identify("osu", true), now)
        .unwrap();

    let other = registry
        .attach(discovered(2222), vec!["vosu.b".into()], now)
        .unwrap();
    registry
        .identify("vosu.b", other.handle, identify("osu", false), now)
        .unwrap();

    registry.detach(other.handle, DetachReason::Exited);
    assert_eq!(registry.priority_holder(), Some(holder.handle));

    let message = registry.detach(holder.handle, DetachReason::Lost).unwrap();
    assert_eq!(message.reason, DetachReason::Lost);
    assert_eq!(registry.priority_holder(), None);
}

#[test]
fn detach_of_unknown_handle_returns_none() {
    let mut registry = SessionRegistry::new();
    assert!(
        registry
            .detach(SessionHandle(999), DetachReason::Exited)
            .is_none()
    );
}
