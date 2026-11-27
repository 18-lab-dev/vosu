use super::{InstanceSlot, PluginHost, SessionKey, vosu_status};
use crate::plugin::PluginInstanceId;

impl PluginHost {
    /// Starts or replays the lifecycle watch under `state_serial`. During
    /// `create` only remember the request: no attachment or demand is sent
    /// until the instance is published, and failed creates drop the bit.
    pub(in crate::plugin::host) fn subscribe_sessions_watch(
        &self,
        who: PluginInstanceId,
    ) -> vosu_status {
        let plugin = {
            let mut instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            match instances.get_mut(&who) {
                Some(InstanceSlot::Creating { watch_sessions, .. }) => {
                    *watch_sessions = true;
                    return vosu_status::VOSU_OK;
                }
                Some(InstanceSlot::Live(entry)) => entry.plugin_id.clone(),
                _ => return vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            }
        };
        let attachments = {
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let mut delivery = self
                .state_delivery
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if delivery.ending.contains(&who) {
                delivery.restart.insert(who);
                return vosu_status::VOSU_OK;
            }
            delivery.watchers.insert(who);
            self.state_watcher_count.store(
                delivery.watchers.len(),
                std::sync::atomic::Ordering::Release,
            );
            // Identification is recorded before `open_bus_session`. A
            // session not yet open would refuse the watcher's immediate
            // subscription; `state_session_identified` announces it to
            // this (now registered) watcher once its bus state exists.
            sessions
                .identified_sessions()
                .filter(|(_, owner, _)| owner.as_str() != plugin)
                .filter(|(handle, _, _)| self.state.store.current(SessionKey(handle.0)).is_some())
                .filter_map(|(handle, _, _)| sessions.public_attachment(handle))
                .collect::<Vec<_>>()
        };
        for attached in attachments {
            let key = SessionKey(attached.handle);
            self.queue_attachment(who, attached);
            // A providing watcher learns its namespace's current demand.
            self.refresh_state_demand(key);
        }
        vosu_status::VOSU_OK
    }
}
