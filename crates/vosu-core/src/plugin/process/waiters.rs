//! The plugin host's status and `SYNC` waiters (`docs/v2/plugin-abi.md`
//! §7.6 "Acknowledged sends", A5.2).
//!
//! A `send` whose status only core can decide reserves one waiter before
//! anything is written: its request id (allocated here, strictly
//! increasing and never reused within the generation) is opened in the
//! channel's [`Ledger`], which caps live waiters at [`MAX_REQUESTS`],
//! shared by status and `SYNC` requests. The calling thread then writes
//! `ACK_NEXT` and its frame, releases the producer lock, and blocks here.
//!
//! The ring reader completes a waiter:
//!
//! - `STATUS` — [`Waiters::complete_status`] runs the waiter's [`Settle`]
//!   before the waiting thread can observe the status, so local admission
//!   and delivery state already reflect core's answer when `send` returns
//!   and before the reader routes the next frame;
//! - `REPLY_NEXT` and its `READ_PLAN_RESULT` — [`Waiters::complete_result`]
//!   hands the reader's aligned buffer to the waiter and gives the reader a
//!   spare one in exchange, so the result is copied out of the ring exactly
//!   once. The waiting thread dispatches it before `send` returns.
//!
//! A reply naming an id that is not waiting (never issued, or released
//! after a local failure) is discarded and counted, never delivered: ids
//! are never reused, so a late reply cannot satisfy a newer waiter.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use vosu_plugin_api::vosu_status;

use super::admission::Settle;
use super::control::{EXIT_CHANNEL_LOST, EXIT_TRANSPORT_TIMEOUT};
use super::frame::{FrameHeader, Ledger, MAX_REQUESTS, RequestKind};
use super::ring::PayloadBuffer;

/// Core's answer to one acknowledged send.
#[derive(Debug)]
pub(crate) enum Reply {
    Status(vosu_status),
    /// An accepted `SYNC` plan's `READ_PLAN_RESULT`, still to be
    /// dispatched on the submitting thread.
    Result {
        header: FrameHeader,
        payload: PayloadBuffer,
    },
}

#[derive(Debug)]
struct Slot {
    settle: Settle,
    reply: Option<Reply>,
}

#[derive(Debug, Default)]
struct State {
    /// The last request id handed out; ids start at 1 and only increase.
    last_id: u64,
    slots: HashMap<u64, Slot>,
    /// Result buffers returned after dispatch, reused by later `SYNC`
    /// replies: at most [`MAX_REQUESTS`], each at most 64 KiB.
    spare: Vec<PayloadBuffer>,
    /// Replies for an id nobody waits on.
    discarded: u64,
    /// The channel ended: every waiter gives up.
    closed: bool,
}

pub(crate) struct Waiters {
    ledger: Arc<Ledger>,
    state: Mutex<State>,
    changed: Condvar,
}

impl std::fmt::Debug for Waiters {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Waiters")
            .field("ledger", &self.ledger)
            .finish_non_exhaustive()
    }
}

impl Waiters {
    /// Waiters over the channel's `ledger` (the one its receiving policy
    /// pairs replies with).
    pub(crate) fn new(ledger: Arc<Ledger>) -> Self {
        Self {
            ledger,
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Each critical section leaves the map consistent before any call
        // that could panic.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserves a waiter of `kind`, or hands `settle` back when
    /// [`MAX_REQUESTS`] are already waiting (the overload refusal, before
    /// any write) or the channel ended.
    pub(crate) fn reserve(&self, kind: RequestKind, settle: Settle) -> Result<u64, Settle> {
        let mut state = self.lock();
        let id = state.last_id + 1;
        if state.closed || !self.ledger.open(id, kind) {
            return Err(settle);
        }
        state.last_id = id;
        state.slots.insert(
            id,
            Slot {
                settle,
                reply: None,
            },
        );
        Ok(id)
    }

    /// Gives up a waiter whose frame was never answered: its write failed,
    /// or its backstop expired. A later reply for it is discarded.
    pub(crate) fn release(&self, id: u64) {
        let mut state = self.lock();
        if let Some(Slot {
            reply: Some(Reply::Result { payload, .. }),
            ..
        }) = state.slots.remove(&id)
        {
            recycle(&mut state, payload);
        }
        drop(state);
        self.ledger.close(id);
    }

    /// Routes a `STATUS` (ring reader only). `settle` runs with the
    /// waiter's [`Settle`] before the waiting thread can see the status.
    /// Returns false when nobody waits on `id`.
    pub(crate) fn complete_status(
        &self,
        id: u64,
        status: vosu_status,
        settle: impl FnOnce(&Settle, vosu_status),
    ) -> bool {
        let mut state = self.lock();
        let Some(slot) = state.slots.get_mut(&id).filter(|slot| slot.reply.is_none()) else {
            state.discarded += 1;
            return false;
        };
        settle(&slot.settle, status);
        slot.reply = Some(Reply::Status(status));
        drop(state);
        self.ledger.close(id);
        self.changed.notify_all();
        true
    }

    /// Routes the `READ_PLAN_RESULT` a `REPLY_NEXT` named (ring reader
    /// only): `buffer`'s bytes move to the waiter and `buffer` becomes a
    /// spare. Returns false, leaving `buffer` untouched, when nobody waits
    /// on `id`.
    pub(crate) fn complete_result(
        &self,
        id: u64,
        header: FrameHeader,
        buffer: &mut PayloadBuffer,
    ) -> bool {
        let mut state = self.lock();
        let waiting = state
            .slots
            .get(&id)
            .is_some_and(|slot| slot.reply.is_none());
        if !waiting {
            state.discarded += 1;
            return false;
        }
        let mut payload = state.spare.pop().unwrap_or_default();
        std::mem::swap(&mut payload, buffer);
        if let Some(slot) = state.slots.get_mut(&id) {
            slot.reply = Some(Reply::Result { header, payload });
        }
        drop(state);
        self.ledger.close(id);
        self.changed.notify_all();
        true
    }

    /// Returns a dispatched `SYNC` result buffer for reuse.
    pub(crate) fn recycle(&self, payload: PayloadBuffer) {
        recycle(&mut self.lock(), payload);
    }

    /// Ends every wait: the channel is gone.
    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.changed.notify_all();
    }

    /// Waits for `id`'s reply until `deadline`. `Err` carries the exit
    /// status when the channel ended or the backstop expired; the waiter
    /// stays reserved until [`release`](Self::release).
    pub(crate) fn wait(&self, id: u64, deadline: Instant) -> Result<Reply, i32> {
        let mut state = self.lock();
        loop {
            if let Some(slot) = state.slots.get_mut(&id)
                && let Some(reply) = slot.reply.take()
            {
                state.slots.remove(&id);
                return Ok(reply);
            }
            if state.closed {
                return Err(EXIT_CHANNEL_LOST);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(EXIT_TRANSPORT_TIMEOUT);
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Replies that named no waiting id.
    #[cfg(test)]
    pub(crate) fn discarded(&self) -> u64 {
        self.lock().discarded
    }

    /// Waiters currently reserved.
    #[cfg(test)]
    pub(crate) fn reserved(&self) -> usize {
        self.lock().slots.len()
    }
}

fn recycle(state: &mut State, mut payload: PayloadBuffer) {
    if state.spare.len() < MAX_REQUESTS
        && payload.capacity_bytes() <= vosu_plugin_api::VOSU_READ_PLAN_MAX_BYTES as usize
    {
        payload.fill(&[]);
        state.spare.push(payload);
    }
}

#[cfg(test)]
mod tests;
