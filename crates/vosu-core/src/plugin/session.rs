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
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
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
    /// This handle was never issued by the session registry.
    UnknownHandle,
    /// This handle was issued, but discovery already retired it.
    TargetGone,
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
    bitness: Bitness,
    identify: SessionIdentify,
    public_id: String,
}

/// Every attached session (pending identification, or already identified)
/// across every plugin instance (§6.2).
pub struct SessionRegistry {
    next_generation: u64,
    next_handle: u64,
    next_public_id: u64,
    identification_timeout: Duration,
    /// Revoked under the registry lock, before a retired handle can be reused.
    read_epoch: Arc<AtomicU64>,
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
            read_epoch: Arc::new(AtomicU64::new(0)),
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
    fn missing_handle_error(&self, handle: SessionHandle) -> SessionError {
        // Handles start at one and are never reused. A high-water mark
        // distinguishes stale lifecycle replies without a tombstone map.
        if handle.0 != 0 && handle.0 <= self.next_handle {
            SessionError::TargetGone
        } else {
            SessionError::UnknownHandle
        }
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
            name: Some(process.name.clone()),
            path: Some(process.path.clone()),
            command_line: Some(process.command_line.clone()),
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
            None => return Err(self.missing_handle_error(handle)),
            Some(record) if record.owner != plugin => return Err(SessionError::NotOwner),
            Some(_) => {}
        }
        let mut record = self.pending.remove(&handle).expect("checked above");
        self.invalidate_read_cache();
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
            name: Some(record.process.name.clone()),
            path: Some(record.process.path.clone()),
            command_line: Some(record.process.command_line.clone()),
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
            None => return Err(self.missing_handle_error(handle)),
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
                bitness: record.process.bitness,
                identify: request,
                public_id: record.public_id,
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
        self.invalidate_read_cache();
        if self.priority_holder == Some(handle) {
            self.priority_holder = None;
        }
        Some(SessionDetached {
            handle: handle.0,
            reason,
        })
    }

    /// Owner teardown also retires offers that have not identified yet,
    /// including timed-out offers deliberately kept hidden during discovery.
    pub(crate) fn detach_owner(&mut self, owner: &str) -> Vec<SessionDetached> {
        let detached = self
            .identified
            .extract_if(|_, record| record.owner.as_str() == owner)
            .map(|(handle, _)| handle)
            .chain(
                self.pending
                    .extract_if(|_, record| record.owner.as_str() == owner)
                    .map(|(handle, _)| handle),
            )
            .map(|handle| SessionDetached {
                handle: handle.0,
                reason: DetachReason::Lost,
            })
            .collect();
        self.priority_holder = self
            .priority_holder
            .filter(|handle| self.identified.contains_key(handle));
        detached
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
    /// identified session, for `super::focus`'s Focus rule computation.
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

    /// Identified-session replay for non-owners, deliberately without process facts.
    pub(crate) fn public_attachment(&self, handle: SessionHandle) -> Option<SessionAttached> {
        let record = self.identified.get(&handle)?;
        Some(SessionAttached {
            session: record.public_id.clone(),
            handle: handle.0,
            generation: record.generation.0,
            pid: record.pid,
            bitness: record.bitness,
            name: None,
            path: None,
            command_line: None,
            argv: None,
        })
    }

    pub fn public_id_of(&self, handle: SessionHandle) -> Option<&str> {
        self.identified
            .get(&handle)
            .map(|record| record.public_id.as_str())
            .or_else(|| {
                self.pending
                    .get(&handle)
                    .map(|record| record.public_id.as_str())
            })
    }

    pub fn was_issued(&self, handle: SessionHandle) -> bool {
        handle.0 != 0 && handle.0 <= self.next_handle
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

    pub(crate) fn read_cache_epoch(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.read_epoch)
    }

    fn invalidate_read_cache(&self) {
        self.read_epoch
            .fetch_update(Ordering::Release, Ordering::Relaxed, |epoch| {
                epoch.checked_add(1)
            })
            .expect("session read-cache epoch exhausted");
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

mod diagnostics;

impl From<SessionError> for vosu_status {
    fn from(error: SessionError) -> Self {
        match error {
            // Nothing this registry tracks has that handle at all: the
            // call itself is malformed, not a permission question.
            SessionError::UnknownHandle => Self::VOSU_ERR_INVALID_ARGUMENT,
            SessionError::TargetGone => Self::VOSU_ERR_TARGET_GONE,
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
mod tests;
