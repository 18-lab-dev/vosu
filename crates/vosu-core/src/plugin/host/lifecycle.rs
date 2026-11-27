//! `PluginHost` lifecycle/shutdown/discovery-exit plumbing split out of
//! `host.rs` to keep that file under the 1,000-line maintained-file limit
//! (`docs/architecture/source-map.md`): the dispatch-queue-full rate
//! limiter, `destroy_all`'s own `Creating`-placeholder wait, and
//! `process_exited`/`finish_detach`'s shared non-`SESSION_DECLINE` detach
//! path.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use vosu_plugin_api::DetachReason;

use super::super::error::HostError;
use super::super::matcher::Prefilter;
use super::super::session::SessionHandle;
use super::{InstanceSlot, PluginHost};
use crate::identity::PluginInstanceId;

/// How [`PluginHost::retire_instance`] ends the instance itself.
pub(super) enum Teardown {
    /// Controlled `destroy` within the grace period (shutdown).
    Destroy(Duration),
    /// The process was lost: terminate it and drain dispatch, no `destroy`.
    Abandon(Duration),
}

impl PluginHost {
    /// Rate-limited window `Self::log_dispatch_dropped` enforces
    /// (this struct's own `last_dispatch_queue_full_log` doc comment).
    const DISPATCH_QUEUE_FULL_LOG_WINDOW: Duration = Duration::from_secs(1);

    /// Logs a delivery this host silently dropped because the target
    /// instance's dispatch queue was already full (P3: every such drop
    /// used to be a bare `let _ = dispatch(...)`), at most once per
    /// `DISPATCH_QUEUE_FULL_LOG_WINDOW` across the whole host so a stuck
    /// plugin cannot flood the log. A no-op for any error other than
    /// `HostError::DispatchQueueFull` — every other `dispatch` failure
    /// (an unknown/torn-down instance) is either already logged by its
    /// own caller or genuinely expected (module doc comment).
    pub(super) fn log_dispatch_dropped(
        &self,
        instance: PluginInstanceId,
        opcode: vosu_plugin_api::vosu_opcode,
        error: &HostError,
    ) {
        if !matches!(error, HostError::DispatchQueueFull) {
            return;
        }
        let now = Instant::now();
        let mut last = self
            .last_dispatch_queue_full_log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let should_log = last.is_none_or(|previous| {
            now.duration_since(previous) >= Self::DISPATCH_QUEUE_FULL_LOG_WINDOW
        });
        if should_log {
            *last = Some(now);
            tracing::warn!(
                ?instance,
                ?opcode,
                "dropped a delivery: this instance's dispatch queue is full \
                 (rate-limited to one such log line per second)"
            );
        }
    }
    /// Backstop for [`Self::destroy_all`]'s own wait on a `Creating`
    /// placeholder resolving (P3): real responsiveness comes from
    /// [`Self::instances_changed`]'s own notify, not this bound — a
    /// plugin's `create` is already documented (§4) as never performing
    /// unbounded I/O, so this only guards against a missed notification,
    /// matching `SHUTDOWN_RACE_DESTROY_GRACE`'s own 5-second precedent.
    const CREATING_WAIT_BACKSTOP: Duration = Duration::from_secs(5);

    /// Destroys every registered instance (daemon shutdown). Takes
    /// exactly one instance out of active duty at a time — never all of
    /// them upfront — so an instance still waiting its own turn in this
    /// loop keeps receiving timer and settings deliveries, and keeps its
    /// plugin id on its logs, for as long as it is genuinely still alive
    /// (P3: taking every entry at once left every instance but the first
    /// undeliverable the moment this method started, regardless of how
    /// far down the loop its own turn was). Cancels the instance's timers
    /// both before its own `destroy` call (so none fire into a dispatch
    /// queue about to stop accepting work) and again after `destroy`
    /// returns (§6.11's exit item: `destroy`'s own queue drain can run
    /// `TIMER_SET` again, which must not outlive the instance either),
    /// one instance's hang never extending another's grace period.
    ///
    /// Sets `shutting_down` first, then — unlike a `Live`/`Destroying`
    /// slot — never removes a `Creating` one itself: that instance's own
    /// `create_instance` call is still running the plugin's `create`, and
    /// once it returns, it will observe `shutting_down` and destroy the
    /// instance itself rather than publish it `Live` (`create_instance`'s
    /// own doc comment). Removing the placeholder here instead, the way
    /// an earlier version of this method did, let that call's final swap
    /// still install a `Live` entry *after* this loop had already decided
    /// the map was empty and returned — leaking a running instance this
    /// function was supposed to have torn down. Waiting for
    /// `create_instance` to resolve its own placeholder closes that race.
    ///
    /// Process supervision stops first, so no restart delay survives and
    /// no new generation starts; a start already in progress refuses or
    /// tears down what it starts, and this call waits for it last.
    pub fn destroy_all(&self, grace: Duration) {
        self.supervision_shutdown();
        self.shutting_down.store(true, Ordering::Release);
        self.api_shutdown();
        self.destroy_registered(grace);
        self.supervision_drained();
    }

