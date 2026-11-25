//! Core's plugin → host direction for one plugin host
//! (`docs/v2/plugin-abi.md` §7.6 "Acknowledged sends", A5.2).
//!
//! The ring reader never handles a message itself: handling can take
//! host locks, and a reply it wrote from the reader could park on a full
//! host → plugin ring that only this side's reader would ever announce
//! space on. It moves each public frame's aligned buffer (no copy) into
//! one of two routes:
//!
//! - **ordinary** — every acknowledged frame except a `SYNC` read plan,
//!   and every `VOSU_OP_LOG_RECORD`, handled in ring order by one worker
//!   through [`PluginIngress::deliver`] (the in-process validation and
//!   routing). An acknowledged frame is answered with its `STATUS`.
//!   Acknowledged frames are bounded by the 64 live requests the channel's
//!   policy admits; unacknowledged log records are bounded by
//!   [`LOG_BUDGET`] queued bytes, past which a record is dropped and
//!   counted (§6.10 already drops records past its rate limit).
//! - **broker** — each `VOSU_READ_PLAN_SYNC` submission is queued on the
//!   host's bounded broker workers (M5 B0,
//!   [`PluginIngress::submit_broker_work`]: the same fair pool asynchronous
//!   plans and scans run on, this instance's own sub-queue), so a slow plan
//!   delays neither ordinary traffic nor this reader. The worker runs the
//!   host's own synchronous read-plan path, standing in for the child's
//!   submitting thread: it delivers an accepted plan's result through the
//!   instance's dispatch target, which the endpoint turns into
//!   `REPLY_NEXT` plus the unmodified `READ_PLAN_RESULT`
//!   ([`claim_sync_reply`]); a refusal is answered with its `STATUS`.
//!
//! **`SYNC` budget.** Core answers a `SYNC` plan that has no reply within
//! its budget (`sync_budget_ms`, measured from receipt) with
//! `STATUS VOSU_ERR_TIMED_OUT` itself, from one timer thread per channel:
//! a plan queued behind busy broker workers, or blocked inside one native
//! read, never reaches the child's backstop and costs the channel. Exactly
//! one answer per request: whichever of the timer, the refusal and the
//! result closes the request in the [`Ledger`] first writes; a late result
//! is discarded, and a plan the timer already answered is never started.
//!
//! **Reply order.** Every request is closed in the channel's [`Ledger`]
//! *before* its reply is written, so the child can never meet a full
//! ledger with a request core already answered. While the ordinary worker
//! handles a request and writes its `STATUS`, it holds the endpoint's
//! [`StatusGate`]: any frame another thread dispatches to this child waits
//! until that `STATUS` is on the ring, so an accepted request's `STATUS`
//! always precedes the results it causes (§7.6).

use std::cell::Cell;
use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use vosu_plugin_api::vosu_status;

use super::super::channel::Sender;
use super::super::frame::{FrameHeader, Ledger, RequestKind, control};
use super::super::ring::PayloadBuffer;
use crate::plugin::execution::{MessageRef, PluginIngress};

/// Queued bytes of unacknowledged `VOSU_OP_LOG_RECORD`s (A5 left this open;
/// recorded in `plugin-abi.md` §7.6).
pub(crate) const LOG_BUDGET: usize = 1024 * 1024;
const SPARE_BUFFERS: usize = 16;
const SPARE_CAPACITY: usize = 64 * 1024;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Each critical section restores its queue's invariants before any
    // call that could panic; no host lock or plugin call runs under it.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Orders the frames other threads dispatch to one child after the
/// `STATUS` of the request the ordinary worker is handling (module docs).
#[derive(Debug, Default)]
pub(super) struct StatusGate {
    holder: Mutex<Option<ThreadId>>,
    open: Condvar,
}

impl StatusGate {
    /// Waits until no other thread holds the gate. The holder itself (a
    /// handler dispatching on its own thread) passes at once.
    pub(super) fn pass(&self) {
        let current = std::thread::current().id();
        let mut holder = lock(&self.holder);
        while holder.is_some_and(|thread| thread != current) {
            holder = self
                .open
                .wait(holder)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    fn hold(&self) -> GateHold<'_> {
        *lock(&self.holder) = Some(std::thread::current().id());
        GateHold(self)
    }
}

struct GateHold<'a>(&'a StatusGate);

