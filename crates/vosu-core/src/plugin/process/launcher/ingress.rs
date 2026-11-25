//! Core's plugin → host direction for one plugin host
//! (`docs/v2/plugin-abi.md` §7.6 "Acknowledged sends", A5.2).
//!
//! The ring reader never handles a message itself: handling can take
//! host locks, and a reply it wrote from the reader could park on a full
//! host → plugin ring that only this side's reader would ever announce
//! space on. It moves each public frame's aligned buffer (no copy) into
//! one of two lanes:
//!
//! - **ordinary** — every acknowledged frame except a `SYNC` read plan,
//!   and every `VOSU_OP_LOG_RECORD`, handled in ring order by one worker
//!   through [`PluginIngress::deliver`] (the in-process validation and
//!   routing). An acknowledged frame is answered with its `STATUS`.
//!   Acknowledged frames are bounded by the 64 live requests the channel's
//!   policy admits; unacknowledged log records are bounded by
//!   [`LOG_BUDGET`] queued bytes, past which a record is dropped and
//!   counted (§6.10 already drops records past its rate limit).
//! - **sync** — `VOSU_READ_PLAN_SYNC` submissions, each handled on one of
//!   up to [`MAX_REQUESTS`] workers started on demand, so a slow plan never
//!   delays ordinary traffic or another `SYNC` caller. The handler
//!   delivers an accepted plan's result through the instance's dispatch
//!   target on this same thread, which the endpoint turns into
//!   `REPLY_NEXT` plus the unmodified `READ_PLAN_RESULT`
//!   ([`claim_sync_reply`]); a refusal is answered with its `STATUS`.
//!
//! **Reply order.** Every request is closed in the channel's [`Ledger`]
//! *before* its reply is written, so the child can never meet a full
//! ledger with a request core already answered. While the ordinary worker
//! handles a request and writes its `STATUS`, it holds the endpoint's
//! [`StatusGate`]: any frame another thread dispatches to this child waits
//! until that `STATUS` is on the ring, so an accepted request's `STATUS`
//! always precedes the results it causes (§7.6).

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{JoinHandle, ThreadId};

use vosu_plugin_api::vosu_status;

use super::super::channel::Sender;
use super::super::frame::{FrameHeader, Ledger, MAX_REQUESTS, RequestKind, control};
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
    endpoint: usize,
    request: u64,
    replied: bool,
}

thread_local! {
    /// The `SYNC` request this thread is handling, if any.
    static SYNC_REPLY: Cell<Option<SyncReply>> = const { Cell::new(None) };
}

/// Called by endpoint `endpoint`'s dispatch for a `READ_PLAN_RESULT`: the
/// `SYNC` request this thread is answering, claimed once, or `None` for an
/// ordinary (asynchronous) result.
pub(super) fn claim_sync_reply(endpoint: usize) -> Option<u64> {
    SYNC_REPLY.with(|cell| {
        let mut reply = cell.get()?;
        if reply.endpoint != endpoint || reply.replied {
            return None;
        }
        reply.replied = true;
        cell.set(Some(reply));
        Some(reply.request)
    })
}

struct Item {
    request: Option<u64>,
    header: FrameHeader,
    payload: PayloadBuffer,
}

#[derive(Default)]
struct Lane {
    items: VecDeque<Item>,
    closed: bool,
    /// Ordinary lane: queued unacknowledged log bytes and drops.
    log_bytes: usize,
    dropped_logs: u64,
    /// Sync lane: workers started and those waiting for an item.
    workers: usize,
    idle: usize,
}

struct Shared {
    ingress: PluginIngress,
    sender: Sender,
    ledger: Arc<Ledger>,
    gate: Arc<StatusGate>,
    /// The endpoint's identity for [`claim_sync_reply`].
    endpoint: usize,
    ordinary: Mutex<Lane>,
    ordinary_ready: Condvar,
    sync: Mutex<Lane>,
    sync_ready: Condvar,
    spare: Mutex<Vec<PayloadBuffer>>,
}

