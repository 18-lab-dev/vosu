//! The plugin host's relay to core (`docs/v2/plugin-abi.md` §4, §7.2,
//! §7.6 "Acknowledged sends"): the plugin's `send`, the ring reader and
//! the dispatch workers of one instance.
//!
//! **`send`.** Applies the in-process pre-handler checks locally (private
//! or unknown opcode, unrecognized flags, SDK decode, a `SYNC` submission
//! from a dispatch thread) and the per-instance caps
//! ([`super::admission`]), so a refused message writes nothing. A host →
//! plugin opcode a plugin sends has no effect in-process and returns
//! `VOSU_OK` without a write. `VOSU_OP_LOG_RECORD` is written
//! unacknowledged. Every other message reserves one of the 64 shared
//! waiters ([`super::waiters`]), is written as `ACK_NEXT` plus the
//! unmodified frame under the one producer lock, and, with that lock
//! released, blocks until core's reply: the plugin sees core's own status,
//! and a refused request produces no later result. An accepted `SYNC`
//! plan's result is dispatched by the submitting thread itself before
//! `send` returns, concurrently with whatever the dispatch workers run. A
//! waiter past its backstop (2 s, or the `SYNC` budget plus 2 s) means core
//! stopped serving the channel: the host closes it and exits rather than
//! fabricate a status.
//!
//! **Ring reader.** Only validates (the channel's policy), routes and
//! moves buffers; it never calls plugin code, never waits on a plugin
//! lock, and never blocks on a full queue:
//!
//! - `STATUS` and `REPLY_NEXT` results go to their waiter;
//! - public frames update local admission, then go to the delivery queue
//!   ([`super::delivery`]);
//! - `DESTROY` closes the queue and wakes the lifecycle.
//!
//! **Dispatch.** One worker, or four under `concurrent_dispatch` (the
//! same pool size core gives in-process `concurrent_dispatch` plugins),
//! drains the delivery queue independently of the reader, each call under
//! the dispatch-thread scope that refuses a reentrant `SYNC` submission.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use vosu_plugin_api::{Message, VOSU_READ_PLAN_SYNC, validate_flags, vosu_opcode, vosu_status};

use super::admission::{Admission, Route, Settle, route};
use super::channel::{Closer, Receiver, Sender};
use super::control::EXIT_CHANNEL_LOST;
use super::delivery::{DeliveryQueue, inspect};
use super::frame::{FrameHeader, PRIVATE_OPCODE_BIT, RequestKind, control};
use super::ring::PayloadBuffer;
use super::waiters::{Reply, Waiters};
use crate::plugin::execution::DispatchTarget;
use crate::plugin::read_plan::{DispatchThreadScope, on_dispatch_thread};

/// A status waiter's backstop after its write completed (A5.2).
pub(crate) const STATUS_BACKSTOP: Duration = Duration::from_secs(2);
/// Dispatch workers under `concurrent_dispatch`.
pub(crate) const CONCURRENT_WORKERS: usize = 4;

/// What ends the plugin host when its channel is unusable: closes nothing
/// itself (the relay already closed the channel) and, in production, never
/// returns. Tests substitute a recorder.
pub(crate) type Fatal = Box<dyn Fn(i32) -> vosu_status + Send + Sync>;

/// One instance's relay. The plugin's `host_ctx` points at it, so it lives
/// as long as any plugin thread can call `send`.
pub(crate) struct Relay {
    sender: Sender,
    closer: Closer,
    waiters: Waiters,
    admission: Admission,
    delivery: DeliveryQueue,
    target: OnceLock<DispatchTarget>,
    sync_backstop: Duration,
    fatal: Fatal,
}

impl Relay {
    /// A relay over one attached channel. `waiters` shares the ledger of
    /// that channel's receiving policy; `sync_budget` is `CREATE`'s
    /// `sync_budget_ms`.
    pub(crate) fn new(
        sender: Sender,
        closer: Closer,
        waiters: Waiters,
        sync_budget: Duration,
        fatal: Fatal,
    ) -> Self {
        Self::with_delivery(
            sender,
            closer,
            waiters,
            DeliveryQueue::default(),
            sync_budget,
            fatal,
        )
    }