impl Drop for GateHold<'_> {
    fn drop(&mut self) {
        *lock(&self.0.holder) = None;
        self.0.open.notify_all();
    }
}

#[derive(Clone, Copy)]
struct SyncReply {
    endpoint: u64,
    request: u64,
    replied: bool,
}

thread_local! {
    /// The `SYNC` request this broker worker is handling, if any.
    static SYNC_REPLY: Cell<Option<SyncReply>> = const { Cell::new(None) };
}

/// What a `READ_PLAN_RESULT` dispatched on the current thread answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SyncClaim {
    /// No `SYNC` request is being handled here: an ordinary asynchronous
    /// result.
    Ordinary,
    /// The `SYNC` request this thread handles for this endpoint, claimed
    /// once.
    Reply(u64),
    /// This thread handles a `SYNC` request of another endpoint (an older
    /// generation whose instance id a newer one reuses), or already
    /// answered its own: the result must reach no child.
    Foreign,
}

/// Called by endpoint `endpoint`'s dispatch for a `READ_PLAN_RESULT`.
pub(super) fn claim_sync_reply(endpoint: u64) -> SyncClaim {
    SYNC_REPLY.with(|cell| {
        let Some(mut reply) = cell.get() else {
            return SyncClaim::Ordinary;
        };
        if reply.endpoint != endpoint || reply.replied {
            return SyncClaim::Foreign;
        }
        reply.replied = true;
        cell.set(Some(reply));
        SyncClaim::Reply(reply.request)
    })
}

struct Item {
    request: Option<u64>,
    header: FrameHeader,
    payload: PayloadBuffer,
    /// Answer `request` with this status instead of handling the frame.
    refuse: Option<vosu_status>,
}

#[derive(Default)]
struct Lane {
    items: VecDeque<Item>,
    closed: bool,
    /// Queued unacknowledged log bytes and drops.
    log_bytes: usize,
    dropped_logs: u64,
}

/// The `SYNC` requests still owed an answer, by budget expiry.
#[derive(Default)]
struct Deadlines {
    pending: BTreeSet<(Instant, u64)>,
    closed: bool,
    /// Requests the timer answered `VOSU_ERR_TIMED_OUT`.
    expired: u64,
}

struct Shared {
    ingress: PluginIngress,
    sender: Sender,
    ledger: Arc<Ledger>,
    gate: Arc<StatusGate>,
    /// The endpoint's identity for [`claim_sync_reply`].
    endpoint: u64,
    sync_budget: Duration,
    ordinary: Mutex<Lane>,
    ordinary_ready: Condvar,
    deadlines: Mutex<Deadlines>,
    deadline_ready: Condvar,
    spare: Mutex<Vec<PayloadBuffer>>,
}

