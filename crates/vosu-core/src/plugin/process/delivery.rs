//! The plugin host's per-instance delivery queue (`docs/v2/plugin-abi.md`
//! §7.2, §6.12 "Delivery coalescing and ordering", §7.6).
//!
//! The ring reader classifies every public host → plugin frame
//! ([`inspect`]) and [`DeliveryQueue::push`]es it without ever blocking;
//! the instance's dispatch worker (or, under `concurrent_dispatch`, its
//! worker pool) takes deliveries with [`DeliveryQueue::next`]. A `SYNC`
//! result never enters this queue: it goes straight to its waiter
//! (`super::waiters`).
//!
//! **Lanes.** The queue is bounded by construction, not by refusing an
//! admitted message:
//!
//! - *terminal* — an asynchronous `READ_PLAN_RESULT`, a `SCAN_RESULT`, a
//!   relayed `ACTION_RESULT` and a `STREAM_CLOSE`: results and closes of
//!   requests core admitted under its own caps (§6.3, §6.4, §6.12, M3
//!   leases). They are taken before anything ordinary, exactly as core's
//!   reserved outbox runs ahead of its ordinary dispatch queue, so a
//!   backlog of ordinary work never delays or drops one.
//! - *latest-only* — `DEMAND_CHANGED` and `FOCUS_CHANGED` keyed by handle,
//!   `TIMER_FIRED` by timer, `SETTINGS_CHANGED` (one per instance) and
//!   `ACTION_PROGRESS` by invocation. A newer frame replaces the queued one
//!   of its key in place, keeping its position: bounded by distinct keys.
//! - *state* — `STATE_UPDATE`, per session at most one in-flight split
//!   sequence (never merged until its `more: false` part arrives) plus one
//!   complete pending update, merged by (`ns`, `pointer`) with core's own
//!   [`PendingUpdate`] and re-split with core's [`split_update`] at
//!   delivery. A pending update that is exactly one unsplit frame is
//!   delivered as its original bytes.
//! - *ordinary* — work units and session lifecycle, in arrival order,
//!   bounded by core's own outstanding-request and lifecycle bounds.
//!
//! Every queued payload byte is accounted; more than [`DELIVERY_BUDGET`]
//! can only come from a core that broke its own bounds, a protocol
//! violation that ends the channel rather than an unbounded queue or a
//! dropped message.
//!
//! **Ordering.** State, demand, focus, lifecycle, and scoped actions are
//! session-affine: under `concurrent_dispatch` no two run at once and none
//! overtakes an earlier one. A session's state parts are strictly ordered,
//! its final state precedes detach, and attach precedes its actions.
//! Everything else may run concurrently.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use serde::Deserialize;
use vosu_plugin_api::{
    Message, StateEntry, StateUpdate, VOSU_FRAME_MAX_BYTES, decode, vosu_opcode,
};

use super::admission::Release;
use super::frame::FrameHeader;
use super::ring::PayloadBuffer;
use crate::plugin::execution::DispatchTarget;
use crate::state::{MAX_SELECTORS, PendingUpdate, split_update};

/// The most payload bytes the queue holds at once (A5 left the plugin
/// host's queue budget open; recorded in `plugin-abi.md` §7.6).
pub(crate) const DELIVERY_BUDGET: usize = 64 * 1024 * 1024;
/// Spare payload buffers kept for reuse, each at most [`SPARE_CAPACITY`].
const SPARE_BUFFERS: usize = 16;
const SPARE_CAPACITY: usize = 64 * 1024;

