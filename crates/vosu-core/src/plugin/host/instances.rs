//! `PluginHost` instance-registry plumbing split out of `host.rs` to keep
//! that file under the 1,000-line maintained-file limit
//! (`docs/architecture/source-map.md`): registering and tearing down one
//! plugin instance (`create_instance`), replaying deliveries queued while
//! it was still [`InstanceSlot::Creating`], bridge B2's config-watcher
//! hookup, and the by-`PluginInstanceId`/by-plugin-id dispatch routing
//! every other host-side handler (`lifecycle.rs`, `read_plans.rs`,
//! `scans.rs`, `host.rs`'s own `PluginHostHandler::handle`) calls through.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::runtime::Handle;
use vosu_plugin_api::{Init, Message, encode, vosu_status};

use super::super::dispatch::{OwnedMessage, WorkerPool};
use super::super::error::HostError;
use super::super::execution::PackageOrigin;
use super::super::host_services::PluginHostHandler;
use super::super::instance::PluginInstance;
use super::super::read_plan::OutstandingPlans;
use super::super::registry::LoadedPlugin;
use super::super::scan::OutstandingScans;
use super::super::settings::ConfigSettingsBridgeB2;
use super::{
    InstanceEntry, InstanceSlot, PendingDelivery, PluginHost, SHUTDOWN_RACE_DESTROY_GRACE,
};
use crate::identity::PluginInstanceId;

impl PluginHost {
    /// Creates and registers one plugin instance: resolves `plugin_id`'s
    /// settings schema (currently only bridge B2's own), builds
    /// `init.settings` through `PluginSettingsService` (§6.9), then
    /// `PluginInstance::create`s it with this host as its
    /// `PluginHostHandler`.
    ///
    /// Registers `id` under `InstanceSlot::Creating` *before* calling
    /// into the plugin's own `create` (module doc comment): a timer fired
    /// or a settings change delivered while `create` is still running is
    /// queued there instead of lost, and is replayed, in order, the
    /// moment this call installs the real [`PluginInstance`] in its
    /// place. A `create` that fails at any step — before `PluginInstance`
    /// even exists, or `PluginInstance::create` itself returning `Err` —
    /// removes the placeholder and cancels every timer the plugin may
    /// have set from inside its own failed `create` (§6.11's exit item:
    /// a timer must not outlive the instance that set it, including one
    /// that never finished coming up).
    ///
    /// F3: `plugin.manifest().matcher`'s `[match]` table is registered
    /// only once this call is about to publish `id` `Live`, under the
    /// same `instances` lock as that publish — never while `id` is only
    /// `Creating`. Before this fix, registering it up front left a
    /// narrow window where `process_appeared` (phase (d2)) could attach
    /// a session to a plugin id whose own `create` had not yet (and
    /// might never) finish, stranding that session: the registry
    /// de-duplicates by pid, so a process matched during that window was
    /// never offered again while it lived, and a later-installed,
    /// shadowed plugin never got the chance either. Registering only at
    /// publish time removes the window instead of papering over it with
    /// a detach-and-re-offer step on every failure path.
    ///
    /// Races [`Self::destroy_all`] safely: `shutting_down` is checked
    /// twice. The early check (before the plugin's own `create` even
    /// runs, and before this call takes `instances` at all) only ever
    /// rejects a brand new instance outright once shutdown has already
    /// been observably set — it cannot by itself close the race this
    /// method's doc comment describes, since `destroy_all` may set
    /// `shutting_down` and start draining `instances` at any point after
    /// it. The second check — after `create` returns, atomically with
    /// the swap that would otherwise publish the instance `Live`, both
    /// under the same `instances` lock `destroy_all` itself takes — is
    /// what actually closes it: `destroy_all` only ever skips a
    /// `Creating` placeholder (never removes it, so it never stops
    /// waiting for this id), so if this second check loses the race,
    /// `destroy_all`'s own loop is guaranteed to still find and tear
    /// down the `Live` entry; if it wins the race, this call destroys
    /// the instance itself instead of publishing it, and removes the
    /// placeholder so `destroy_all`'s wait resolves.
    ///
    /// Known behaviour, not addressed here (phase (d2) decides): the
    /// matcher's own install order — which this call's `register` above
    /// extends — is the order in which concurrent `create_instance` calls
    /// *finish*, not the order they were called in, since each one only
    /// registers once its own plugin's `create` has already returned. A
    /// slower-finishing call started earlier is installed after a
    /// faster-finishing one started later. That matters for D18's own
    /// runtime tie-break and the Focus rule's "earliest-installed
    /// plugin" first-ever tie-break (`FocusCandidate::install_order`'s
    /// own doc comment) if instances are ever created concurrently; today
    /// there is no production caller that does.
    ///
    /// A `runtime = "process"` package is supervised (`supervision.rs`,
    /// §7.6 "Restart"): this call is its initial load, a failure leaves it
    /// `failed` until a manual restart, and a later loss restarts it with
    /// backoff under the same `id` as a new generation.
    pub fn create_instance(
        self: &Arc<Self>,
        id: PluginInstanceId,
        plugin: &LoadedPlugin,
        data_dir: PathBuf,
        concurrent_pool: Option<Arc<WorkerPool>>,
    ) -> Result<(), HostError> {
        if super::supervision::is_process(plugin) {
            self.start_supervised(id, plugin, data_dir, concurrent_pool)
        } else {
            self.create_generation(id, plugin, data_dir, concurrent_pool)
        }
    }