/// One plugin host's ingress workers.
pub(super) struct Ingress {
    shared: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Ingress {
    /// Starts the ordinary worker and the `SYNC` budget timer. `endpoint`
    /// identifies the endpoint whose dispatch claims `SYNC` replies.
    pub(super) fn start(
        ingress: PluginIngress,
        sender: Sender,
        ledger: Arc<Ledger>,
        gate: Arc<StatusGate>,
        endpoint: u64,
        sync_budget: Duration,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            ingress,
            sender,
            ledger,
            gate,
            endpoint,
            sync_budget,
            ordinary: Mutex::new(Lane::default()),
            ordinary_ready: Condvar::new(),
            deadlines: Mutex::new(Deadlines::default()),
            deadline_ready: Condvar::new(),
            spare: Mutex::new(Vec::new()),
        });
        let ingress = Self {
            shared: Arc::clone(&shared),
            threads: Mutex::new(Vec::with_capacity(2)),
        };
        for (name, body) in [
            ("vosu-plugin-ingress", ordinary_worker as fn(&Shared)),
            ("vosu-plugin-sync-timer", sync_timer),
        ] {
            let shared = Arc::clone(&shared);
            let spawned = std::thread::Builder::new()
                .name(name.to_owned())
                .spawn(move || body(&shared));
            match spawned {
                Ok(thread) => lock(&ingress.threads).push(thread),
                Err(error) => {
                    ingress.shutdown();
                    return Err(error);
                }
            }
        }
        Ok(ingress)
    }

    /// Queues one public frame the reader received into `buffer`, which
    /// becomes a spare. `request` is the `ACK_NEXT` id the policy paired
    /// with it. Never blocks on handling.
    pub(super) fn submit(
        &self,
        request: Option<u64>,
        header: FrameHeader,
        buffer: &mut PayloadBuffer,
    ) {
        let shared = &*self.shared;
        if let Some(request) = request
            && shared.ledger.kind(request) == Some(RequestKind::Sync)
        {
            self.submit_sync(request, header, buffer);
            return;
        }
        let len = buffer.bytes().len();
        let mut lane = lock(&shared.ordinary);
        if lane.closed {
            return;
        }
        if request.is_none() {
            if lane.log_bytes + len > LOG_BUDGET {
                lane.dropped_logs += 1;
                return;
            }
            lane.log_bytes += len;
            if lane.dropped_logs > 0 {
                tracing::warn!(
                    dropped = std::mem::take(&mut lane.dropped_logs),
                    "plugin log records dropped under ingress pressure"
                );
            }
        }
        let item = shared.item(request, header, buffer);
        lane.items.push_back(item);
        drop(lane);
        shared.ordinary_ready.notify_one();
    }

    /// Starts `request`'s budget and queues it on the broker workers.
    fn submit_sync(&self, request: u64, header: FrameHeader, buffer: &mut PayloadBuffer) {
        let shared = &*self.shared;
        let deadline = Instant::now() + shared.sync_budget;
        {
            let mut deadlines = lock(&shared.deadlines);
            if deadlines.closed {
                return;
            }
            deadlines.pending.insert((deadline, request));
        }
        shared.deadline_ready.notify_one();
        let item = shared.item(Some(request), header, buffer);
        // Weak: a queued job must not keep a lost channel's ingress alive.
        let weak = Arc::downgrade(&self.shared);
        let job = Box::new(move || run_sync(&weak, item, deadline));
        if shared.ingress.submit_broker_work(job).is_err() {
            // No broker workers: refused by the ordinary worker, which
            // already writes `STATUS` (the reader never does).
            let mut lane = lock(&shared.ordinary);
            if !lane.closed {
                lane.items.push_back(Item {
                    request: Some(request),
                    header,
                    payload: PayloadBuffer::new(),
                    refuse: Some(vosu_status::VOSU_ERR_BACKEND),
                });
                drop(lane);
                shared.ordinary_ready.notify_one();
            }
        }
    }

    /// Stops the ordinary lane and the timer, dropping what they still
    /// hold (the child is gone), and joins both except the calling thread.
    /// A broker job still queued finds the ingress closed and does nothing.
    pub(super) fn shutdown(&self) {
        let items = {
            let mut lane = lock(&self.shared.ordinary);
            lane.closed = true;
            std::mem::take(&mut lane.items)
        };
        drop(items);
        self.shared.ordinary_ready.notify_all();
        lock(&self.shared.deadlines).closed = true;
        self.shared.deadline_ready.notify_all();
        let current = std::thread::current().id();
        for thread in std::mem::take(&mut *lock(&self.threads)) {
            if thread.thread().id() != current {
                let _ = thread.join();
            }
        }
    }

    /// Log records dropped and not yet reported.
    #[cfg(test)]
    pub(super) fn dropped_logs(&self) -> u64 {
        lock(&self.shared.ordinary).dropped_logs
    }

    /// `SYNC` requests the budget timer answered `VOSU_ERR_TIMED_OUT`.
    #[cfg(test)]
    pub(super) fn expired_sync(&self) -> u64 {
        lock(&self.shared.deadlines).expired
    }
}

impl Shared {
    fn item(&self, request: Option<u64>, header: FrameHeader, buffer: &mut PayloadBuffer) -> Item {
        let mut payload = lock(&self.spare).pop().unwrap_or_default();
        std::mem::swap(&mut payload, buffer);
        Item {
            request,
            header,
            payload,
            refuse: None,
        }
    }