    fn destroy_registered(&self, grace: Duration) {
        loop {
            let next = {
                let mut guard = self
                    .instances
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                loop {
                    if guard.is_empty() {
                        return;
                    }
                    let ready = guard
                        .iter()
                        .find(|(_, slot)| !matches!(slot, InstanceSlot::Creating { .. }))
                        .map(|(id, _)| *id);
                    let Some(id) = ready else {
                        // Only `Creating` placeholders remain: wait for
                        // their own `create_instance` calls to resolve
                        // them (see this method's doc comment), woken by
                        // `instances_changed`'s own notify (P3) rather
                        // than polling on a fixed sleep; `wait_timeout`
                        // is only a bounded backstop
                        // (`CREATING_WAIT_BACKSTOP`) in case a
                        // notification is ever missed.
                        let (woken, _timed_out) = self
                            .instances_changed
                            .wait_timeout(guard, Self::CREATING_WAIT_BACKSTOP)
                            .unwrap_or_else(|poison| poison.into_inner());
                        guard = woken;
                        continue;
                    };
                    let slot = guard.remove(&id).expect("just read this key from the map");
                    break match slot {
                        InstanceSlot::Live(entry) => {
                            guard.insert(
                                id,
                                InstanceSlot::Destroying {
                                    plugin_id: entry.plugin_id,
                                },
                            );
                            self.bump_topology_generation();
                            (id, Some(entry.instance))
                        }
                        InstanceSlot::Destroying { .. } => (id, None),
                        InstanceSlot::Creating { .. } => unreachable!("excluded by `find` above"),
                    };
                }
            };
            let (id, instance) = next;
            if let Err(error) = self.retire_instance(id, instance, Teardown::Destroy(grace)) {
                tracing::warn!(?id, %error, "plugin did not destroy within its grace period");
            }
        }
    }

