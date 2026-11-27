//! `PluginHost`'s supervision of `runtime = "process"` instances
//! (`docs/v2/plugin-abi.md` §7.6 "Loss identity", "Restart", "Destroy and
//! shutdown"; A5.3): the host side of `process::supervisor`.
//!
//! - Every process instance [`PluginHost::create_instance`] creates is
//!   supervised: the initial load starts its window, and a failure leaves
//!   it `Failed` until a manual restart.
//! - A loss is recorded (`last_failure`, the crash event, `backoff` or
//!   `failed`) as soon as `instance_loss.rs` takes the generation out of
//!   service, before its retirement ends any waiter; the restart delay is
//!   armed through [`PluginHost::follow`] only after that retirement closed
//!   its endpoints and reaped its process.
//! - A restart is a new generation under the same instance id: a new child,
//!   new handles, new sessions offered by discovery once the matcher
//!   registers again, identification before public exposure.
//! - Stop, manual restart and shutdown make every pending delay and start
//!   stale through the supervisor's epoch; nothing respawns after them.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::dispatch::WorkerPool;
use super::super::error::HostError;
use super::super::execution::Execution;
use super::super::process::control::{StartupFailure, StartupStage};
use super::super::process::supervisor::{
    FailureReason, Next, ProcessStatus, Published, RestartRefused, RestartTimer, StartGuard,
    Template,
};
use super::super::registry::LoadedPlugin;
use super::super::session::SessionHandle;
use super::lifecycle::Teardown;
use super::{InstanceSlot, PluginHost, SHUTDOWN_RACE_DESTROY_GRACE};
use crate::identity::PluginInstanceId;

/// How long shutdown waits for starts already in progress: a first-load
/// startup deadline plus a destroy grace, with margin.
const SHUTDOWN_START_BOUND: Duration = Duration::from_secs(45);

/// How long a manual restart waits for a lost generation's retirement to
/// release the instance id. Retirement is bounded by the destroy grace.
const SLOT_RELEASE_BOUND: Duration = Duration::from_secs(10);

/// What a failed start reports to the supervisor. A loss between `READY`
/// and publication is a crash of that start.
fn failure_reason(error: &HostError) -> FailureReason {
    match error {
        HostError::ProcessStartupFailed(failure) => FailureReason::Startup(failure.clone()),
        HostError::InstanceLost => FailureReason::Startup(StartupFailure::new(
            StartupStage::Crash,
            "lost_before_publication",
            "the plugin host was lost after READY, before it was published",
        )),
        other => FailureReason::Refused(other.to_string()),
    }
}

impl PluginHost {
    /// The initial load of a process package under supervision.
    pub(super) fn start_supervised(
        &self,
        id: PluginInstanceId,
        plugin: &LoadedPlugin,
        data_dir: PathBuf,
        pool: Option<Arc<WorkerPool>>,
    ) -> Result<(), HostError> {
        let template = Template {
            plugin: plugin.clone(),
            data_dir,
            pool,
        };
        let guard =
            self.supervisor
                .register(id, template.clone())
                .map_err(|refused| match refused {
                    RestartRefused::ShuttingDown => HostError::ShuttingDown,
                    RestartRefused::Busy
                    | RestartRefused::Unknown
                    | RestartRefused::GenerationChanged => {
                        HostError::State(format!("process instance {} is already running", id.0))
                    }
                })?;
        self.run_start(id, &template, guard, false)
    }

    /// Starts one generation from `template` and reports the outcome.
    fn run_start(
        &self,
        id: PluginInstanceId,
        template: &Template,
        guard: StartGuard,
        automatic: bool,
    ) -> Result<(), HostError> {
        let result = self.arc().create_generation(
            id,
            &template.plugin,
            template.data_dir.clone(),
            template.pool.clone(),
        );
        match &result {
            Ok(()) => {
                if self.supervisor.published(id, guard.epoch) == Published::TearDown {
                    // A stop or shutdown overtook this start: the new
                    // generation goes at once.
                    let _ = self.stop_live(id);
                }
            }
            Err(error) => {
                let reason = failure_reason(error);
                tracing::warn!(?id, %reason, automatic, "process plugin did not start");
                let next = self.supervisor.start_failed(
                    id,
                    guard.epoch,
                    reason,
                    automatic,
                    Instant::now(),
                );
                self.follow(id, next);
            }
        }
        drop(guard);
        result
    }

    /// Acts on a supervisor transition: arms the restart delay, or logs why
    /// nothing restarts.
    pub(super) fn follow(&self, id: PluginInstanceId, next: Next) {
        match next {
            Next::Restart { epoch, delay } => {
                tracing::info!(?id, ?delay, "restarting process plugin after backoff");
                self.arm_restart(id, epoch, delay);
            }
            Next::Nothing => {
                if let Some(status) = self.supervisor.status(id) {
                    tracing::warn!(?id, ?status, "process plugin not restarted");
                }
            }
        }
    }

