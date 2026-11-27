//! State-delivery slot scheduling, retry admission, and worker draining.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{InstanceSlot, PluginHost, Slot};
use crate::plugin::{
    PluginInstanceId,
    dispatch::{DispatchTarget, OwnedMessage},
};
use crate::state::{SessionKey, split_update};
use vosu_plugin_api::{ActionCancel, Message, VOSU_FRAME_MAX_BYTES};

impl PluginHost {
    pub(super) fn schedule_state_slot(
        &self,
        who: PluginInstanceId,
        key: SessionKey,
        slot: Arc<Mutex<Slot>>,
    ) {
        let terminal = {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            if pending.dead || pending.queued || !pending.dirty() {
                return;
            }
            pending.queued = true;
            pending.detached.is_some()
        };
        self.submit_state_slot(who, key, slot, terminal);
    }

    /// Schedules a slot that now carries `SESSION_DETACHED`. A job already
    /// queued or running delivers it; one waiting to retry admission is
    /// resubmitted at once through the instance's lifecycle lane, which a
    /// full dispatch queue never refuses. Waiting for the retry instead
    /// could find the instance already out of service (a teardown detaches
    /// its sessions, then unpublishes it at once) and lose the detach.
    pub(super) fn schedule_terminal(
        &self,
        who: PluginInstanceId,
        key: SessionKey,
        slot: Arc<Mutex<Slot>>,
    ) {
        let terminal = {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            if std::mem::take(&mut pending.retrying) {
                pending.detached.is_some()
            } else if pending.dead || pending.queued || !pending.dirty() {
                return;
            } else {
                pending.queued = true;
                pending.detached.is_some()
            }
        };
        self.submit_state_slot(who, key, slot, terminal);
    }

    /// Submits the slot's one job. A slot carrying `SESSION_DETACHED` goes
    /// through the lifecycle lane (§7.6 "Lifecycle terminals are reserved"),
    /// keeping its FIFO place behind the session's earlier deliveries;
    /// anything else is ordinary work, retried while the queue is full.
    fn submit_state_slot(
        &self,
        who: PluginInstanceId,
        key: SessionKey,
        slot: Arc<Mutex<Slot>>,
        terminal: bool,
    ) {
        let weak = self.self_ref.clone();
        let job_slot = slot.clone();
        let task = Box::new(move |target: &DispatchTarget| {
            if let Some(host) = weak.upgrade() {
                host.drain_state_slot(who, key, &job_slot, target);
            }
        });
        let result = {
            let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            match instances.get(&who) {
                Some(InstanceSlot::Live(entry)) if terminal => {
                    entry.instance.dispatch_lifecycle(key.0, task)
                }
                Some(InstanceSlot::Live(entry)) => entry.instance.dispatch_task(task),
                _ => {
                    slot.lock().unwrap_or_else(|p| p.into_inner()).dead = true;
                    self.state_delivery
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .slots
                        .remove(&(who, key));
                    return;
                }
            }
        };
        if result.is_ok() {
            return;
        }
        let detach_arrived = {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            // A detach that arrived since `terminal` was read saw this slot
            // queued and left the delivery to this job, which was refused.
            let arrived = !terminal && pending.detached.is_some();
            pending.retrying = !arrived;
            arrived
        };
        if detach_arrived {
            self.submit_state_slot(who, key, slot, true);
            return;
        }
        let weak = self.self_ref.clone();
        let slot = Arc::downgrade(&slot);
        self.state_runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if let (Some(host), Some(slot)) = (weak.upgrade(), slot.upgrade()) {
                let retry = {
                    let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
                    (!pending.dead && std::mem::take(&mut pending.retrying))
                        .then(|| pending.detached.is_some())
                };
                if let Some(terminal) = retry {
                    host.submit_state_slot(who, key, slot, terminal);
                }
            }
        });
    }

    fn drain_state_slot(
        &self,
        who: PluginInstanceId,
        key: SessionKey,
        slot: &Arc<Mutex<Slot>>,
        target: &DispatchTarget,
    ) {
        let (attached, focus, entries, demand, actions, detached) = {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            if pending.dead {
                return;
            }
            let attached = pending.attached.take();
            // Mark before dispatch: a subscription submitted reentrantly from
            // SESSION_ATTACHED must already recognize the handle.
            pending.known |= attached.is_some();
            (
                attached,
                pending.focus.take(),
                pending.pending.take(),
                pending.demand.take(),
                std::mem::take(&mut pending.actions),
                pending.detached.take(),
            )
        };
        let send = |message: Message| {
            if let Ok(message) = OwnedMessage::from_message(&message, 0) {
                message.call(target);
            }
        };
        if let Some(attached) = attached {
            send(Message::SessionAttached(attached));
        }
        if let Some(focus) = focus {
            send(Message::FocusChanged(focus));
        }
        if !entries.is_empty() {
            for update in split_update(key.0, entries, VOSU_FRAME_MAX_BYTES as usize)
                .expect("ABI cap fits errors")
            {
                send(Message::StateUpdate(update));
            }
        }
        if let Some(demand) = demand {
            let demand = vosu_plugin_api::DemandChanged {
                handle: key.0,
                regular: demand.regular,
                precise: demand.precise,
                selectors: demand
                    .selectors
                    .into_iter()
                    .map(|(ns, pointer)| vosu_plugin_api::FieldSelector {
                        ns: ns.to_string(),
                        pointer: pointer.to_string(),
                    })
                    .collect(),
            };
            send(Message::DemandChanged(demand));
        }
        for invocation in actions {
            if let Some(message) = self.take_queued_action(invocation) {
                message.call(target);
                if self.mark_action_dispatched(invocation) {
                    send(Message::ActionCancel(ActionCancel { invocation }));
                }
            }
        }
        if let Some(detached) = detached {
            send(Message::SessionDetached(detached));
            slot.lock().unwrap_or_else(|p| p.into_inner()).dead = true;
            self.retire_state_slot(who, key, slot);
            return;
        }
        let (detach_arrived, resubmit) = {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            if pending.dead {
                (false, false)
            } else if pending.detached.is_some() {
                (true, false)
            } else if pending.dirty() {
                (false, true)
            } else {
                pending.queued = false;
                (false, false)
            }
        };
        if detach_arrived {
            // A detach that ran while this job did found the slot queued and
            // left `SESSION_DETACHED` to this job. It goes out now, after what
            // was merged meanwhile, ending the slot: a resubmission could find
            // the instance already out of service (a teardown detaches its
            // sessions, then unpublishes it at once) and never deliver it.
            self.drain_state_slot(who, key, slot, target);
            return;
        }
        if resubmit {
            self.submit_state_slot(who, key, slot.clone(), false);
        }
    }
}
