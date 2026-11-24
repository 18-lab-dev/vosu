//! Focus (`docs/v2/plugin-abi.md` §6.2, `docs/v2/api-v1.md` "Focus rule"):
//! the foreground-owner rule with a most-recent fallback and a
//! deterministic first-ever tie-break, `FOCUS_CHANGED` delivery ordering
//! (the losing session before the gaining one), and the promised first
//! `FOCUS_CHANGED` a newly identified session always receives.
//!
//! `focus_priority` *holding* (first identify wins, ignored not refused on
//! a second request, picked up by the next to request it once the holder
//! detaches) is [`super::session::SessionRegistry`]'s concern, since it is
//! part of processing `SESSION_IDENTIFY` (§6.2); this module only consumes
//! the result — "which session, if any, currently holds it" — to decide
//! who is focused.

use std::collections::{HashMap, HashSet};

/// Which process, if any, currently owns the OS foreground window.
/// Pluggable so the Focus rule is tested without a real desktop: tests use
/// [`SyntheticForegroundSource`].
pub trait ForegroundSource: Send + Sync {
    fn foreground_pid(&self) -> Option<u32>;
    /// Whether this source can distinguish no foreground owner from no API.
    fn available(&self) -> bool {
        true
    }
}

/// The production host's cached poller sample. Only Windows has a poller
/// (`OsForegroundSource`); elsewhere core has no OS-foreground source, so the
/// sample is unavailable and `foreground` is absent (`docs/v2/plugin-abi.md`
/// §6.2), never a fabricated `false`.
impl ForegroundSource for tokio::sync::watch::Receiver<Option<u32>> {
    fn foreground_pid(&self) -> Option<u32> {
        *self.borrow()
    }
    fn available(&self) -> bool {
        cfg!(windows)
    }
}

/// A test-only, settable [`ForegroundSource`].
#[derive(Debug, Default)]
pub struct SyntheticForegroundSource {
    pid: std::sync::Mutex<Option<u32>>,
    unavailable: bool,
}

impl SyntheticForegroundSource {
    #[must_use]
    pub fn new(pid: Option<u32>) -> Self {
        Self {
            pid: std::sync::Mutex::new(pid),
            unavailable: false,
        }
    }

    /// A source that reports no OS-foreground source at all, through the
    /// same [`ForegroundSource::available`] seam the production host's
    /// sample uses off Windows, so foreground facts are absent rather than
    /// `false`.
    #[cfg(any(test, feature = "test-harness"))]
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            pid: std::sync::Mutex::new(None),
            unavailable: true,
        }
    }

    pub fn set(&self, pid: Option<u32>) {
        *self.pid.lock().unwrap() = pid;
    }
}

impl ForegroundSource for SyntheticForegroundSource {
    fn foreground_pid(&self) -> Option<u32> {
        *self.pid.lock().unwrap()
    }
    fn available(&self) -> bool {
        !self.unavailable
    }
}

/// The real OS foreground window's owning process, for production use
/// (`PluginHost`'s default; tests use [`SyntheticForegroundSource`]
/// instead). `vosu-sys` only implements foreground-window detection on
/// Windows today (`vosu_sys::process::foreground_process_id`); on every
/// other target this always reports no foreground owner, which the Focus
/// rule already handles correctly (falls through to the most-recently-
/// focused session, then the first-ever tie-break) rather than being
/// wrong — a known gap for a future Linux window-manager integration to
/// fill, not this phase's scope.
#[derive(Debug, Default)]
pub struct OsForegroundSource;

impl ForegroundSource for OsForegroundSource {
    fn available(&self) -> bool {
        cfg!(windows)
    }
    #[cfg(windows)]
    fn foreground_pid(&self) -> Option<u32> {
        vosu_sys::process::foreground_process_id()
    }

    #[cfg(not(windows))]
    fn foreground_pid(&self) -> Option<u32> {
        None
    }
}