    /// Arms the one cancellable delay; when it elapses, a dedicated thread
    /// starts the next generation if `epoch` is still current.
    fn arm_restart(&self, id: PluginInstanceId, epoch: u64, delay: Duration) {
        let weak = self.self_ref.clone();
        let task = self.state_runtime.spawn(async move {
            tokio::time::sleep(delay).await;
            let Some(host) = weak.upgrade() else {
                return;
            };
            // A start can block for its whole startup deadline: never on
            // a runtime worker.
            let spawned = std::thread::Builder::new()
                .name(format!("vosu-plugin-restart-{}", id.0))
                .spawn(move || host.restart_after_backoff(id, epoch));
            if let Err(error) = spawned {
                tracing::error!(?id, %error, "could not start a plugin restart thread");
            }
        });
        self.supervisor.arm(id, epoch, RestartTimer::new(task));
    }

    fn restart_after_backoff(&self, id: PluginInstanceId, epoch: u64) {
        let Some((template, guard)) = self.supervisor.begin_restart(id, epoch) else {
            return;
        };
        let _ = self.run_start(id, &template, guard, true);
    }

    /// The per-instance teardown seam (§7.6 "Destroy and shutdown"): the
    /// manual stop, approval revocation, and M6b uninstall's
    /// `RootRemoval::stop`. Works for both runtimes and returns once the
    /// teardown completed:
    ///
    /// 1. a process instance's restart delay is cancelled and it becomes
    ///    `Stopped`, so nothing restarts it (a start in progress tears down
    ///    what it starts, and this call waits for that);
    /// 2. the instance stops receiving dispatch, its matcher is
    ///    unregistered and every session it owns detaches with reason
    ///    `lost`;
    /// 3. its resources, streams, actions, jobs, relays, subscriptions and
    ///    timers are released (`retire_instance`);
    /// 4. its queued dispatch drains and `destroy` runs within the 5-second
    ///    grace. A process child that does not exit by then is killed and
    ///    reaped.
    ///
    /// Returns `Err(HostError::DestroyTimedOut)` when the grace ran out. For
    /// a process instance the child is already dead then. An in-process
    /// plugin whose `destroy` or queued dispatch never returns cannot be
    /// stopped: its thread keeps running, and its context and library stay
    /// mapped (leaked, never freed under it) until the daemon exits. Its
    /// host state is released all the same. `UnknownInstance` when nothing
    /// is registered or supervised under `id`. Never call it from the
    /// plugin's own dispatch thread, whose drain it waits for.
    pub fn teardown_instance(&self, id: PluginInstanceId) -> Result<(), HostError> {
        self.teardown(id, None)
    }

    /// The management stop (§7.6): [`Self::teardown_instance`] of process
    /// instance `id` only while `generation` is still its current one.
    /// The check and the `Stopped` transition share the supervisor's lock,
    /// so an automatic restart in between makes it refuse with
    /// `GenerationChanged` before anything is torn down.
    pub fn stop_process_generation(
        &self,
        id: PluginInstanceId,
        generation: u64,
    ) -> Result<(), HostError> {
        self.teardown(id, Some(generation))
    }

    fn teardown(&self, id: PluginInstanceId, expected: Option<u64>) -> Result<(), HostError> {
        let supervised = self
            .supervisor
            .stop(id, expected)
            .map_err(|refused| restart_refused(id, refused))?;
        let result = self.stop_live(id);
        if supervised && !self.supervisor.wait_starts(id, SHUTDOWN_START_BOUND) {
            return Err(HostError::State(format!(
                "instance {} did not finish starting",
                id.0
            )));
        }
        if let Some(result) = result {
            return result;
        }
        let registered = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains_key(&id);
        if !registered {
            return if supervised {
                Ok(())
            } else {
                Err(HostError::UnknownInstance)
            };
        }
        // Still being created, or a lost generation still retiring: both
        // finish on their own, and a start that sees the stop tears its
        // generation down.
        if self.wait_slot_free(id, SHUTDOWN_START_BOUND) {
            Ok(())
        } else {
            Err(HostError::State(format!(
                "instance {} did not finish starting or retiring",
                id.0
            )))
        }
    }

    /// Manually restarts process instance `id` (§7.6 "Manual restart"):
    /// resets the crash window and the `failed` latch, cancels any delay,
    /// tears a running generation down, and starts a new generation.
    pub fn restart_process_instance(&self, id: PluginInstanceId) -> Result<(), HostError> {
        self.restart(id, None)
    }

    /// The management restart (§7.6): [`Self::restart_process_instance`]
    /// only while `generation` is still the current one, checked under the
    /// same supervisor lock that begins the new generation.
    pub fn restart_process_generation(
        &self,
        id: PluginInstanceId,
        generation: u64,
    ) -> Result<(), HostError> {
        self.restart(id, Some(generation))
    }