/// One plugin host's ingress workers.
pub(super) struct Ingress {
    shared: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Ingress {
    /// Starts the ordinary worker. `endpoint` identifies the endpoint
    /// whose dispatch claims `SYNC` replies.
    pub(super) fn start(
        ingress: PluginIngress,
        sender: Sender,
        ledger: Arc<Ledger>,
        gate: Arc<StatusGate>,
        endpoint: usize,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            ingress,
            sender,
            ledger,
            gate,
            endpoint,
            ordinary: Mutex::new(Lane::default()),
            ordinary_ready: Condvar::new(),
            sync: Mutex::new(Lane::default()),
            sync_ready: Condvar::new(),
            spare: Mutex::new(Vec::new()),
        });
        let worker = std::thread::Builder::new()
            .name("vosu-plugin-ingress".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || ordinary_worker(&shared)
            })?;
        Ok(Self {
            shared,
            threads: Mutex::new(vec![worker]),
        })
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
        let len = buffer.bytes().len();
        let sync = request.is_some_and(|id| shared.ledger.kind(id) == Some(RequestKind::Sync));
        if sync {
            let item = shared.item(request, header, buffer);
            let spawn = {
                let mut lane = lock(&shared.sync);
                if lane.closed {
                    return;
                }
                lane.items.push_back(item);
                let spawn = lane.idle == 0 && lane.workers < MAX_REQUESTS;
                if spawn {
                    lane.workers += 1;
                }
                spawn
            };
            shared.sync_ready.notify_one();
            if spawn {
                self.spawn_sync_worker();
            }
            return;
        }
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

    fn spawn_sync_worker(&self) {
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("vosu-plugin-sync".to_owned())
            .spawn(move || sync_worker(&shared));
        match spawned {
            Ok(worker) => lock(&self.threads).push(worker),
            Err(error) => {
                // The queued item waits for a running worker; with none,
                // the child's backstop ends the channel.
                lock(&self.shared.sync).workers -= 1;
                tracing::warn!(%error, "could not start a plugin SYNC worker");
            }
        }
    }

    /// Stops both lanes, dropping what they still hold (the child is gone),
    /// and joins every worker except the calling thread.
    pub(super) fn shutdown(&self) {
        for (lane, ready) in [
            (&self.shared.ordinary, &self.shared.ordinary_ready),
            (&self.shared.sync, &self.shared.sync_ready),
        ] {
            let items = {
                let mut lane = lock(lane);
                lane.closed = true;
                std::mem::take(&mut lane.items)
            };
            drop(items);
            ready.notify_all();
        }
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

    /// `SYNC` workers started.
    #[cfg(test)]
    pub(super) fn sync_workers(&self) -> usize {
        lock(&self.shared.sync).workers
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

    /// Closes `request` and writes its `STATUS`. A failed write means the
    /// channel already ended; the reader reports why.
    fn answer(&self, request: u64, status: vosu_status) {
        self.ledger.close(request);
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

fn next(lane: &Mutex<Lane>, ready: &Condvar, sync: bool) -> Option<Item> {
    let mut state = lock(lane);
    loop {
        if state.closed {
            return None;
        }
        if let Some(item) = state.items.pop_front() {
            return Some(item);
        }
        if sync {
            state.idle += 1;
        }
        state = ready.wait(state).unwrap_or_else(PoisonError::into_inner);
        if sync {
            state.idle -= 1;
        }
    }
}

fn ordinary_worker(shared: &Shared) {
    while let Some(item) = next(&shared.ordinary, &shared.ordinary_ready, false) {
        match item.request {
            Some(request) => {
                let _hold = shared.gate.hold();
                let status = shared.deliver(&item);
                shared.answer(request, status);
            }
            None => {
                let len = item.payload.bytes().len();
                shared.deliver(&item);
                let mut lane = lock(&shared.ordinary);
                lane.log_bytes -= len;
            }
        }
        shared.recycle(item.payload);
    }
}

fn sync_worker(shared: &Shared) {
    while let Some(item) = next(&shared.sync, &shared.sync_ready, true) {
        let request = item
            .request
            .expect("only acknowledged SYNC submissions reach the sync lane");
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
        shared.recycle(item.payload);
    }
}

#[cfg(test)]
mod tests;
