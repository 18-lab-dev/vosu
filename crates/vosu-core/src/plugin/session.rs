//! Session lifecycle (`docs/v2/plugin-abi.md` §6.2): PID generations,
//! `SESSION_ATTACHED`/`SESSION_DETACHED` message construction,
//! `SESSION_DECLINE` re-offering (D6), `SESSION_IDENTIFY` and its
//! `focus_priority` holder bookkeeping, the 5-second identification
//! timeout, and the "still exists internally, hidden" rule. Focus
//! computation itself (the foreground-owner rule, `FOCUS_CHANGED` delivery
//! order) is [`super::focus`]; this module only tracks which session, if
//! any, currently holds `focus_priority`, since that is part of processing
//! `SESSION_IDENTIFY` (§6.2), not the Focus rule (`docs/v2/api-v1.md`).
//!
//! Discovery wiring (the real OS process list) is a later milestone (phase
//! (d)); [`SessionRegistry::attach`] takes an already-matched, already-
//! ordered candidate list (`super::matcher::MatchOutcome` plus every
//! shadowed plugin, in install order) exactly as
//! `docs/v2/implementation-plan.md` M1's exit criteria test it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use vosu_plugin_api::{
    Bitness, DetachReason, SessionAttached, SessionDetached, SessionIdentify, vosu_status,
};

use super::matcher::PluginId;

/// §6.2: "If identification does not arrive within 5 seconds of
/// `SESSION_ATTACHED`, core logs once and keeps the session hidden".
pub const IDENTIFICATION_TIMEOUT: Duration = Duration::from_secs(5);

/// This plugin instance's `vosu_session_id` for a session (§6.2, §3
/// "Session identity"). Globally unique in this registry; the ABI only
/// promises it is meaningful within the owning plugin instance, which a
/// globally unique value also satisfies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionHandle(pub u64);

/// One PID generation (§6.1 "A matched process becomes a candidate
/// instance exactly as 1.x client detection does"; `src/instance.rs`'s
/// `next_generation` counter, generalized): assigned once per discovered
/// process occurrence, kept across a `SESSION_DECLINE` re-offer to the
/// next matching plugin since it is still the same discovered occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Generation(pub u64);

/// A process discovery observed (synthetic in M1 tests). Carries the
/// four process facts `SESSION_ATTACHED` also publishes (A1b/DA,
/// `docs/v2/plugin-abi.md` §6.2): `name`/`path`/`command_line`/`argv`
/// make this `Clone`, not `Copy`.
#[derive(Debug, Clone)]
pub struct DiscoveredProcess {
    pub pid: u32,
    pub bitness: Bitness,
    pub name: String,
    pub path: String,
    pub command_line: String,
    pub argv: Option<Vec<String>>,
}

/// Every way a `SESSION_DECLINE`/`SESSION_IDENTIFY` call can be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    /// No pending or identified session has this handle.
    UnknownHandle,
    /// This handle belongs to a different plugin instance than the caller.
    NotOwner,
    /// `SESSION_DECLINE` arrived after this plugin's own `SESSION_IDENTIFY`
    /// for the handle: "valid only before this plugin's own
    /// `SESSION_IDENTIFY` for the handle — sending it after is refused
    /// with `VOSU_ERR_INVALID_ARGUMENT`" (§6.2, D6).
    AlreadyIdentified,
}

/// A freshly minted `SESSION_ATTACHED` offer to one plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachedSession {
    pub handle: SessionHandle,
    pub message: SessionAttached,
}

/// [`SessionRegistry::decline`]'s outcome. Both variants carry `detached`:
/// the declining plugin's own old handle is retired with
/// `SessionDetached{reason: Declined}` (`docs/v2/plugin-abi.md` §6.2: "core
/// detaches it from this plugin instance"), delivered before any re-offer.
#[derive(Debug, Clone, PartialEq)]
pub enum DeclineOutcome {
    /// Re-offered to the next candidate in install order (D6).
    ReOffered {
        detached: SessionDetached,
        reoffered: AttachedSession,
    },
    /// No more candidates matched this process: the generation is dropped
    /// for good, never exposed.
    Dropped { detached: SessionDetached },
}

/// [`SessionRegistry::identify`]'s outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdentifyOutcome {
    /// This session now holds `focus_priority` (it was the first to
    /// request it, or no session currently holds it).
    pub focus_priority_granted: bool,
    /// `focus_priority: true` was requested but another session already
    /// holds it: "not refused — refusing it would need a cross-instance
    /// check … is **not** refused … is ignored (logged once, not
    /// refused)" (§6.2). The caller logs this once.
    pub focus_priority_ignored: bool,
}