/// A session's own `vosu_session_id` handle, decoupled from
/// `super::session::SessionHandle` so this module stays independent and
/// directly testable with synthetic data (`docs/v2/implementation-plan.md`
/// M1 Exit: "Tests drive matching with synthetic process records").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FocusKey(pub u64);

/// One identified session, as the Focus rule needs to see it.
#[derive(Debug, Clone, Copy)]
pub struct FocusCandidate {
    pub handle: FocusKey,
    pub pid: u32,
    /// Install order of the owning plugin, the first key for the "first
    /// focus decision ever" tie-break (`docs/v2/api-v1.md` "Focus rule":
    /// "the first session by [ADR 0001]'s deterministic ordering within
    /// the earliest-attached plugin"). The second key is `pid` itself:
    /// ADR 0001 retains instances in PID order and "the lowest matching
    /// PID wins", not attach/discovery order, which discovery's own
    /// iteration makes unreliable.
    pub install_order: usize,
}

/// `VOSU_OP_FOCUS_CHANGED` to deliver to one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusEvent {
    pub handle: FocusKey,
    pub focused: bool,
    pub foreground: Option<bool>,
}

/// Tracks the currently focused session and the recency fallback across
/// calls, so a transition can be diffed and delivered in the documented
/// order.
pub struct FocusTracker {
    focused: Option<FocusKey>,
    /// Most-recent-first; pruned to only the sessions [`Self::recompute`]
    /// was last given as present (§6.2: a session handle is used once —
    /// `docs/v2/plugin-abi.md` §3 — so one that left `candidates` has
    /// detached for good and keeping its entry here would only grow this
    /// list without bound).
    recency: Vec<FocusKey>,
    foreground: HashMap<FocusKey, Option<bool>>,
}

impl Default for FocusTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl FocusTracker {
    #[must_use]
    pub fn new() -> Self {
        Self {
            focused: None,
            recency: Vec::new(),
            foreground: HashMap::new(),
        }
    }

    #[must_use]
    pub fn focused(&self) -> Option<FocusKey> {
        self.focused
    }

    fn decide(
        &self,
        candidates: &[FocusCandidate],
        present: &HashSet<FocusKey>,
        priority_holder: Option<FocusKey>,
        foreground_pid: Option<u32>,
    ) -> Option<FocusKey> {
        if candidates.is_empty() {
            return None;
        }
        // `focus_priority`: "is always the focused session while it
        // exists, regardless of OS foreground or recency" (api-v1.md
        // "Focus rule").
        if let Some(holder) = priority_holder
            && present.contains(&holder)
        {
            return Some(holder);
        }
        // The foreground-owner rule.
        if let Some(fg_pid) = foreground_pid
            && let Some(candidate) = candidates.iter().find(|c| c.pid == fg_pid)
        {
            return Some(candidate.handle);
        }
        // Most-recently-focused fallback.
        if let Some(recent) = self.recency.iter().find(|handle| present.contains(handle)) {
            return Some(*recent);
        }
        // "The first focus decision after no session was focused … picks
        // the first session by [ADR 0001]'s deterministic ordering within
        // the earliest-attached plugin": install order, then lowest PID
        // (ADR 0001's "lowest matching PID wins").
        candidates
            .iter()
            .min_by_key(|c| (c.install_order, c.pid))
            .map(|c| c.handle)
    }

