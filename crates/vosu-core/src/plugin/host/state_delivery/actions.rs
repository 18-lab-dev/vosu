use super::{InstanceSlot, OwnedMessage, PluginHost, SessionHandle, SessionKey};
use crate::actions::{InvocationError, InvocationId};
use crate::plugin::PluginInstanceId;
use crate::plugin::host::actions::MAX_INVOCATIONS;

impl PluginHost {
    /// A guest action uses the lifecycle slot, even when a watch attachment
    /// is running concurrently or waiting for room on the dispatch queue.
    pub(in crate::plugin::host) fn queue_guest_action(
        &self,
        who: PluginInstanceId,
        handle: SessionHandle,
        invocation: InvocationId,
        message: OwnedMessage,
    ) -> Result<(), InvocationError> {
        let _serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
        let key = SessionKey(handle.0);
        let slot = {
            let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            if !matches!(instances.get(&who), Some(InstanceSlot::Live(_))) {
                return Err(InvocationError::Unavailable);
            }
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            if !sessions.is_identified(handle) {
                return Err(InvocationError::InvalidSession);
            }
            let mut delivery = self
                .state_delivery
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let ending = delivery.ending.contains(&who);
            let mut actions = self.action_state.lock().unwrap_or_else(|p| p.into_inner());
            if !actions.pending.contains_key(&invocation) {
                // The invocation already timed out or lost its caller.
                return Ok(());
            }
            let slot = delivery.slots.entry((who, key)).or_default().clone();
            {
                let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
                if pending.dead || (pending.lifecycle && ending) {
                    // Do not overtake a retiring watch's terminal.
                    return Err(InvocationError::Unavailable);
                }
                if !pending.known && pending.attached.is_none() {
                    // The registry uses the same numeric handle for each
                    // recipient; the slot records which instance knows it.
                    pending.attached = sessions.public_attachment(handle);
                }
                if pending.actions.len() == MAX_INVOCATIONS {
                    pending
                        .actions
                        .retain(|id| actions.pending.contains_key(id));
                }
                pending.actions.push(invocation);
            }
            actions
                .pending
                .get_mut(&invocation)
                .expect("reserved")
                .queued_message = Some(message);
            slot
        };
        self.schedule_state_slot(who, key, slot);
        Ok(())
    }
}
