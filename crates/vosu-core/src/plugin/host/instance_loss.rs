//! Process-backend hooks on `PluginHost`: installing the launcher that
//! starts `runtime = "process"` instances, and retiring an instance whose
//! child or channel was reported lost (`execution::InstanceLoss`). Loss
//! releases the same host state shutdown does (`retire_instance`), plus
//! the instance's matcher registration and owned sessions, which detach
//! with reason `lost` (§6.2) so a later generation can attach fresh. Only
//! once the lost generation is retired, its endpoints closed and its
//! process reaped, does supervision decide whether a new generation starts
//! (`supervision.rs`).

use std::sync::Arc;

use super::super::error::HostError;
use super::super::execution::{LossCause, ProcessLauncher};
use super::super::instance::PluginInstance;
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
        tracing::warn!(?id, plugin = %plugin_id, reason = cause.reason(), "plugin-host process lost");
        let host = self.arc();
        std::thread::Builder::new()
            .name(format!("vosu-plugin-lost-{}", id.0))
            .spawn(move || host.retire_lost(id, plugin_id, instance, cause))
            .expect("spawning a lost plugin instance's retirement thread");
    }

    /// Core's own decision to lose a live process instance (an action-grace
    /// kill, §7.6 A5.4): the same one loss per generation as an OS exit or
    /// a channel failure, whichever is observed first. Does nothing for an
    /// in-process instance or one already lost.
    pub(super) fn lose_process_instance(&self, id: PluginInstanceId, cause: LossCause) {
        let marked = matches!(
            self.instances
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .get(&id),
            Some(InstanceSlot::Live(entry)) if entry.instance.mark_lost()
        );
        if marked {
            self.process_instance_lost(id, cause);
        }
    }

    fn retire_lost(
        &self,
        id: PluginInstanceId,
        plugin_id: String,
        instance: PluginInstance,
        cause: LossCause,
    ) {
        // Unregistered first so discovery stops offering processes to the
        // lost instance, and a replacement generation can register again.
        self.release_owned(&plugin_id);
        // Abandoning never runs `destroy`, so there is no result to report.
        let _ = self.retire_instance(
            id,
            Some(instance),
            Teardown::Abandon(SHUTDOWN_RACE_DESTROY_GRACE),
        );
        self.supervised_lost(id, cause);
    }
}