    /// Recomputes focus given the current identified `candidates` and
    /// `priority_holder` (`super::session::SessionRegistry::priority_holder`,
    /// re-keyed to [`FocusKey`] by the caller). Returns the events to
    /// deliver, in §6.2's documented "Delivery ordering": the session
    /// losing focus before the session gaining it. Foreground-only changes
    /// also notify the affected owner, without changing the focus decision.
    pub fn recompute(
        &mut self,
        candidates: &[FocusCandidate],
        priority_holder: Option<FocusKey>,
        foreground: &dyn ForegroundSource,
    ) -> Vec<FocusEvent> {
        let present: HashSet<FocusKey> = candidates.iter().map(|c| c.handle).collect();
        self.recency.retain(|handle| present.contains(handle));
        let foreground_pid = foreground.foreground_pid();
        let new_focus = self.decide(candidates, &present, priority_holder, foreground_pid);
        let states: HashMap<_, _> = candidates
            .iter()
            .map(|c| {
                (
                    c.handle,
                    foreground
                        .available()
                        .then_some(foreground_pid == Some(c.pid)),
                )
            })
            .collect();
        let mut events = Vec::new();
        // The previously focused session is told it lost focus only while
        // it is still a candidate: once it has left `candidates` it has
        // already detached, and its own `SESSION_DETACHED` already
        // retired the handle — a `FOCUS_CHANGED` naming it afterward
        // would name a handle the plugin no longer recognizes.
        if let Some(old) = self.focused
            && Some(old) != new_focus
            && present.contains(&old)
        {
            events.push(FocusEvent {
                handle: old,
                focused: false,
                foreground: states.get(&old).copied().flatten(),
            });
        }
        if let Some(new) = new_focus
            && Some(new) != self.focused
        {
            events.push(FocusEvent {
                handle: new,
                focused: true,
                foreground: states.get(&new).copied().flatten(),
            });
            self.recency.retain(|handle| *handle != new);
            self.recency.insert(0, new);
        }
        for candidate in candidates {
            if self
                .foreground
                .get(&candidate.handle)
                .is_some_and(|old| Some(old) != states.get(&candidate.handle))
                && !events.iter().any(|e| e.handle == candidate.handle)
            {
                events.push(FocusEvent {
                    handle: candidate.handle,
                    focused: Some(candidate.handle) == new_focus,
                    foreground: states.get(&candidate.handle).copied().flatten(),
                });
            }
        }
        self.foreground = states;
        self.focused = new_focus;
        events
    }

    /// Call once a session is newly identified, after adding it to
    /// `candidates`: §6.2 "`SESSION_ATTACHED` carries no `focused` field; a
    /// plugin that needs it waits for the first `FOCUS_CHANGED` (sent
    /// once, promptly, after the session is identified, even if focus does
    /// not change relative to the previous state)". [`Self::recompute`]
    /// alone would miss this when the identified session's own focus state
    /// happens not to change the overall transition.
    pub fn on_session_identified(
        &mut self,
        new_handle: FocusKey,
        candidates: &[FocusCandidate],
        priority_holder: Option<FocusKey>,
        foreground: &dyn ForegroundSource,
    ) -> Vec<FocusEvent> {
        let mut events = self.recompute(candidates, priority_holder, foreground);
        if !events.iter().any(|event| event.handle == new_handle) {
            events.push(FocusEvent {
                handle: new_handle,
                focused: self.focused == Some(new_handle),
                foreground: self.foreground.get(&new_handle).copied().flatten(),
            });
        }
        events
    }

