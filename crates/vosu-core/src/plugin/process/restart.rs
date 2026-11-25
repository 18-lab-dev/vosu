//! The A5.3 restart policy for one supervised process instance
//! (`docs/v2/plugin-abi.md` §7.6 "Restart"): a rolling crash window that
//! decides, for each crash event, between a backoff delay and the `failed`
//! latch.
//!
//! Pure and clock-free: every call takes the monotonic `now`, so the
//! domain cases (bursts, the fifth event, aging, manual reset) run with
//! injected time and never sleep. `host::supervision` owns the one
//! cancellable timer that waits out a delay.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Events older than this no longer count toward the latch.
pub const CRASH_WINDOW: Duration = Duration::from_secs(300);

/// The event that sets `failed` when this many fall inside the window.
pub const FAILED_AFTER: usize = 5;

/// The accepted-restart delays, saturating at the last.
pub const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

/// What one crash event decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDecision {
    /// Start a new generation after `delay`. `streak` counts the events
    /// since the window was last empty, this one included.
    Restart { delay: Duration, streak: u32 },
    /// The fifth event inside the window: no automatic restart until a
    /// manual one.
    Failed,
}

/// The timestamps of the last [`FAILED_AFTER`] events and the streak since
/// the window was last empty. Bounded: at most five entries, because the
/// fifth sets the latch and no further event is recorded until a reset.
#[derive(Debug, Default, Clone)]
pub struct CrashWindow {
    events: VecDeque<Instant>,
    streak: u32,
}

impl CrashWindow {
    pub fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(FAILED_AFTER),
            streak: 0,
        }
    }

    /// Records one crash event at `now` and decides what follows it.
    pub fn record(&mut self, now: Instant) -> RestartDecision {
        while self
            .events
            .front()
            .is_some_and(|&event| now.saturating_duration_since(event) >= CRASH_WINDOW)
        {
            self.events.pop_front();
        }
        if self.events.is_empty() {
            self.streak = 0;
        }
        self.events.push_back(now);
        self.streak = self.streak.saturating_add(1);
        if self.events.len() >= FAILED_AFTER {
            return RestartDecision::Failed;
        }
        let step = (self.streak as usize - 1).min(BACKOFF.len() - 1);
        RestartDecision::Restart {
            delay: BACKOFF[step],
            streak: self.streak,
        }
    }

    /// A manual restart: forgets every event and the streak.
    pub fn reset(&mut self) {
        self.events.clear();
        self.streak = 0;
    }

    /// Events currently inside the window as of the last record.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests;