/// A frame core may not send: a protocol violation that ends the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum DeliveryError {
    #[error("core sent opcode {0:#x} after DESTROY")]
    AfterDestroy(u32),
    #[error("host → plugin opcode {0:#x} carries a malformed payload")]
    Malformed(u32),
    #[error("queued deliveries would exceed the {DELIVERY_BUDGET}-byte budget")]
    Budget,
    #[error("session {0}'s STATE_UPDATE exceeds the selector bound")]
    StateOverflow(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum LatestKey {
    Demand(u64),
    Focus(u64),
    Timer(u64),
    Settings,
    Progress(u64),
}

#[derive(Debug)]
enum Lane {
    Terminal,
    Ordinary,
    Invoke(u64),
    Cancel(u64),
    Latest(LatestKey),
    State(StateUpdate),
}

/// How the ring reader classified one host → plugin frame.
#[derive(Debug)]
pub(crate) struct Inbound {
    lane: Lane,
    session: Option<u64>,
    /// What this frame frees in local admission.
    pub(crate) release: Release,
}

#[derive(Deserialize)]
struct HandleField {
    handle: u64,
}

#[derive(Deserialize)]
struct InvocationField {
    invocation: u64,
}

#[derive(Deserialize)]
struct ActionFields {
    invocation: u64,
    session: Option<u64>,
}

#[derive(Deserialize)]
struct TimerField {
    timer: u64,
}

fn leading_u64(payload: &[u8]) -> Option<u64> {
    Some(u64::from_le_bytes(payload.get(..8)?.try_into().ok()?))
}

/// Classifies one public host → plugin frame the policy already admitted.
/// Only the fields routing needs are decoded; `dispatch` still receives
/// the payload byte for byte.
pub(crate) fn inspect(header: &FrameHeader, payload: &[u8]) -> Result<Inbound, DeliveryError> {
    use vosu_opcode::*;
    let malformed = || DeliveryError::Malformed(header.opcode);
    let opcode = vosu_opcode::try_from(header.opcode).map_err(|()| malformed())?;
    let handle = || decode::<HandleField>(payload).map(|field| field.handle);
    let invocation = || decode::<InvocationField>(payload).map(|field| field.invocation);
    let inbound = |lane, session, release| Inbound {
        lane,
        session,
        release,
    };
    Ok(match opcode {
        VOSU_OP_READ_PLAN_RESULT => {
            let plan = leading_u64(payload).ok_or_else(malformed)?;
            inbound(Lane::Terminal, None, Release::Plan(plan))
        }
        VOSU_OP_SCAN_RESULT => inbound(Lane::Terminal, None, Release::Scan(header.correlation)),
        VOSU_OP_ACTION_RESULT => {
            let invocation = invocation().map_err(|_| malformed())?;
            inbound(Lane::Terminal, None, Release::Call(invocation))
        }
        // An artifact declaration's reply and its release are terminal
        // like a lease close: bounded by core's own reservations, and a
        // release never overtakes the `STREAM_CLOSE`s that precede it.
        VOSU_OP_STREAM_CLOSE | VOSU_OP_ARTIFACT_DECLARED | VOSU_OP_ARTIFACT_RELEASE => {
            inbound(Lane::Terminal, None, Release::Nothing)
        }
        VOSU_OP_DEMAND_CHANGED => {
            let handle = leading_u64(payload).ok_or_else(malformed)?;
            let key = LatestKey::Demand(handle);
            inbound(Lane::Latest(key), Some(handle), Release::Nothing)
        }
        VOSU_OP_FOCUS_CHANGED => {
            let handle = handle().map_err(|_| malformed())?;
            let key = LatestKey::Focus(handle);
            inbound(Lane::Latest(key), Some(handle), Release::Nothing)
        }
        VOSU_OP_TIMER_FIRED => {
            let timer = decode::<TimerField>(payload)
                .map_err(|_| malformed())?
                .timer;
            let key = LatestKey::Timer(timer);
            inbound(Lane::Latest(key), None, Release::Timer(timer))
        }
        VOSU_OP_SETTINGS_CHANGED => {
            inbound(Lane::Latest(LatestKey::Settings), None, Release::Nothing)
        }
        VOSU_OP_ACTION_PROGRESS => {
            let key = LatestKey::Progress(invocation().map_err(|_| malformed())?);
            inbound(Lane::Latest(key), None, Release::Nothing)
        }
        VOSU_OP_ACTION_INVOKE => {
            let action = decode::<ActionFields>(payload).map_err(|_| malformed())?;
            let lane = action
                .session
                .map_or(Lane::Ordinary, |_| Lane::Invoke(action.invocation));
            inbound(lane, action.session, Release::Nothing)
        }
        VOSU_OP_ACTION_CANCEL => {
            let invocation = invocation().map_err(|_| malformed())?;
            inbound(Lane::Cancel(invocation), None, Release::Nothing)
        }
        VOSU_OP_STATE_UPDATE => {
            let update = decode::<StateUpdate>(payload).map_err(|_| malformed())?;
            let session = update.session;
            inbound(Lane::State(update), Some(session), Release::Nothing)
        }
        VOSU_OP_SESSION_ATTACHED => {
            let handle = handle().map_err(|_| malformed())?;
            inbound(Lane::Ordinary, Some(handle), Release::Nothing)
        }
        VOSU_OP_SESSION_DETACHED => {
            let handle = handle().map_err(|_| malformed())?;
            inbound(Lane::Ordinary, Some(handle), Release::Session(handle))
        }
        _ => inbound(Lane::Ordinary, None, Release::Nothing),
    })
}

#[derive(Debug)]
pub(crate) struct Entry {
    header: FrameHeader,
    payload: PayloadBuffer,
}

impl Entry {
    fn len(&self) -> usize {
        self.payload.bytes().len()
    }
}

#[derive(Debug)]
enum Item {
    Frame {
        entry: Entry,
        session: Option<u64>,
        invocation: Option<u64>,
    },
    Latest {
        key: LatestKey,
        session: Option<u64>,
    },
    State(u64),
}

impl Item {
    fn session(&self) -> Option<u64> {
        match self {
            Self::Frame { session, .. } | Self::Latest { session, .. } => *session,
            Self::State(session) => Some(*session),
        }
    }
}

#[derive(Default)]
struct StateSlot {
    /// Entries of a split sequence whose `more: false` part has not arrived:
    /// never merged into `pending`.
    inflight: Vec<StateEntry>,
    inflight_bytes: usize,
    in_sequence: bool,
    /// The subscription was replaced mid-sequence: drop the rest of it.
    discard_rest: bool,
    /// The one complete update waiting for delivery.
    pending: PendingUpdate,
    /// `pending` is exactly this one unsplit frame.
    raw: Option<Entry>,
    /// `pending` has its [`Item::State`] in the ordinary lane.
    queued: bool,
}

#[derive(Default)]
struct QueueState {
    terminal: VecDeque<Entry>,
    ordinary: VecDeque<Item>,
    latest: HashMap<LatestKey, Entry>,
    states: HashMap<u64, StateSlot>,
    /// A cancel arriving before a scoped invoke is taken must wait behind it.
    queued_invokes: HashMap<u64, u64>,
    /// Session-affine deliveries currently being dispatched.
    running: HashMap<u64, usize>,
    bytes: usize,
    high_water: usize,
    spare: Vec<PayloadBuffer>,
    /// `DESTROY` arrived: drain what is queued, accept nothing more.
    closed: bool,
    /// The channel was lost: deliver nothing more.
    aborted: bool,
}

impl QueueState {
    /// Moves `buffer`'s bytes into a new entry and leaves a spare in it.
    fn take(&mut self, header: FrameHeader, buffer: &mut PayloadBuffer) -> Entry {
        let mut payload = self.spare.pop().unwrap_or_default();
        std::mem::swap(&mut payload, buffer);
        Entry { header, payload }
    }

    fn recycle(&mut self, payload: PayloadBuffer) {
        if self.spare.len() < SPARE_BUFFERS && payload.capacity_bytes() <= SPARE_CAPACITY {
            self.spare.push(payload);
        }
    }

    fn admit_bytes(&mut self, len: usize, budget: usize) -> Result<(), DeliveryError> {
        let bytes = self.bytes.checked_add(len).ok_or(DeliveryError::Budget)?;
        if bytes > budget {
            return Err(DeliveryError::Budget);
        }
        self.bytes = bytes;
        self.high_water = self.high_water.max(bytes);
        Ok(())
    }

    fn push_state(
        &mut self,
        header: FrameHeader,
        buffer: &mut PayloadBuffer,
        update: StateUpdate,
        budget: usize,
    ) -> Result<(), DeliveryError> {
        let session = update.session;
        let len = buffer.bytes().len();
        let slot = self.states.entry(session).or_default();
        if slot.discard_rest {
            slot.discard_rest = update.more;
            return Ok(());
        }
        let single = !update.more && !slot.in_sequence;
        let complete = if single {
            update.values
        } else {
            if slot.inflight.len() + update.values.len() > MAX_SELECTORS {
                return Err(DeliveryError::StateOverflow(session));
            }
            slot.inflight.extend(update.values);
            slot.in_sequence = update.more;
            if update.more {
                slot.inflight_bytes += len;
                return self.admit_bytes(len, budget);
            }
            let inflight_bytes = std::mem::take(&mut slot.inflight_bytes);
            let complete = std::mem::take(&mut slot.inflight);
            self.bytes -= inflight_bytes;
            complete
        };
        let slot = self.states.get_mut(&session).expect("inserted above");
        let first = slot.pending.is_empty() && slot.raw.is_none();
        slot.pending
            .merge(complete)
            .map_err(|_| DeliveryError::StateOverflow(session))?;
        let raw = slot.raw.take();
        let queue = !std::mem::replace(&mut slot.queued, true);
        if let Some(raw) = raw {
            // Merged with a newer update: re-split at delivery instead.
            self.bytes -= raw.len();
            self.recycle(raw.payload);
        }
        if single && first {
            self.admit_bytes(len, budget)?;
            let entry = self.take(header, buffer);
            self.states.get_mut(&session).expect("inserted above").raw = Some(entry);
        }
        if queue {
            self.ordinary.push_back(Item::State(session));
        }
        Ok(())
    }

    /// The next eligible ordinary item: the first whose session (if any) has
    /// nothing running and no earlier item still waiting.
    fn take_ordinary(&mut self) -> Option<Delivery> {
        let mut blocked: Vec<u64> = Vec::new();
        let mut index = 0;
        while index < self.ordinary.len() {
            if let Some(session) = self.ordinary[index].session()
                && (self.running.contains_key(&session) || blocked.contains(&session))
            {
                blocked.push(session);
                index += 1;
                continue;
            }
            let item = self.ordinary.remove(index).expect("index is in bounds");
            let delivery = match item {
                Item::Frame {
                    entry,
                    session,
                    invocation,
                } => {
                    if let Some(invocation) = invocation {
                        self.queued_invokes.remove(&invocation);
                    }
                    self.bytes -= entry.len();
                    if entry.header.opcode == vosu_opcode::VOSU_OP_SESSION_DETACHED as u32
                        && let Some(session) = session
                    {
                        self.retire_state(session);
                    }
                    Delivery::Frame { entry, session }
                }
                Item::Latest { key, session } => {
                    let entry = self
                        .latest
                        .remove(&key)
                        .expect("a latest item has its entry");
                    self.bytes -= entry.len();
                    Delivery::Frame { entry, session }
                }
                Item::State(session) => {
                    let slot = self
                        .states
                        .get_mut(&session)
                        .expect("a state item has its slot");
                    slot.queued = false;
                    let update = match slot.raw.take() {
                        Some(raw) => {
                            slot.pending.clear();
                            self.bytes -= raw.len();
                            StatePayload::Raw(raw)
                        }
                        None => {
                            let entries = slot.pending.take();
                            if entries.is_empty() {
                                // Cleared by a resubscription.
                                continue;
                            }
                            StatePayload::Merged(entries)
                        }
                    };
                    Delivery::State { session, update }
                }
            };
            if let Some(session) = delivery.session() {
                *self.running.entry(session).or_default() += 1;
            }
            return Some(delivery);
        }
        None
    }

    /// Drops a detached session's state slot once its last update was taken.
    fn retire_state(&mut self, session: u64) {
        if let Some(slot) = self.states.get(&session)
            && !slot.queued
        {
            let slot = self.states.remove(&session).expect("checked above");
            self.bytes -= slot.inflight_bytes;
        }
    }
}

/// What a dispatch worker delivers next.
#[derive(Debug)]
pub(crate) enum Delivery {
    Frame { entry: Entry, session: Option<u64> },
    State { session: u64, update: StatePayload },
}

#[derive(Debug)]
pub(crate) enum StatePayload {
    /// One unsplit update, as core sent it.
    Raw(Entry),
    /// A merged update, re-split at delivery.
    Merged(Vec<StateEntry>),
}

impl Delivery {
    fn session(&self) -> Option<u64> {
        match self {
            Self::Frame { session, .. } => *session,
            Self::State { session, .. } => Some(*session),
        }
    }

    /// Calls `dispatch` for this delivery: one call per frame, or one per
    /// re-split part of a merged update, in order. `scratch` holds each
    /// re-encoded part 8-byte aligned. Dispatch statuses are ignored, as
    /// core ignores them in-process.
    pub(crate) fn dispatch(&mut self, target: &DispatchTarget, scratch: &mut PayloadBuffer) {
        let send = |entry: &Entry| {
            let FrameHeader {
                opcode,
                flags,
                correlation,
            } = entry.header;
            target.dispatch(opcode, flags, correlation, entry.payload.bytes());
        };
        match self {
            Self::Frame { entry, .. }
            | Self::State {
                update: StatePayload::Raw(entry),
                ..
            } => send(entry),
            Self::State {
                session,
                update: StatePayload::Merged(entries),
            } => {
                let parts = split_update(
                    *session,
                    std::mem::take(entries),
                    VOSU_FRAME_MAX_BYTES as usize,
                )
                .expect("a merged update keeps within the selector bound");
                for part in parts {
                    let Ok(bytes) = Message::StateUpdate(part).encode_payload() else {
                        continue;
                    };
                    scratch.fill(&bytes);
                    target.dispatch(
                        vosu_opcode::VOSU_OP_STATE_UPDATE as u32,
                        0,
                        0,
                        scratch.bytes(),
                    );
                }
            }
        }
    }
}

/// One instance's delivery queue.
pub(crate) struct DeliveryQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
    budget: usize,
}

