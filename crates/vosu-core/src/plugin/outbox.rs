//! Capacity-reserved, FIFO terminal delivery on the instance's dispatch workers.
//! Reserve before accepting work. No host or outbox lock spans plugin callbacks.
//!
//! Terminal results of brokered work (asynchronous read plans §6.3, scans
//! §6.4) reserve from their own classes, one per admission cap, so neither
//! competes with action or resource terminals for [`MAX_OUTBOX_SLOTS`]. A
//! work-result slot is released when its message is taken for dispatch,
//! before the plugin sees it: §6.3 frees a plan's slot when its result is
//! delivered, so a plugin that resubmits from inside its result callback,
//! or a process child that frees its local slot on receipt, never meets a
//! slot core still counts.
use super::dispatch::{DispatchTarget, InFlightTracker, Job, OwnedMessage};
use super::dispatch_queue::DispatchQueue;
use super::error::HostError;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::oneshot;
use vosu_plugin_api::vosu_status;

pub const MAX_OUTBOX_SLOTS: usize = 128;
pub type Delivery = oneshot::Receiver<vosu_status>;

/// The reservation classes of one instance's outbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotClass {
    /// Action, relay and resource terminals: [`MAX_OUTBOX_SLOTS`].
    Terminal,
    /// An asynchronous `READ_PLAN_RESULT`: the 64-plan admission cap.
    PlanResult,
    /// A `SCAN_RESULT`: the 64-scan admission cap.
    ScanResult,
    /// An `ARTIFACT_RELEASE`, reserved when its artifact is declared: the
    /// live-artifact bound (AF.12).
    ArtifactRelease,
}

impl SlotClass {
    const fn capacity(self) -> usize {
        match self {
            Self::Terminal => MAX_OUTBOX_SLOTS,
            Self::PlanResult => super::read_plan::MAX_OUTSTANDING_ASYNC_PLANS,
            Self::ScanResult => super::scan::MAX_OUTSTANDING_SCANS,
            Self::ArtifactRelease => crate::actions::artifacts::MAX_LIVE_ARTIFACTS,
        }
    }

    /// Released when the message is taken for dispatch (module docs).
    const fn releases_at_dispatch(self) -> bool {
        !matches!(self, Self::Terminal)
    }
}

pub(crate) struct ReservedOutbox {
    state: Mutex<State>,
    queue: Weak<DispatchQueue>,
    target: DispatchTarget,
    in_flight: InFlightTracker,
}
#[derive(Default)]
struct State {
    /// Reserved [`SlotClass::Terminal`] slots.
    used: usize,
    /// Reserved [`SlotClass::PlanResult`], [`SlotClass::ScanResult`] and
    /// [`SlotClass::ArtifactRelease`] slots.
    work_used: [usize; 3],
    running: bool,
    closed: bool,
    messages: VecDeque<(OwnedMessage, oneshot::Sender<vosu_status>, OutboxSlot)>,
}
impl State {
    fn used_mut(&mut self, class: SlotClass) -> &mut usize {
        match class {
            SlotClass::Terminal => &mut self.used,
            SlotClass::PlanResult => &mut self.work_used[0],
            SlotClass::ScanResult => &mut self.work_used[1],
            SlotClass::ArtifactRelease => &mut self.work_used[2],
        }
    }
}
/// One terminal-delivery reservation. Dropping an unused slot releases capacity.
pub struct OutboxSlot {
    outbox: Weak<ReservedOutbox>,
    class: SlotClass,
}
impl std::fmt::Debug for OutboxSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboxSlot")
            .field("class", &self.class)
            .finish_non_exhaustive()
    }
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
        self.reserve_class(SlotClass::Terminal)
    }
    pub(crate) fn reserve_class(
        self: &Arc<Self>,
        class: SlotClass,
    ) -> Result<OutboxSlot, HostError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed || *state.used_mut(class) == class.capacity() {
            return Err(HostError::DispatchQueueFull);
        }
        *state.used_mut(class) += 1;
        Ok(OutboxSlot {
            outbox: Arc::downgrade(self),
            class,
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
            let held = if slot.class.releases_at_dispatch() {
                drop(slot);
                None
            } else {
                Some(slot)
            };
            let status = message.call(&self.target);
            let _ = reply.send(status);
            drop(held);
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
            *state.used_mut(self.class) -= 1;
        }
    }
}

#[cfg(test)]
mod tests;
