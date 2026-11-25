//! Capacity-reserved, FIFO terminal delivery on the instance's dispatch workers.
//! Reserve before accepting work. No host or outbox lock spans plugin callbacks.
use super::dispatch::{DispatchTarget, InFlightTracker, Job, OwnedMessage};
use super::dispatch_queue::DispatchQueue;
use super::error::HostError;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::oneshot;
use vosu_plugin_api::vosu_status;

pub const MAX_OUTBOX_SLOTS: usize = 128;
pub type Delivery = oneshot::Receiver<vosu_status>;

pub(crate) struct ReservedOutbox {
    state: Mutex<State>,
    queue: Weak<DispatchQueue>,
    target: DispatchTarget,
    in_flight: InFlightTracker,
}
#[derive(Default)]
struct State {
    used: usize,
    running: bool,
    closed: bool,
    messages: VecDeque<(OwnedMessage, oneshot::Sender<vosu_status>, OutboxSlot)>,
}
/// One terminal-delivery reservation. Dropping an unused slot releases capacity.
pub struct OutboxSlot {
    outbox: Weak<ReservedOutbox>,
}
impl ReservedOutbox {
    pub(crate) fn new(
        queue: Arc<DispatchQueue>,
        target: DispatchTarget,
        in_flight: InFlightTracker,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            queue: Arc::downgrade(&queue),
            target,
            in_flight,
        })
    }
    pub(crate) fn reserve(self: &Arc<Self>) -> Result<OutboxSlot, HostError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed || state.used == MAX_OUTBOX_SLOTS {
            return Err(HostError::DispatchQueueFull);
        }
        state.used += 1;
        Ok(OutboxSlot {
            outbox: Arc::downgrade(self),
        })
    }
    pub(crate) fn close(&self) {
        let messages = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.closed = true;
            std::mem::take(&mut state.messages)
        };
        drop(messages);
    }
    fn drain(&self) {
        loop {
            let next = {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                match state.messages.pop_front() {
                    Some(message) => Some(message),
                    None => {
                        state.running = false;
                        None
                    }
                }
            };
            let Some((message, reply, slot)) = next else {
                return;
            };
            let status = message.call(&self.target);
            let _ = reply.send(status);
            drop(slot);
        }
    }
}
impl OutboxSlot {
    /// The returned receiver resolves after dispatch, not merely after enqueue.
    pub fn send(self, message: OwnedMessage) -> Delivery {
        let (reply, delivered) = oneshot::channel();
        let Some(outbox) = self.outbox.upgrade() else {
            return delivered;
        };
        let schedule = {
            let mut state = outbox.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.closed {
                return delivered;
            }
            state.messages.push_back((message, reply, self));
            if state.running {
                false
            } else {
                state.running = true;
                true
            }
        };
        if schedule {
            if let Some(queue) = outbox.queue.upgrade() {
                let weak = Arc::downgrade(&outbox);
                queue.submit_reserved(Job {
                    target: outbox.target.clone(),
                    message: OwnedMessage {
                        opcode: 0,
                        flags: 0,
                        correlation_id: 0,
                        payload: Vec::new(),
                    },
                    task: Some(Box::new(move |_| {
                        if let Some(outbox) = weak.upgrade() {
                            outbox.drain();
                        }
                    })),
                    reply: None,
                    in_flight: Some(outbox.in_flight.enter()),
                });
            } else {
                outbox.close();
            }
        }
        delivered
    }
}
impl Drop for OutboxSlot {
    fn drop(&mut self) {
        if let Some(outbox) = self.outbox.upgrade() {
            let mut state = outbox.state.lock().unwrap_or_else(|p| p.into_inner());
            state.used -= 1;
        }
    }
}

#[cfg(test)]
mod tests;