struct PendingSession {
    generation: Generation,
    process: DiscoveredProcess,
    public_id: String,
    /// Every plugin whose matcher accepted this process, in install order
    /// (D18's runtime winner first, every shadowed plugin after).
    candidates: Vec<PluginId>,
    /// Index into `candidates` of the plugin currently holding this offer.
    offer_index: usize,
    owner: PluginId,
    attached_at: Instant,
    timeout_logged: bool,
}

struct IdentifiedSession {
    owner: PluginId,
    generation: Generation,
    pid: u32,
    identify: SessionIdentify,
}

/// Every attached session (pending identification, or already identified)
/// across every plugin instance (§6.2).
pub struct SessionRegistry {
    next_generation: u64,
    next_handle: u64,
    next_public_id: u64,
    identification_timeout: Duration,
    pending: HashMap<SessionHandle, PendingSession>,
    identified: HashMap<SessionHandle, IdentifiedSession>,
    /// The one session currently holding `focus_priority`, if any (§6.2:
    /// "Core keeps the first session identified with `focus_priority: true`
    /// and holds it until that session detaches").
    priority_holder: Option<SessionHandle>,
    /// PIDs declined by every matching plugin (D6: "dropped for that PID
    /// generation, never re-offered again"), cleared only by
    /// [`Self::forget_exited_pid`] once the real OS process behind the PID
    /// is actually gone — bounded the same way the OS bounds live PIDs,
    /// never by an eviction policy of its own.
    dropped: HashMap<u32, Generation>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::with_identification_timeout(IDENTIFICATION_TIMEOUT)
    }

    /// Same as [`Self::new`] but with an overridable timeout, so tests
    /// exercise the 5-second rule without a real 5-second wait.
    #[must_use]
    pub fn with_identification_timeout(identification_timeout: Duration) -> Self {
        Self {
            next_generation: 0,
            next_handle: 0,
            next_public_id: 0,
            identification_timeout,
            pending: HashMap::new(),
            identified: HashMap::new(),
            priority_holder: None,
            dropped: HashMap::new(),
        }
    }

    fn next_generation(&mut self) -> Generation {
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("session generation exhausted");
        Generation(self.next_generation)
    }

    fn next_handle(&mut self) -> SessionHandle {
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .expect("session handle exhausted");
        SessionHandle(self.next_handle)
    }

    fn next_public_id(&mut self) -> String {
        self.next_public_id = self
            .next_public_id
            .checked_add(1)
            .expect("public session id exhausted");
        format!("session-{}", self.next_public_id)
    }

    /// Offers a newly discovered process to the first of `candidates`
    /// (install order). `candidates` empty means nothing matched: no
    /// session is created at all. Refuses (also returning `None`) a `pid`
    /// this registry already tracks — pending, identified, or dropped for
    /// this PID generation (D6) — since a rescan of a still-running
    /// process must not mint it a second generation; the caller must
    /// [`Self::forget_exited_pid`] once the OS process actually exits
    /// before a later discovery of the same PID attaches again. Otherwise
    /// mints a fresh [`Generation`] and public id for this discovered
    /// occurrence.
    pub fn attach(
        &mut self,
        process: DiscoveredProcess,
        candidates: Vec<PluginId>,
        now: Instant,
    ) -> Option<AttachedSession> {
        if candidates.is_empty() {
            return None;
        }
        if self.dropped.contains_key(&process.pid)
            || self.pending.values().any(|r| r.process.pid == process.pid)
            || self.identified.values().any(|r| r.pid == process.pid)
        {
            return None;
        }
        let generation = self.next_generation();
        let public_id = self.next_public_id();
        let owner = candidates[0].clone();
        let handle = self.next_handle();
        let message = SessionAttached {
            session: public_id.clone(),
            handle: handle.0,
            generation: generation.0,
            pid: process.pid,
            bitness: process.bitness,
            name: process.name.clone(),
            path: process.path.clone(),
            command_line: process.command_line.clone(),
            argv: process.argv.clone(),
        };
        self.pending.insert(
            handle,
            PendingSession {
                generation,
                process,
                public_id,
                candidates,
                offer_index: 0,
                owner,
                attached_at: now,
                timeout_logged: false,
            },
        );
        Some(AttachedSession { handle, message })
    }

    /// Clears the dropped-PID record for `pid`: the real OS process behind
    /// it has exited, so a PID reused by a different, later process is not
    /// held to the first process's decline (D6 only promises the drop
    /// holds "within" one PID generation). A no-op if `pid` was not
    /// dropped.
    pub fn forget_exited_pid(&mut self, pid: u32) {
        self.dropped.remove(&pid);
    }

    /// Whether `pid` is currently refused by [`Self::attach`] because
    /// every matching plugin declined its last discovered occurrence.
    #[must_use]
    pub fn is_dropped(&self, pid: u32) -> bool {
        self.dropped.contains_key(&pid)
    }

    /// `VOSU_OP_SESSION_DECLINE` from `plugin` for `handle` (D6): re-offers
    /// the same generation to the next candidate in install order, or
    /// drops it if there is none left. Either way the declining plugin's
    /// own `handle` is retired with `SessionDetached{reason: Declined}`
    /// (`docs/v2/plugin-abi.md` §6.2).
    pub fn decline(
        &mut self,
        plugin: &str,
        handle: SessionHandle,
        now: Instant,
    ) -> Result<DeclineOutcome, SessionError> {
        if self.identified.contains_key(&handle) {
            return Err(SessionError::AlreadyIdentified);
        }
        match self.pending.get(&handle) {
            None => return Err(SessionError::UnknownHandle),
            Some(record) if record.owner != plugin => return Err(SessionError::NotOwner),
            Some(_) => {}
        }
        let mut record = self.pending.remove(&handle).expect("checked above");
        let detached = SessionDetached {
            handle: handle.0,
            reason: DetachReason::Declined,
        };
        record.offer_index += 1;
        if record.offer_index >= record.candidates.len() {
            self.dropped.insert(record.process.pid, record.generation);
            return Ok(DeclineOutcome::Dropped { detached });
        }
        let next_owner = record.candidates[record.offer_index].clone();
        let new_handle = self.next_handle();
        let message = SessionAttached {
            session: record.public_id.clone(),
            handle: new_handle.0,
            generation: record.generation.0,
            pid: record.process.pid,
            bitness: record.process.bitness,
            name: record.process.name.clone(),
            path: record.process.path.clone(),
            command_line: record.process.command_line.clone(),
            argv: record.process.argv.clone(),
        };
        record.owner = next_owner;
        record.attached_at = now;
        record.timeout_logged = false;
        self.pending.insert(new_handle, record);
        Ok(DeclineOutcome::ReOffered {
            detached,
            reoffered: AttachedSession {
                handle: new_handle,
                message,
            },
        })
    }

    /// `VOSU_OP_SESSION_IDENTIFY` from `plugin` for `handle`: exposes the
    /// session and resolves the `focus_priority` request.
    pub fn identify(
        &mut self,
        plugin: &str,
        handle: SessionHandle,
        request: SessionIdentify,
        _now: Instant,
    ) -> Result<IdentifyOutcome, SessionError> {
        if self.identified.contains_key(&handle) {
            return Err(SessionError::AlreadyIdentified);
        }
        match self.pending.get(&handle) {
            None => return Err(SessionError::UnknownHandle),
            Some(record) if record.owner != plugin => return Err(SessionError::NotOwner),
            Some(_) => {}
        }
        let record = self.pending.remove(&handle).expect("checked above");
        let focus_priority_granted = request.focus_priority && self.priority_holder.is_none();
        if focus_priority_granted {
            self.priority_holder = Some(handle);
        }
        let focus_priority_ignored = request.focus_priority && !focus_priority_granted;
        self.identified.insert(
            handle,
            IdentifiedSession {
                owner: record.owner,
                generation: record.generation,
                pid: record.process.pid,
                identify: request,
            },
        );
        Ok(IdentifyOutcome {
            focus_priority_granted,
            focus_priority_ignored,
        })
    }

    /// The process behind `handle` exited, was lost, or was superseded;
    /// not a `SESSION_DECLINE`. Releases `focus_priority` if `handle` held
    /// it. Returns the `SESSION_DETACHED` message, or `None` if `handle`
    /// names nothing this registry is tracking.
    pub fn detach(
        &mut self,
        handle: SessionHandle,
        reason: DetachReason,
    ) -> Option<SessionDetached> {
        let was_identified = self.identified.remove(&handle).is_some();
        let was_pending = self.pending.remove(&handle).is_some();
        if !was_identified && !was_pending {
            return None;
        }
        if self.priority_holder == Some(handle) {
            self.priority_holder = None;
        }
        Some(SessionDetached {
            handle: handle.0,
            reason,
        })
    }

    /// Sweeps every still-pending session for the 5-second identification
    /// timeout (§6.2), returning each handle that just crossed it for the
    /// first time (the caller logs once per handle). A timed-out session
    /// is not removed — "core … keeps the session hidden … it still
    /// exists internally, and the game plugin may still identify it
    /// later" — so a later `identify`/`decline` for it still succeeds.
    pub fn sweep_identification_timeouts(&mut self, now: Instant) -> Vec<SessionHandle> {
        let timeout = self.identification_timeout;
        self.pending
            .iter_mut()
            .filter_map(|(handle, record)| {
                if !record.timeout_logged && now.duration_since(record.attached_at) >= timeout {
                    record.timeout_logged = true;
                    Some(*handle)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Test-only: whether `handle`'s pending identification timeout has
    /// already been logged by [`Self::sweep_identification_timeouts`] —
    /// lets a test wait for the sweep to actually run instead of a fixed
    /// sleep guessing it has.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn timeout_logged(&self, handle: SessionHandle) -> Option<bool> {
        self.pending
            .get(&handle)
            .map(|record| record.timeout_logged)
    }

    #[must_use]
    pub fn is_identified(&self, handle: SessionHandle) -> bool {
        self.identified.contains_key(&handle)
    }

    #[must_use]
    pub fn is_pending(&self, handle: SessionHandle) -> bool {
        self.pending.contains_key(&handle)
    }

    #[must_use]
    pub fn priority_holder(&self) -> Option<SessionHandle> {
        self.priority_holder
    }

    /// The owning plugin, PID, and generation of every currently
    /// identified session, for [`super::focus`]'s Focus rule computation.
    pub fn identified_sessions(
        &self,
    ) -> impl Iterator<Item = (SessionHandle, &PluginId, u32)> + '_ {
        self.identified
            .iter()
            .map(|(handle, record)| (*handle, &record.owner, record.pid))
    }

    /// The full `SESSION_IDENTIFY` request a session sent, once
    /// identified.
    #[must_use]
    pub fn identify_of(&self, handle: SessionHandle) -> Option<&SessionIdentify> {
        self.identified.get(&handle).map(|record| &record.identify)
    }

    /// The [`Generation`] a handle belongs to, whether pending or
    /// identified.
    #[must_use]
    pub fn generation_of(&self, handle: SessionHandle) -> Option<Generation> {
        self.identified
            .get(&handle)
            .map(|record| record.generation)
            .or_else(|| self.pending.get(&handle).map(|record| record.generation))
    }

    /// The owning plugin and pid for `handle`, whether pending or
    /// identified: a plugin may read a session before identifying it
    /// (§6.2 "The plugin may now begin reading ... and must identify the
    /// client ... before publishing anything else"), so read-plan/scan
    /// resolution (§6.3, §6.4) needs this for either state, unlike
    /// [`Self::identified_sessions`], which is Focus-rule-only.
    #[must_use]
    pub fn owner_and_pid(&self, handle: SessionHandle) -> Option<(&PluginId, u32)> {
        if let Some(record) = self.identified.get(&handle) {
            return Some((&record.owner, record.pid));
        }
        self.pending
            .get(&handle)
            .map(|record| (&record.owner, record.process.pid))
    }

    /// The current handle for `pid`, pending or identified, for
    /// `PluginHost::process_exited` to resolve a discovery-reported exit
    /// back to the session it must detach. `None` once `pid` already has
    /// no tracked session (never attached, or whichever was there already
    /// detached) — correct either way, since [`Self::detach`] is already
    /// a no-op for an unknown handle.
    #[must_use]
    pub fn handle_for_pid(&self, pid: u32) -> Option<SessionHandle> {
        self.identified
            .iter()
            .find(|(_, record)| record.pid == pid)
            .map(|(handle, _)| *handle)
            .or_else(|| {
                self.pending
                    .iter()
                    .find(|(_, record)| record.process.pid == pid)
                    .map(|(handle, _)| *handle)
            })
    }
}

impl From<SessionError> for vosu_status {
    fn from(error: SessionError) -> Self {
        match error {
            // Nothing this registry tracks has that handle at all: the
            // call itself is malformed, not a permission question.
            SessionError::UnknownHandle => Self::VOSU_ERR_INVALID_ARGUMENT,
            // The handle exists, but belongs to a different plugin
            // instance — exactly §5's `VOSU_ERR_PERMISSION_DENIED`
            // example ("a read plan touching a session it did not
            // match").
            SessionError::NotOwner => Self::VOSU_ERR_PERMISSION_DENIED,
            // D6: "sending it after is refused with
            // `VOSU_ERR_INVALID_ARGUMENT`".
            SessionError::AlreadyIdentified => Self::VOSU_ERR_INVALID_ARGUMENT,
        }
    }
}

#[cfg(test)]
mod tests {
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
}