    /// Removes one instance already taken out of active duty (its slot is
    /// `Destroying`) from every host service — resources, actions,
    /// subscriptions, surface readers, bus sessions, its own namespace in
    /// sessions it does not own, timers — then tears the instance itself
    /// down and drops its slot. Shared by shutdown,
    /// per-instance teardown and process loss so all release exactly the
    /// same state. Pending resource and stream operations end before
    /// running jobs and relay calls fail (§7.6 "Loss identity" order).
    /// Timers are cancelled both before and after the teardown: a local
    /// `destroy`'s queue drain can run `TIMER_SET` again (§6.11's exit
    /// item). State subscriptions end before the teardown, but the
    /// instance's delivery slots are dropped only after it: the drain
    /// still delivers the `SESSION_DETACHED` (and final state) its owned
    /// sessions' detach queued before `destroy` (§7.6 "Per-instance
    /// teardown seam" step 2). Returns the controlled `destroy`'s result.
    pub(super) fn retire_instance(
        &self,
        id: PluginInstanceId,
        instance: Option<super::super::instance::PluginInstance>,
        teardown: Teardown,
    ) -> Result<(), HostError> {
        self.resource_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
        self.action_instance_lost(id);
        self.state_subscriber_removed(id);
        if let Some(plugin_id) = self.plugin_id_of(id) {
            self.surface_reader_removed(&plugin_id);
            self.close_bus_sessions_for(&plugin_id);
            self.clear_guest_publications(&plugin_id);
        }
        self.timers.cancel_all(id);
        let mut result = Ok(());
        if let Some(instance) = instance {
            match teardown {
                Teardown::Destroy(grace) => result = instance.destroy(grace),
                Teardown::Abandon(grace) => instance.abandon(grace),
            }
            self.timers.cancel_all(id);
        }
        self.state_slots_retired(id);
        self.instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&id);
        self.instances_changed.notify_all();
        result
    }
    /// Discovery's own entry point for a process exit (phase (d2)):
    /// detaches whatever session, if any, this registry still tracks for
    /// `pid` (`DetachReason::Exited`) and clears the D6 decline record so
    /// a later reuse of `pid` can attach fresh.
    ///
    /// P3: `handle_for_pid`, the owner lookup, `detach` itself, and
    /// `forget_exited_pid` all run under one `sessions` lock acquisition
    /// instead of several separate ones — a concurrent `SESSION_DECLINE`
    /// re-offer (same generation, a new handle and owner) landing in a
    /// gap between separate locks could otherwise reassign `pid`'s
    /// tracked handle out from under this call: the detach this call
    /// issued for the handle it first saw would then silently no-op
    /// (that handle no longer exists), leaving the re-offered handle
    /// attached to a pid that, in reality, has already exited.
    pub fn process_exited(&self, pid: u32) {
        let resolved = {
            let mut sessions = self
                .sessions
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let found = sessions.handle_for_pid(pid).and_then(|handle| {
                let owner = sessions
                    .owner_and_pid(handle)
                    .map(|(owner, _)| owner.clone());
                sessions
                    .detach(handle, DetachReason::Exited)
                    .map(|detached| (handle, owner, detached))
            });
            sessions.forget_exited_pid(pid);
            found
        };
        if let Some((handle, owner, detached)) = resolved {
            self.finish_detach(handle, owner, detached);
        }
        self.notify_exited(pid);
    }

    /// Discovery's foreground-window entry point (P2-9, phase (d2)): the
    /// OS foreground window changed owner (or lost one), so the Focus
    /// rule (`docs/v2/api-v1.md` "Focus rule") must be re-run — nothing
    /// else calls `Self::recompute_focus` on a foreground change by
    /// itself; `foreground_pid()` is otherwise only consulted lazily, the
    /// next time some other event (identify, detach) happens to
    /// recompute. A later wiring phase feeds this from a real OS
    /// foreground-change source; this phase only provides and tests it,
    /// the same split `process_appeared`/`process_exited` already use for
    /// the rest of discovery.
    pub fn foreground_changed(&self) {
        self.recompute_focus();
        self.surface_roster_changed();
    }

    /// Finishes a detach once `sessions` has already removed `handle` and
    /// produced `detached` — shared by every non-`SESSION_DECLINE` detach
    /// path ([`Self::process_exited`], and a lost process instance's owned
    /// sessions in `instance_loss.rs`; "superseded" is later wiring).
    ///
    /// P3: cancels `owner`'s outstanding read plans and scans for
    /// `handle` *before* dispatching `SESSION_DETACHED` — on serial
    /// dispatch, this guarantees any result or scan answer already
    /// queued for this handle at the moment of cancellation is delivered
    /// before `SESSION_DETACHED`, never after it (mirroring the same
    /// ordering `SessionDecline`'s own handler already uses).
    pub(super) fn finish_detach(
        &self,
        handle: SessionHandle,
        owner: Option<super::super::matcher::PluginId>,
        detached: vosu_plugin_api::SessionDetached,
    ) {
        self.surface_session_detaching(crate::state::SessionKey(handle.0));
        self.resource_session_detached(handle);
        self.action_session_detached(handle);
        if let Some(owner) = &owner {
            self.cancel_read_plans_for_session(owner, handle);
            self.cancel_scans_for_session(owner, handle);
        }
        self.state_session_detaching(&detached, owner.as_deref());
        self.forget_read_plan_session(handle);
        self.close_bus_session(handle);
        self.recompute_focus();
    }

    /// Whether any enabled plugin currently has a registered `[match]`
    /// table (`docs/v2/plugin-abi.md` §6.1): the discovery service's own
    /// "zero cost when unused" gate (`docs/v2/implementation-plan.md` M1
    /// §4, phase d2b) — production `BUNDLED` holds no matching plugin
    /// until `vosu-osu` lands, so this is `false` there and the caller
    /// never starts the discovery service at all.
    #[must_use]
    pub fn has_enabled_matchers(&self) -> bool {
        !self
            .matcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .is_empty()
    }

    /// This host's own matcher-set generation (P2,
    /// `MatcherEngine::generation`'s own doc comment): re-read on every
    /// reconciliation pass so a plugin installed or torn down after
    /// startup is picked up, including re-offering a process discovery
    /// already tracked under the old set.
    #[must_use]
    pub fn matcher_generation(&self) -> u64 {
        self.matcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .generation()
    }

    /// The unified enumeration-cost pre-filter (§6.1,
    /// `MatcherEngine::prefilter`'s own doc comment), unioned with any
    /// discovery observer's extra interest (`observer.rs`): re-read on every
    /// reconciliation pass so a plugin installed or torn down after
    /// startup narrows or widens what a source opens or admits.
    #[must_use]
    pub fn prefilter(&self) -> Prefilter {
        let installed = self
            .matcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .prefilter();
        self.observer_prefilter(installed)
    }
}