impl Default for DeliveryQueue {
    fn default() -> Self {
        Self::with_budget(DELIVERY_BUDGET)
    }
}

impl DeliveryQueue {
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
            budget,
        }
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        // Every critical section restores the queue's invariants before
        // anything that could panic; plugin code never runs under it.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues one frame the reader received into `buffer`, classified by
    /// [`inspect`]. Never blocks. `buffer` keeps the bytes when the frame
    /// is discarded, and otherwise becomes a spare.
    pub(crate) fn push(
        &self,
        header: FrameHeader,
        buffer: &mut PayloadBuffer,
        inbound: Inbound,
    ) -> Result<(), DeliveryError> {
        let mut state = self.lock();
        if state.aborted {
            return Ok(());
        }
        if state.closed {
            return Err(DeliveryError::AfterDestroy(header.opcode));
        }
        let len = buffer.bytes().len();
        match inbound.lane {
            Lane::State(update) => state.push_state(header, buffer, update, self.budget)?,
            Lane::Latest(key) => {
                if let Some(old) = state.latest.get(&key).map(Entry::len) {
                    state.bytes -= old;
                    state.admit_bytes(len, self.budget)?;
                    let entry = state.latest.get_mut(&key).expect("checked above");
                    entry.header = header;
                    std::mem::swap(&mut entry.payload, buffer);
                } else {
                    state.admit_bytes(len, self.budget)?;
                    let entry = state.take(header, buffer);
                    state.latest.insert(key, entry);
                    let session = inbound.session;
                    state.ordinary.push_back(Item::Latest { key, session });
                }
            }
            Lane::Terminal => {
                state.admit_bytes(len, self.budget)?;
                let entry = state.take(header, buffer);
                state.terminal.push_back(entry);
            }
            Lane::Ordinary | Lane::Invoke(_) | Lane::Cancel(_) => {
                state.admit_bytes(len, self.budget)?;
                let entry = state.take(header, buffer);
                let (session, invocation) = match inbound.lane {
                    Lane::Invoke(invocation) => {
                        let session = inbound.session.expect("scoped invocation");
                        state.queued_invokes.insert(invocation, session);
                        (Some(session), Some(invocation))
                    }
                    Lane::Cancel(invocation) => {
                        (state.queued_invokes.get(&invocation).copied(), None)
                    }
                    _ => (inbound.session, None),
                };
                state.ordinary.push_back(Item::Frame {
                    entry,
                    session,
                    invocation,
                });
            }
        }
        drop(state);
        self.ready.notify_one();
        Ok(())
    }

    /// Waits for the next delivery. `None` once the channel was lost, or
    /// after `DESTROY` once everything queued was taken.
    pub(crate) fn next(&self) -> Option<Delivery> {
        let mut state = self.lock();
        loop {
            if state.aborted {
                return None;
            }
            if let Some(entry) = state.terminal.pop_front() {
                state.bytes -= entry.len();
                return Some(Delivery::Frame {
                    entry,
                    session: None,
                });
            }
            if let Some(delivery) = state.take_ordinary() {
                return Some(delivery);
            }
            if state.closed && state.ordinary.is_empty() {
                return None;
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Ends one delivery taken with [`next`](Self::next): its session may
    /// deliver again, and its buffers are reused.
    pub(crate) fn done(&self, delivery: Delivery) {
        let mut state = self.lock();
        if let Some(session) = delivery.session()
            && let Some(running) = state.running.get_mut(&session)
        {
            *running -= 1;
            if *running == 0 {
                state.running.remove(&session);
            }
        }
        match delivery {
            Delivery::Frame { entry, .. }
            | Delivery::State {
                update: StatePayload::Raw(entry),
                ..
            } => state.recycle(entry.payload),
            Delivery::State { .. } => {}
        }
        drop(state);
        self.ready.notify_all();
    }

    /// Core accepted a `STATE_SUBSCRIBE` replacing `session`'s selection:
    /// what core had not delivered is cleared there (§6.12), so the
    /// updates not yet dispatched here are cleared too, including the rest
    /// of a split sequence still arriving. The new selection's baseline
    /// follows on the ring.
    pub(crate) fn reset_state(&self, session: u64) {
        let mut state = self.lock();
        let Some(slot) = state.states.get_mut(&session) else {
            return;
        };
        slot.pending.clear();
        slot.inflight.clear();
        slot.discard_rest = slot.in_sequence;
        slot.in_sequence = false;
        let freed = std::mem::take(&mut slot.inflight_bytes);
        let raw = slot.raw.take();
        state.bytes -= freed;
        if let Some(raw) = raw {
            state.bytes -= raw.len();
            state.recycle(raw.payload);
        }
    }

    /// `DESTROY`: the workers drain what is queued and stop. False when the
    /// queue was already closed (a second `DESTROY`).
    pub(crate) fn close(&self) -> bool {
        let newly = !std::mem::replace(&mut self.lock().closed, true);
        self.ready.notify_all();
        newly
    }

    /// The channel was lost: nothing more is delivered.
    pub(crate) fn abort(&self) {
        self.lock().aborted = true;
        self.ready.notify_all();
    }

    /// (queued payload bytes, their high-water mark).
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> (usize, usize) {
        let state = self.lock();
        (state.bytes, state.high_water)
    }
}

#[cfg(test)]
mod tests;