    /// Call after a session detaches and has already been removed from
    /// `candidates`.
    pub fn on_session_detached(
        &mut self,
        candidates: &[FocusCandidate],
        priority_holder: Option<FocusKey>,
        foreground: &dyn ForegroundSource,
    ) -> Vec<FocusEvent> {
        self.recompute(candidates, priority_holder, foreground)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(handle: u64, pid: u32, install_order: usize) -> FocusCandidate {
        FocusCandidate {
            handle: FocusKey(handle),
            pid,
            install_order,
        }
    }

    #[test]
    fn first_ever_decision_picks_the_deterministic_first_by_install_order() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(None);
        let candidates = vec![candidate(2, 200, 1), candidate(1, 100, 0)];
        let events = tracker.recompute(&candidates, None, &foreground);
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(1),
                focused: true,
                foreground: Some(false),
            }]
        );
        assert_eq!(tracker.focused(), Some(FocusKey(1)));
    }

    #[test]
    fn first_ever_decision_ties_on_install_order_by_lowest_pid() {
        // Same install order (both the earliest-installed plugin's own
        // discoveries): ADR 0001's "lowest matching PID wins" decides, not
        // whichever happened to attach/discover first.
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(None);
        let candidates = vec![candidate(2, 300, 0), candidate(1, 150, 0)];
        let events = tracker.recompute(&candidates, None, &foreground);
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(1),
                focused: true,
                foreground: Some(false),
            }]
        );
    }

    #[test]
    fn foreground_owner_wins_over_everything_but_focus_priority() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(Some(200));
        let candidates = vec![candidate(1, 100, 0), candidate(2, 200, 1)];
        let events = tracker.recompute(&candidates, None, &foreground);
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(2),
                focused: true,
                foreground: Some(true),
            }]
        );
    }

    #[test]
    fn delivery_order_is_losing_session_first_then_gaining_session() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(Some(100));
        let candidates = vec![candidate(1, 100, 0), candidate(2, 200, 1)];
        tracker.recompute(&candidates, None, &foreground);
        assert_eq!(tracker.focused(), Some(FocusKey(1)));

        foreground.set(Some(200));
        let events = tracker.recompute(&candidates, None, &foreground);
        assert_eq!(
            events,
            vec![
                FocusEvent {
                    handle: FocusKey(1),
                    focused: false,
                    foreground: Some(false),
                },
                FocusEvent {
                    handle: FocusKey(2),
                    focused: true,
                    foreground: Some(true),
                },
            ]
        );
    }

    #[test]
    fn most_recently_focused_fallback_when_nothing_owns_foreground() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(Some(100));
        let candidates = vec![candidate(1, 100, 0), candidate(2, 200, 1)];
        tracker.recompute(&candidates, None, &foreground);
        assert_eq!(tracker.focused(), Some(FocusKey(1)));

        // Neither process now owns the foreground: falls back to the most
        // recently focused session (1), not the deterministic first (also
        // 1 here, so exercise it with a disjoint "first" to be sure).
        foreground.set(Some(999));
        let events = tracker.recompute(&candidates, None, &foreground);
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(1),
                focused: true,
                foreground: Some(false)
            }]
        );
        assert_eq!(tracker.focused(), Some(FocusKey(1)));
    }

    #[test]
    fn most_recent_fallback_prefers_recency_over_deterministic_first() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(Some(200));
        let candidates = vec![candidate(1, 100, 0), candidate(2, 200, 1)];
        tracker.recompute(&candidates, None, &foreground);
        assert_eq!(tracker.focused(), Some(FocusKey(2)));

        foreground.set(None);
        let events = tracker.recompute(&candidates, None, &foreground);
        // 2 was most recently focused even though 1 is the deterministic
        // "first by install order".
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(2),
                focused: true,
                foreground: Some(false)
            }]
        );
        assert_eq!(tracker.focused(), Some(FocusKey(2)));
    }

    #[test]
    fn recency_drops_a_handle_once_it_leaves_candidates() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(Some(200));
        let candidates = vec![candidate(1, 100, 0), candidate(2, 200, 1)];
        tracker.recompute(&candidates, None, &foreground);
        assert_eq!(tracker.recency, vec![FocusKey(2)]);

        // 2 detaches: its recency entry is pruned on the very next
        // recompute, not kept around without bound in case it reappears
        // (a handle is never reused, so it never does, `docs/v2/plugin-
        // abi.md` §3).
        foreground.set(None);
        let remaining = vec![candidate(1, 100, 0)];
        tracker.on_session_detached(&remaining, None, &foreground);
        assert_eq!(tracker.focused(), Some(FocusKey(1)));
        assert_eq!(tracker.recency, vec![FocusKey(1)]);
    }

    #[test]
    fn focus_priority_wins_unconditionally_over_foreground() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(Some(100));
        let candidates = vec![candidate(1, 100, 0), candidate(2, 200, 1)];
        let events = tracker.recompute(&candidates, Some(FocusKey(2)), &foreground);
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(2),
                focused: true,
                foreground: Some(false),
            }]
        );
    }

    #[test]
    fn on_session_identified_always_notifies_the_new_session_even_without_a_transition() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(Some(100));
        let first = vec![candidate(1, 100, 0)];
        tracker.recompute(&first, None, &foreground);
        assert_eq!(tracker.focused(), Some(FocusKey(1)));

        // Session 2 identifies but does not become focused (1 keeps
        // foreground): overall focus is unchanged, yet 2 still gets its
        // mandatory first FOCUS_CHANGED.
        let both = vec![candidate(1, 100, 0), candidate(2, 200, 1)];
        let events = tracker.on_session_identified(FocusKey(2), &both, None, &foreground);
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(2),
                focused: false,
                foreground: Some(false),
            }]
        );
    }

    #[test]
    fn on_session_identified_does_not_duplicate_when_the_new_session_wins_focus() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(Some(200));
        let candidates = vec![candidate(1, 100, 0), candidate(2, 200, 1)];
        let events = tracker.on_session_identified(FocusKey(2), &candidates, None, &foreground);
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(2),
                focused: true,
                foreground: Some(true),
            }]
        );
    }

    #[test]
    fn detach_of_the_focused_session_lets_recency_pick_the_next_one() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(None);
        let candidates = vec![candidate(1, 100, 0), candidate(2, 200, 1)];
        tracker.recompute(&candidates, None, &foreground);
        assert_eq!(tracker.focused(), Some(FocusKey(1)));

        // Focus 2 next, so recency is [2, 1].
        foreground.set(Some(200));
        tracker.recompute(&candidates, None, &foreground);
        assert_eq!(tracker.focused(), Some(FocusKey(2)));
        foreground.set(None);

        // Session 2 detaches: falls back to the next most recent, 1. 2's
        // own SESSION_DETACHED already retired its handle, so no loss
        // FOCUS_CHANGED is sent for it — only the gain for 1.
        let remaining = vec![candidate(1, 100, 0)];
        let events = tracker.on_session_detached(&remaining, None, &foreground);
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(1),
                focused: true,
                foreground: Some(false),
            }]
        );
    }

    #[test]
    fn no_candidates_means_no_focused_session() {
        let mut tracker = FocusTracker::new();
        let foreground = SyntheticForegroundSource::new(Some(100));
        let candidates = vec![candidate(1, 100, 0)];
        tracker.recompute(&candidates, None, &foreground);
        assert_eq!(tracker.focused(), Some(FocusKey(1)));

        // 1 detached (it is no longer a candidate): its own
        // SESSION_DETACHED already retired the handle, so no loss
        // FOCUS_CHANGED is sent for it.
        let events = tracker.on_session_detached(&[], None, &foreground);
        assert!(events.is_empty());
        assert_eq!(tracker.focused(), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_os_source_omits_foreground() {
        let mut tracker = FocusTracker::new();
        let events = tracker.recompute(&[candidate(8414, 8414, 0)], None, &OsForegroundSource);
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(8414),
                focused: true,
                foreground: None,
            }]
        );
    }

    /// The host's default source is the poller's cached sample. It reports
    /// a foreground fact exactly where `OsForegroundSource` does: on
    /// Windows a pid that owns nothing is `false`; elsewhere the fact is
    /// absent, never `false` (plugin-abi.md §6.2).
    #[test]
    fn production_sample_reports_foreground_only_where_the_os_source_does() {
        let (_updates, sample) = tokio::sync::watch::channel(Some(9_000));
        assert_eq!(sample.available(), OsForegroundSource.available());
        let mut tracker = FocusTracker::new();
        let events = tracker.recompute(&[candidate(8414, 8414, 0)], None, &sample);
        assert_eq!(
            events,
            vec![FocusEvent {
                handle: FocusKey(8414),
                focused: true,
                foreground: cfg!(windows).then_some(false),
            }]
        );
    }
}