    pub(crate) fn with_delivery(
        sender: Sender,
        closer: Closer,
        waiters: Waiters,
        delivery: DeliveryQueue,
        sync_budget: Duration,
        fatal: Fatal,
    ) -> Self {
        Self {
            sender,
            closer,
            waiters,
            admission: Admission::default(),
            delivery,
            target: OnceLock::new(),
            sync_backstop: sync_budget.saturating_add(STATUS_BACKSTOP),
            fatal,
        }
    }

    /// Sets the plugin's dispatch target once `create` returned.
    pub(crate) fn set_target(&self, target: DispatchTarget) {
        let _ = self.target.set(target);
    }

    pub(crate) fn closer(&self) -> &Closer {
        &self.closer
    }

    #[cfg(test)]
    pub(crate) fn admission(&self) -> &Admission {
        &self.admission
    }

    #[cfg(test)]
    pub(crate) fn delivery(&self) -> &DeliveryQueue {
        &self.delivery
    }

    #[cfg(test)]
    pub(crate) fn waiters(&self) -> &Waiters {
        &self.waiters
    }

    /// Closes the channel, ends every wait and delivery, and hands `code`
    /// to the fatal hook (§7.6 backstops, A5.8).
    fn fail(&self, code: i32) -> vosu_status {
        let _ = self.closer.close();
        self.waiters.close();
        self.delivery.abort();
        (self.fatal)(code)
    }

