//! Process-backend hooks on `PluginHost`: installing the launcher that
//! starts `runtime = "process"` instances, and retiring an instance whose
//! child or channel was reported lost (`execution::InstanceLoss`). Loss
//! releases the same host state shutdown does (`retire_instance`), plus
//! the instance's matcher registration and owned sessions, which detach
//! with reason `lost` (§6.2) so a later instance of the plugin can attach
//! fresh.

use std::sync::Arc;

use super::super::error::HostError;
use super::super::execution::{LossCause, ProcessLauncher};
use super::super::instance::PluginInstance;
use super::super::session::SessionHandle;
use super::lifecycle::Teardown;
use super::{InstanceSlot, PluginHost, SHUTDOWN_RACE_DESTROY_GRACE};
use crate::identity::PluginInstanceId;

impl PluginHost {
    /// Installs the launcher for `runtime = "process"` packages. Composition
    /// installs it once, before creating any instance; until then (and for
    /// a second attempt) process packages are refused.
    pub fn install_process_launcher(
        &self,
        launcher: Arc<dyn ProcessLauncher>,
    ) -> Result<(), HostError> {
        self.process_launcher
            .set(launcher)
            .map_err(|_| HostError::State("a process launcher is already installed".into()))
    }

    /// First loss report for a live process instance: takes it out of
    /// active duty under the `instances` lock, then retires it on a
    /// dedicated thread. The report may arrive on a thread the endpoint
    /// owns (its exit watcher or channel reader), which terminating the
    /// endpoint can join, so retirement never runs on the reporting
    /// thread. An instance still `Creating` is left to `create_instance`,
    /// which checks the loss flag before publishing.
    pub(super) fn process_instance_lost(&self, id: PluginInstanceId, cause: LossCause) {
        let taken = {
            let mut guard = self
                .instances
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            match guard.get(&id) {
                Some(InstanceSlot::Live(entry)) if entry.instance.is_lost() => {}
                _ => return,
            }
            let Some(InstanceSlot::Live(entry)) = guard.remove(&id) else {
                unreachable!("matched Live just above under the same lock");
            };
            guard.insert(
                id,
                InstanceSlot::Destroying {
                    plugin_id: entry.plugin_id.clone(),
                },
            );
            (entry.plugin_id, entry.instance)
        };
        let (plugin_id, instance) = taken;
        tracing::warn!(?id, plugin = %plugin_id, ?cause, "plugin-host process lost");
        let host = self.arc();
        std::thread::Builder::new()
            .name(format!("vosu-plugin-lost-{}", id.0))
            .spawn(move || host.retire_lost(id, plugin_id, instance))
            .expect("spawning a lost plugin instance's retirement thread");
    }

    fn retire_lost(&self, id: PluginInstanceId, plugin_id: String, instance: PluginInstance) {
        // Unregistered first so discovery stops offering processes to the
        // lost instance, and a replacement instance can register again.
        self.matcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .unregister(&plugin_id);
        let detached = self
            .sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .detach_owner(&plugin_id);
        for event in detached {
            self.finish_detach(SessionHandle(event.handle), Some(plugin_id.clone()), event);
        }
        self.retire_instance(
            id,
            Some(instance),
            Teardown::Abandon(SHUTDOWN_RACE_DESTROY_GRACE),
        );
    }
}
