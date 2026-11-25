//! A5.3 domain cases with injected monotonic time: no test sleeps.

use std::time::{Duration, Instant};

use super::{BACKOFF, CRASH_WINDOW, CrashWindow, FAILED_AFTER, RestartDecision};

fn secs(value: u64) -> Duration {
    Duration::from_secs(value)
}

fn delay(decision: RestartDecision) -> Duration {
    match decision {
        RestartDecision::Restart { delay, .. } => delay,
        RestartDecision::Failed => panic!("expected a restart, got the failed latch"),
    }
}

#[test]
fn a_burst_backs_off_one_two_four_eight_and_the_fifth_event_fails() {
    let start = Instant::now();
    let mut window = CrashWindow::new();
    let delays: Vec<_> = (0..4)
        .map(|event| delay(window.record(start + secs(event))))
        .collect();
    assert_eq!(delays, [secs(1), secs(2), secs(4), secs(8)]);
    assert_eq!(window.record(start + secs(4)), RestartDecision::Failed);
    assert_eq!(window.len(), FAILED_AFTER);
}

#[test]
fn the_fifth_event_just_inside_the_window_still_fails() {
    let start = Instant::now();
    let mut window = CrashWindow::new();
    for event in 0..4 {
        delay(window.record(start + secs(event)));
    }
    let edge = start + CRASH_WINDOW - Duration::from_millis(1);
    assert_eq!(window.record(edge), RestartDecision::Failed);
}

#[test]
fn sixteen_seconds_is_reached_only_after_older_events_age_out() {
    let start = Instant::now();
    let mut window = CrashWindow::new();
    for event in 0..4 {
        delay(window.record(start + secs(event)));
    }
    // The first event is exactly one window old: it no longer counts, so
    // this is the fourth event inside the window and the fifth of the
    // streak, which never emptied.
    let decision = window.record(start + CRASH_WINDOW);
    assert_eq!(
        decision,
        RestartDecision::Restart {
            delay: secs(16),
            streak: 5
        }
    );
    assert_eq!(window.len(), 4);
    // Saturates at 16 s while the streak continues.
    assert_eq!(delay(window.record(start + CRASH_WINDOW + secs(1))), secs(16));
}

#[test]
fn an_emptied_window_starts_the_backoff_again() {
    let start = Instant::now();
    let mut window = CrashWindow::new();
    for event in 0..3 {
        delay(window.record(start + secs(event)));
    }
    let later = start + secs(2) + CRASH_WINDOW;
    assert_eq!(
        window.record(later),
        RestartDecision::Restart {
            delay: BACKOFF[0],
            streak: 1
        }
    );
    assert_eq!(window.len(), 1);
}

#[test]
fn a_manual_reset_clears_the_latch_and_the_streak() {
    let start = Instant::now();
    let mut window = CrashWindow::new();
    for event in 0..4 {
        delay(window.record(start + secs(event)));
    }
    assert_eq!(window.record(start + secs(5)), RestartDecision::Failed);
    window.reset();
    assert!(window.is_empty());
    assert_eq!(delay(window.record(start + secs(6))), secs(1));
}

#[test]
fn slow_crashes_spaced_beyond_the_window_never_fail() {
    let start = Instant::now();
    let mut window = CrashWindow::new();
    for event in 0..20 {
        assert_eq!(
            delay(window.record(start + CRASH_WINDOW * event)),
            secs(1),
            "event {event}"
        );
    }
}