    fn recycle(&self, payload: PayloadBuffer) {
        let mut spare = lock(&self.spare);
        if spare.len() < SPARE_BUFFERS && payload.capacity_bytes() <= SPARE_CAPACITY {
            spare.push(payload);
        }
    }

    fn deliver(&self, item: &Item) -> vosu_status {
        self.ingress.deliver(MessageRef {
            opcode: item.header.opcode,
            flags: item.header.flags,
            correlation_id: item.header.correlation,
            payload: item.payload.bytes(),
        })
    }

    /// Closes `request` and writes its `STATUS`, unless another path
    /// already answered it.
    fn answer(&self, request: u64, status: vosu_status) {
        if self.ledger.close(request).is_some() {
            self.write_status(request, status);
        }
    }

    /// Writes the `STATUS` of a request already closed. A failed write
    /// means the channel already ended; the reader reports why.
    fn write_status(&self, request: u64, status: vosu_status) {
        let header = FrameHeader {
            opcode: control::STATUS,
            flags: 0,
            correlation: request,
        };
        if let Err(error) = self.sender.send(&header, &(status as u32).to_le_bytes()) {
            tracing::debug!(%error, "a plugin-host STATUS could not be written");
        }
    }
}

fn ordinary_worker(shared: &Shared) {
    loop {
        let item = {
            let mut lane = lock(&shared.ordinary);
            loop {
                if lane.closed {
                    return;
                }
                if let Some(item) = lane.items.pop_front() {
                    break item;
                }
                lane = shared
                    .ordinary_ready
                    .wait(lane)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        match (item.request, item.refuse) {
            (Some(request), Some(status)) => {
                shared.answer(request, status);
            }
            (Some(request), None) => {
                let _hold = shared.gate.hold();
                let status = shared.deliver(&item);
                shared.answer(request, status);
            }
            (None, _) => {
                let len = item.payload.bytes().len();
                shared.deliver(&item);
                let mut lane = lock(&shared.ordinary);
                lane.log_bytes -= len;
            }
        }
        shared.recycle(item.payload);
    }
}

/// One `SYNC` submission on a broker worker (module docs).
fn run_sync(shared: &Weak<Shared>, item: Item, deadline: Instant) {
    let Some(shared) = shared.upgrade() else {
        return;
    };
    let request = item
        .request
        .expect("only acknowledged SYNC submissions reach the broker");
    let live = !lock(&shared.deadlines).closed && shared.ledger.kind(request).is_some();
    if live {
        SYNC_REPLY.with(|cell| {
            cell.set(Some(SyncReply {
                endpoint: shared.endpoint,
                request,
                replied: false,
            }));
        });
        let status = shared.deliver(&item);
        let replied = SYNC_REPLY
            .with(Cell::take)
            .is_some_and(|reply| reply.replied);
        if !replied {
            // An accepted SYNC plan is answered only by its result: a
            // handler that reported OK without delivering one has nothing
            // to answer with, and the child's policy refuses `STATUS OK`
            // for a SYNC request.
            let status = if status == vosu_status::VOSU_OK {
                vosu_status::VOSU_ERR_BACKEND
            } else {
                status
            };
            shared.answer(request, status);
        }
    }
    lock(&shared.deadlines).pending.remove(&(deadline, request));
    shared.recycle(item.payload);
}

/// Answers every `SYNC` request still open at its budget (module docs).
fn sync_timer(shared: &Shared) {
    let mut deadlines = lock(&shared.deadlines);
    loop {
        if deadlines.closed {
            return;
        }
        let Some(&(at, request)) = deadlines.pending.first() else {
            deadlines = shared
                .deadline_ready
                .wait(deadlines)
                .unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let now = Instant::now();
        if at > now {
            deadlines = shared
                .deadline_ready
                .wait_timeout(deadlines, at - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            continue;
        }
        deadlines.pending.remove(&(at, request));
        if shared.ledger.close(request).is_none() {
            continue;
        }
        deadlines.expired += 1;
        drop(deadlines);
        shared.write_status(request, vosu_status::VOSU_ERR_TIMED_OUT);
        deadlines = lock(&shared.deadlines);
    }
}

#[cfg(test)]
mod tests;