    /// The plugin's `send`, after the trampoline's pointer checks.
    pub(crate) fn send(
        &self,
        opcode: u32,
        flags: u32,
        correlation: u64,
        payload: &[u8],
    ) -> vosu_status {
        if opcode & PRIVATE_OPCODE_BIT != 0 {
            return vosu_status::VOSU_ERR_UNSUPPORTED;
        }
        let Ok(known) = vosu_opcode::try_from(opcode) else {
            return vosu_status::VOSU_ERR_UNSUPPORTED;
        };
        if validate_flags(known, flags).is_err() {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let Ok(message) = Message::decode(known, flags, payload) else {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        };
        let header = FrameHeader {
            opcode,
            flags,
            correlation,
        };
        match route(known) {
            Route::NoEffect => vosu_status::VOSU_OK,
            Route::Unacknowledged => match self.sender.send(&header, payload) {
                Ok(()) => vosu_status::VOSU_OK,
                Err(_) => self.fail(EXIT_CHANNEL_LOST),
            },
            Route::Acknowledged => {
                let sync = known == vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT
                    && flags & VOSU_READ_PLAN_SYNC != 0;
                self.acknowledged(&header, payload, &message, sync)
            }
        }
    }

    fn acknowledged(
        &self,
        header: &FrameHeader,
        payload: &[u8],
        message: &Message,
        sync: bool,
    ) -> vosu_status {
        let (kind, settle, backstop) = if sync {
            // §6.3: a dispatch thread may never block on its own SYNC read.
            if on_dispatch_thread() {
                return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
            }
            (RequestKind::Sync, Settle::Nothing, self.sync_backstop)
        } else {
            match self.admission.reserve(message, header.correlation) {
                Ok(settle) => (RequestKind::Status, settle, STATUS_BACKSTOP),
                Err(status) => return status,
            }
        };
        let id = match self.waiters.reserve(kind, settle) {
            Ok(id) => id,
            Err(settle) => {
                // The documented overload status, before any write.
                self.admission
                    .settle(&settle, vosu_status::VOSU_ERR_INVALID_ARGUMENT);
                return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
            }
        };
        if self.sender.send_acknowledged(id, header, payload).is_err() {
            self.waiters.release(id);
            return self.fail(EXIT_CHANNEL_LOST);
        }
        let deadline = Instant::now() + backstop;
        match self.waiters.wait(id, deadline) {
            Ok(Reply::Status(status)) => status,
            Ok(Reply::Result { header, payload }) => self.dispatch_sync(header, payload),
            Err(code) => {
                self.waiters.release(id);
                self.fail(code)
            }
        }
    }

    /// Dispatches an accepted `SYNC` plan's result on the submitting
    /// thread, as the in-process host does.
    fn dispatch_sync(&self, header: FrameHeader, payload: PayloadBuffer) -> vosu_status {
        let status = match self.target.get() {
            Some(target) => {
                let _dispatch_thread = DispatchThreadScope::enter();
                target.dispatch(
                    header.opcode,
                    header.flags,
                    header.correlation,
                    payload.bytes(),
                );
                vosu_status::VOSU_OK
            }
            // Core answers no SYNC plan before READY; a result before
            // `create` returned has no plugin to reach, as in-process.
            None => vosu_status::VOSU_ERR_NOT_READY,
        };
        self.waiters.recycle(payload);
        status
    }

    /// The ring reader, until the channel ends. `on_destroy` runs once for
    /// `DESTROY`. Returns whether the channel was lost (anything other
    /// than the local close after `DESTROYED`): every waiter and delivery
    /// is then ended.
    pub(crate) fn read(&self, mut receiver: Receiver, mut on_destroy: impl FnMut()) -> bool {
        let mut buffer = PayloadBuffer::new();
        let mut reply_to: Option<u64> = None;
        let lost = loop {
            let Ok(header) = receiver.recv(&mut buffer) else {
                break true;
            };
            if let Some(id) = reply_to.take() {
                // The policy admitted exactly the READ_PLAN_RESULT this
                // REPLY_NEXT announced; nobody waiting means it is
                // discarded and counted.
                self.waiters.complete_result(id, header, &mut buffer);
                continue;
            }
            let routed = match header.opcode {
                control::STATUS => {
                    let bytes = buffer.bytes();
                    let raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    // The policy admitted only a real status.
                    let status =
                        vosu_status::try_from(raw).unwrap_or(vosu_status::VOSU_ERR_BACKEND);
                    self.waiters
                        .complete_status(header.correlation, status, |settle, status| {
                            self.admission.settle(settle, status);
                            if let Settle::Resubscribe(session) = *settle
                                && status == vosu_status::VOSU_OK
                            {
                                self.delivery.reset_state(session);
                            }
                        });
                    Ok(())
                }
                control::REPLY_NEXT => {
                    reply_to = Some(header.correlation);
                    Ok(())
                }
                control::DESTROY => {
                    if self.delivery.close() {
                        on_destroy();
                        Ok(())
                    } else {
                        Err("a second DESTROY".to_owned())
                    }
                }
                // A second CREATE: lifecycle frames come once.
                _ if header.is_private() => {
                    Err(format!("unexpected private frame {:#x}", header.opcode))
                }
                _ => inspect(&header, buffer.bytes())
                    .and_then(|inbound| {
                        self.admission.observe(inbound.release);
                        self.delivery.push(header, &mut buffer, inbound)
                    })
                    .map_err(|error| error.to_string()),
            };
            if let Err(reason) = routed {
                tracing::debug!(%reason, "core broke the plugin-host protocol");
                break true;
            }
        };
        self.waiters.close();
        if lost {
            self.delivery.abort();
            let _ = self.closer.close();
        }
        lost
    }

    /// One dispatch worker: delivers until the queue ends.
    pub(crate) fn work(&self) {
        let Some(target) = self.target.get() else {
            return;
        };
        let mut scratch = PayloadBuffer::new();
        while let Some(mut delivery) = self.delivery.next() {
            {
                // §4/D5: a SYNC submission from inside this call is refused
                // instead of blocking this worker on its own result.
                let _dispatch_thread = DispatchThreadScope::enter();
                delivery.dispatch(target, &mut scratch);
            }
            self.delivery.done(delivery);
        }
    }
}

#[cfg(test)]
mod tests;