    fn restart(&self, id: PluginInstanceId, expected: Option<u64>) -> Result<(), HostError> {
        let (template, guard) = self
            .supervisor
            .manual_restart(id, expected)
            .map_err(|refused| restart_refused(id, refused))?;
        if let Some(Err(error)) = self.stop_live(id) {
            tracing::warn!(?id, %error, "the running generation needed a kill before the manual restart");
        }
        if !self.wait_slot_free(id, SLOT_RELEASE_BOUND) {
            let error =
                HostError::State(format!("process instance {} is still being retired", id.0));
            self.supervisor.start_failed(
                id,
                guard.epoch,
                FailureReason::Refused(error.to_string()),
                false,
                Instant::now(),
            );
            return Err(error);
        }
        self.run_start(id, &template, guard, false)
    }

    /// The supervised state of process instance `id`, for the management
    /// surface; `None` for an id that never ran a process package.
    pub fn process_status(&self, id: PluginInstanceId) -> Option<ProcessStatus> {
        self.supervisor.status(id)
    }

    /// Drops an uninstalled process package's restart template after teardown.
    pub(crate) fn forget_stopped_process(&self, id: PluginInstanceId) {
        self.supervisor.forget_stopped(id);
    }

    pub(crate) fn has_development_process(&self, plugin: &str) -> bool {
        self.supervisor.has_development(plugin)
    }

    /// Shutdown's first step: no restart starts and no delay survives.
    pub(super) fn supervision_shutdown(&self) {
        self.supervisor.shutdown();
    }

    /// Shutdown's last step: waits for starts already in progress, which
    /// refuse or tear down what they start once shutdown began.
    pub(super) fn supervision_drained(&self) {
        if !self.supervisor.wait_idle(SHUTDOWN_START_BOUND) {
            tracing::warn!("a process plugin start did not finish before shutdown completed");
        }
    }

    /// Takes a published instance `id` out of service and runs the
    /// controlled teardown. Its matcher and owned sessions go first, while
    /// it is still published, so its final committed state and
    /// `SESSION_DETACHED` (reason `lost`) still reach it before `destroy`.
    /// Then the instance-removal path runs `destroy` (process: `DESTROY`,
    /// then kill) within the 5-second grace. `None` when `id` is not
    /// published.
    fn stop_live(&self, id: PluginInstanceId) -> Option<Result<(), HostError>> {
        let plugin_id = match self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&id)
        {
            Some(InstanceSlot::Live(entry)) => entry.plugin_id.clone(),
            _ => return None,
        };
        self.release_owned(&plugin_id);
        let instance = {
            let mut instances = self
                .instances
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if !matches!(instances.get(&id), Some(InstanceSlot::Live(_))) {
                // Lost meanwhile: its own retirement finishes the job.
                return None;
            }
            let Some(InstanceSlot::Live(entry)) = instances.remove(&id) else {
                unreachable!("matched Live just above under the same lock");
            };
            instances.insert(id, InstanceSlot::Destroying { plugin_id });
            self.bump_topology_generation();
            entry.instance
        };
        let result = self.retire_instance(
            id,
            Some(instance),
            Teardown::Destroy(SHUTDOWN_RACE_DESTROY_GRACE),
        );
        if let Err(error) = &result {
            tracing::warn!(?id, %error, "plugin did not destroy within its grace period");
        }
        Some(result)
    }

    /// Unregisters `plugin_id`'s matcher, so discovery stops offering it
    /// processes and a next generation can register again, and detaches
    /// every session it owns with reason `lost`.
    pub(super) fn release_owned(&self, plugin_id: &str) {
        self.matcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .unregister(plugin_id);
        let detached = self
            .sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .detach_owner(plugin_id);
        for event in detached {
            self.finish_detach(
                SessionHandle(event.handle),
                Some(plugin_id.to_owned()),
                event,
            );
        }
    }

    /// Waits up to `bound` until no slot holds `id`.
    fn wait_slot_free(&self, id: PluginInstanceId, bound: Duration) -> bool {
        let instances = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (instances, timeout) = self
            .instances_changed
            .wait_timeout_while(instances, bound, |map| map.contains_key(&id))
            .unwrap_or_else(|poison| poison.into_inner());
        drop(instances);
        !timeout.timed_out()
    }
}

/// Whether `plugin` runs as a supervised process instance.
pub(super) fn is_process(plugin: &LoadedPlugin) -> bool {
    matches!(plugin.execution(), Execution::Process(_))
}

fn restart_refused(id: PluginInstanceId, refused: RestartRefused) -> HostError {
    match refused {
        RestartRefused::Unknown => HostError::UnknownInstance,
        RestartRefused::ShuttingDown => HostError::ShuttingDown,
        RestartRefused::GenerationChanged => HostError::GenerationChanged,
        RestartRefused::Busy => {
            HostError::State(format!("process instance {} is already starting", id.0))
        }
    }
}