    /// Creates and publishes one generation of instance `id`: everything
    /// [`Self::create_instance`] describes, without supervision.
    pub(super) fn create_generation(
        self: &Arc<Self>,
        id: PluginInstanceId,
        plugin: &LoadedPlugin,
        data_dir: PathBuf,
        concurrent_pool: Option<Arc<WorkerPool>>,
    ) -> Result<(), HostError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(HostError::ShuttingDown);
        }
        let plugin_id = plugin.manifest().id.clone();
        let publisher = crate::state::PublisherGrant::from_manifest(plugin.manifest())
            .map_err(|e| HostError::State(e.to_string()))?;
        let registration =
            super::state::SchemaRegistration::new(&self.state, &self.topology_generation, plugin)?;
        let matcher_table = plugin.manifest().matcher.clone();
        let schema = plugin.settings_schema().cloned();
        self.instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(
                id,
                InstanceSlot::Creating {
                    plugin_id: plugin_id.clone(),
                    settings_schema: schema.clone(),
                    pending: Vec::new(),
                    watch_sessions: false,
                },
            );
        self.resource_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .register(id, &plugin.manifest().root_slots);

        let created = (|| {
            let settings = self
                .settings
                .effective_settings(&plugin_id, schema.as_ref())
                .unwrap_or_else(|_| vosu_plugin_api::CborValue::Map(Vec::new()));
            let init = encode(&Init {
                plugin_id: plugin_id.clone(),
                settings,
                data_dir,
            })
            .map_err(|error| HostError::InitEncodeFailed(error.to_string()))?;
            PluginInstance::create_with_launcher(
                id,
                plugin,
                &init,
                self.clone() as Arc<dyn PluginHostHandler>,
                concurrent_pool,
                self.process_launcher
                    .get()
                    .map(|launcher| launcher.as_ref()),
            )
        })();

        let instance = match created {
            Ok(instance) => instance,
            Err(error) => {
                self.timers.cancel_all(id);
                self.resource_state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(id);
                self.instances
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(&id);
                self.instances_changed.notify_all();
                // F3: the matcher is registered only once this instance
                // is about to publish `Live` (below), never before
                // `create` runs, so a `create` failure here needs no
                // matcher unregister — nothing was ever registered for
                // it, and no process discovered during this window could
                // ever have been attached to this dead plugin id.
                return Err(error);
            }
        };
        // Publish and activate a startup watch as one state transition.
        let state_serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
        let mut guard = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if self.shutting_down.load(Ordering::Acquire) {
            // `destroy_all` started (and may already have finished
            // draining every non-`Creating` slot) while the plugin's own
            // `create` above was still running. Destroy the instance this
            // call just built instead of publishing it `Live`, and remove
            // the placeholder so `destroy_all`'s own wait for it resolves.
            self.discard_unpublished(guard, id);
            drop(state_serial);
            // F3: same reason as the `created` error path above — this
            // instance is being destroyed, never published `Live`, so
            // its matcher table (not yet registered) needs no unregister.
            if let Err(error) = instance.destroy(SHUTDOWN_RACE_DESTROY_GRACE) {
                tracing::warn!(
                    ?id, %error,
                    "instance created during shutdown did not destroy within its grace period"
                );
            }
            self.timers.cancel_all(id);
            return Err(HostError::ShuttingDown);
        }
        if instance.is_lost() {
            // The child was reported lost after its launch returned but
            // before this publication: `process_instance_lost` found only
            // the `Creating` placeholder and left the teardown to this
            // call. Nothing was published, so no matcher, action or
            // session cleanup is owed; the namespace registration drops
            // with `registration` on return.
            self.discard_unpublished(guard, id);
            drop(state_serial);
            instance.abandon(SHUTDOWN_RACE_DESTROY_GRACE);
            self.timers.cancel_all(id);
            return Err(HostError::InstanceLost);
        }
        if let Some(table) = matcher_table {
            // F3: registered only now, atomically with publishing `Live`
            // under this same `instances` lock — never while `id` was
            // only `Creating`, so `process_appeared` can never attach a
            // session to a plugin instance that might still fail to come
            // up. A refusal here (a duplicate id already registered)
            // tears this instance down exactly like the paths above:
            // it never got a chance to publish `Live` either.
            if let Err(error) = self
                .matcher
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .register(plugin_id.clone(), table)
            {
                self.discard_unpublished(guard, id);
                drop(state_serial);
                if let Err(destroy_error) = instance.destroy(SHUTDOWN_RACE_DESTROY_GRACE) {
                    tracing::warn!(
                        ?id, %destroy_error,
                        "instance whose matcher registration failed did not destroy within its grace period"
                    );
                }
                self.timers.cancel_all(id);
                return Err(HostError::MatcherRegister(error));
            }
        }
        self.register_actions(id, plugin);
        let previous = guard.insert(
            id,
            InstanceSlot::Live(InstanceEntry {
                instance,
                plugin_id,
                settings_schema: schema,
                replaces: plugin.manifest().replaces.clone(),
                capabilities: plugin
                    .manifest()
                    .provides
                    .iter()
                    .filter(|provided| !provided.id.starts_with("action:"))
                    .map(|provided| super::capabilities::ProvidedCapability {
                        id: provided.id.clone(),
                        version: if provided.id.starts_with("game:") {
                            // Game capabilities are unversioned presence markers.
                            semver::Version::new(0, 0, 0)
                        } else {
                            provided
                                .version
                                .parse()
                                .expect("validated capability version")
                        },
                    })
                    .collect(),
                outstanding_plans: Arc::new(OutstandingPlans::new()),
                outstanding_scans: OutstandingScans::new(),
                publisher: Arc::new(super::state::Publisher {
                    grant: publisher,
                    version: plugin.manifest().version.clone(),
                    read_grant: crate::state::InternalGrant::from_manifest(plugin.manifest()),
                    read_fields: plugin
                        .manifest()
                        .requires
                        .iter()
                        .chain(&plugin.manifest().optional)
                        .filter(|cap| cap.id.starts_with("field:"))
                        .map(|cap| cap.id.clone())
                        .collect(),
                    read_resources: plugin
                        .manifest()
                        .requires
                        .iter()
                        .chain(&plugin.manifest().optional)
                        .filter_map(|cap| cap.id.strip_prefix("resource:").map(str::to_owned))
                        .collect(),
                    bundled: plugin.origin() == PackageOrigin::Bundled,
                    official: plugin.origin().is_official(),
                    _registration: registration,
                }),
            }),
        );
        self.bump_topology_generation();
        let (pending, watch_sessions) = match previous {
            Some(InstanceSlot::Creating {
                pending,
                watch_sessions,
                ..
            }) => (pending, watch_sessions),
            _ => (Vec::new(), false),
        };
        drop(guard);
        if watch_sessions {
            self.subscribe_sessions_watch(id);
        }
        drop(state_serial);
        self.instances_changed.notify_all();
        self.replay_pending(id, pending);
        Ok(())
    }

    /// Replays every delivery `id` accumulated while it was still
    /// `Creating`, in the order it arrived, now that the real instance is
    /// `Live`.
    fn replay_pending(&self, id: PluginInstanceId, pending: Vec<PendingDelivery>) {
        if pending.is_empty() {
            return;
        }
        let guard = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(InstanceSlot::Live(entry)) = guard.get(&id) else {
            return;
        };
        for delivery in pending {
            let Ok(owned) = OwnedMessage::from_message(&delivery.message, 0) else {
                continue;
            };
            if let Err(error) = entry.instance.dispatch(owned) {
                self.log_dispatch_dropped(id, delivery.message.opcode(), &error);
            }
        }
    }

    /// Spawns bridge B2's background `ConfigStore` change watcher (§6.9:
    /// "every later change arrives as `SETTINGS_CHANGED`"), delivering
    /// through this host and resolving `vosu.osu`'s current
    /// `settings.schema.json` from this host's own `SettingsSchemaSource`
    /// impl (the live instance's `LoadedPlugin::settings_schema`, P0/P1) —
    /// no longer a schema bridge B2 embeds itself. A no-op for a `bridge`
    /// whose plugin id has no live instance yet or ever —
    /// `settings_changed` below simply finds nothing to deliver to.
    pub fn watch_bridge_b2(
        self: &Arc<Self>,
        bridge: &Arc<ConfigSettingsBridgeB2>,
        runtime: &Handle,
    ) -> tokio::task::JoinHandle<()> {
        bridge.watch_config_changes(
            runtime,
            Arc::clone(&self.settings),
            self.clone(),
            self.clone(),
        )
    }

    /// Dispatches `message` directly to `instance`'s own queue, exactly
    /// the per-instance delivery route `TimerDelivery`'s `timer_fired`
    /// uses internally — the hook point a later phase's session (§6.2)
    /// or focus delivery would call once it routes through `PluginHost`
    /// instead of `src/runtime/dev_plugins.rs`'s own instance list.
    /// Answers `UnknownInstance` for an id that is not currently
    /// `InstanceSlot::Live` (including one still `Creating` or already
    /// `Destroying`): a caller needing `Creating`-aware queuing is
    /// `timer_fired`/`settings_changed`'s own narrower logic, not this
    /// general hook. `correlation_id` is carried unchanged into the
    /// delivered message (`0` for every current timer/settings/read-plan
    /// caller; a scan's own request-chosen value for `scans.rs`, §6.4).
    pub fn dispatch_to_instance(
        &self,
        instance: PluginInstanceId,
        message: &Message,
        correlation_id: u64,
    ) -> Result<std::sync::mpsc::Receiver<vosu_status>, HostError> {
        let owned = OwnedMessage::from_message(message, correlation_id)
            .map_err(|error| HostError::InitEncodeFailed(error.to_string()))?;
        let guard = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match guard.get(&instance) {
            Some(InstanceSlot::Live(entry)) => entry.instance.dispatch(owned),
            _ => Err(HostError::UnknownInstance),
        }
    }

    /// Reserve terminal delivery capacity before accepting bounded work.
    pub fn reserve_outbox(
        &self,
        instance: PluginInstanceId,
    ) -> Result<crate::plugin::outbox::OutboxSlot, HostError> {
        let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        match instances.get(&instance) {
            Some(InstanceSlot::Live(entry)) => entry.instance.reserve_outbox(),
            _ => Err(HostError::UnknownInstance),
        }
    }

    /// Reserves `instance`'s delivery of one brokered work result (M5 B0):
    /// see [`PluginInstance::reserve_work_result`].
    pub(super) fn reserve_work_result(
        &self,
        instance: PluginInstanceId,
        class: crate::plugin::outbox::SlotClass,
    ) -> Result<crate::plugin::outbox::OutboxSlot, HostError> {
        let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        match instances.get(&instance) {
            Some(InstanceSlot::Live(entry)) => entry.instance.reserve_work_result(class),
            _ => Err(HostError::UnknownInstance),
        }
    }

    pub(super) fn dispatch_to(&self, plugin_id: &str, message: &Message) {
        let Ok(owned) = OwnedMessage::from_message(message, 0) else {
            return;
        };
        let mut guard = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for (id, slot) in guard.iter_mut() {
            match slot {
                InstanceSlot::Live(entry) if entry.plugin_id == plugin_id => {
                    if let Err(error) = entry.instance.dispatch(owned.clone()) {
                        self.log_dispatch_dropped(*id, message.opcode(), &error);
                    }
                }
                InstanceSlot::Creating {
                    plugin_id: creating_id,
                    pending,
                    ..
                } if creating_id == plugin_id => pending.push(PendingDelivery {
                    message: message.clone(),
                }),
                _ => {}
            }
        }
    }

    /// `plugin_id` this `instance` is registered under, for every message
    /// that must be attributed or routed by plugin id rather than by
    /// `PluginInstanceId` — the same lookup `handle`'s `LogRecord` arm
    /// already did inline.
    pub(super) fn plugin_id_of(&self, instance: PluginInstanceId) -> Option<String> {
        self.instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&instance)
            .map(|slot| slot.plugin_id().to_owned())
    }

    /// Drops a `Creating` placeholder that will never be published `Live`
    /// (shutdown raced it, its process was lost, or its matcher refused),
    /// releasing the `instances` lock before waking `destroy_all`, and the
    /// resource slots registered alongside it. The caller then tears the
    /// instance itself down outside every host lock.
    fn discard_unpublished(
        &self,
        mut guard: std::sync::MutexGuard<
            '_,
            std::collections::HashMap<PluginInstanceId, InstanceSlot>,
        >,
        id: PluginInstanceId,
    ) {
        guard.remove(&id);
        drop(guard);
        self.instances_changed.notify_all();
        self.timers.cancel_all(id);
        self.resource_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
    }
}
