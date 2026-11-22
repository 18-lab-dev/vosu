//! `PluginHost` (`docs/v2/implementation-plan.md` M1): owns every live
//! plugin instance and is the `PluginHostHandler` that routes the M1 host
//! services' plugin -> host opcodes — `VOSU_OP_LOG_RECORD` (§6.10),
//! `VOSU_OP_TIMER_SET`/`VOSU_OP_TIMER_CANCEL` (§6.11) — to
//! [`super::logging::PluginLogRouter`] and [`super::timers::TimerRegistry`],
//! delivering a fired timer and a settings change back through the owning
//! instance's own dispatch path (`super::dispatch::OwnedMessage`). Builds
//! `init.settings` through [`super::settings::PluginSettingsService`] at
//! `create` time (§6.9) and cancels every live timer on `destroy` (§6.11's
//! exit item).
//!
//! Session lifecycle (§6.2) and focus (`FocusTracker`) delivery, and real
//! process discovery feeding `MatcherEngine`, are later phases: this type
//! does not implement them. [`InstanceEntry::plugin_id`] is already the
//! hook a session-scoped log attribution would read, [`PluginHost::handle`]'s
//! fallback arm is where `SESSION_IDENTIFY`/`SESSION_DECLINE` routing
//! plugs in, and [`PluginHost::log_session`] is where an attached
//! session's public id would come from once session lifecycle routes
//! through here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::runtime::Handle;
use vosu_plugin_api::{Init, Message, TimerCancel, TimerFired, TimerSet, encode, vosu_status};

use super::dispatch::{OwnedMessage, WorkerPool};
use super::error::HostError;
use super::host_services::{PluginHostHandler, PluginInstanceId};
use super::instance::PluginInstance;
use super::logging::PluginLogRouter;
use super::registry::LoadedPlugin;
use super::settings::{
    BRIDGE_B2_PLUGIN_ID, ConfigSettingsBridgeB2, PluginSettingsService, SettingsChangeDelivery,
};
use super::timers::TimerDelivery;
use super::timers::TimerRegistry;
use crate::logging::Logger;

/// One registered instance's metadata alongside its [`PluginInstance`]:
/// enough to attribute its logs (§6.10) without re-deriving the plugin id
/// from the manifest on every `LOG_RECORD`.
struct InstanceEntry {
    instance: PluginInstance,
    plugin_id: String,
}

/// Owns every live plugin instance (replacing a bare `Vec<PluginInstance>`,
/// `src/runtime/dev_plugins.rs`'s former ad-hoc `LoggingHandler`, and its
/// stub `TIMER_SET`/empty `init.settings` answers) and answers the M1 host
/// services' opcodes for all of them.
pub struct PluginHost {
    self_ref: Weak<PluginHost>,
    log_router: PluginLogRouter,
    timers: TimerRegistry,
    settings: Arc<PluginSettingsService>,
    instances: Mutex<HashMap<PluginInstanceId, InstanceEntry>>,
}

impl PluginHost {
    /// `runtime` backs both the log router's background summary-flush
    /// task and the timer registry's spawned timer tasks (`Handle::spawn`
    /// works from any thread, unlike the ambient `tokio::spawn`, so a
    /// plugin instance's own dispatch thread — a plain OS thread, not a
    /// Tokio runtime, §4 — can still drive both through this one host).
    pub fn new(logger: Logger, settings: Arc<PluginSettingsService>, runtime: Handle) -> Arc<Self> {
        Arc::new_cyclic(|self_ref| Self {
            self_ref: self_ref.clone(),
            log_router: PluginLogRouter::new(logger, runtime.clone()),
            timers: TimerRegistry::new(runtime),
            settings,
            instances: Mutex::new(HashMap::new()),
        })
    }

    /// `self.self_ref` always upgrades while any `&self` call (this one
    /// included) is running: that call itself is reachable only through a
    /// live `Arc<PluginHost>` strong reference.
    fn arc(&self) -> Arc<Self> {
        self.self_ref
            .upgrade()
            .expect("PluginHost is called only through a live Arc<Self>")
    }

    /// The `settings.schema.json` this plugin id validates its settings
    /// against, if any. M1 only ever resolves one for bridge B2's own
    /// plugin id: no in-tree plugin ships a real `settings.schema.json`
    /// yet (`vosu-osu` is a parallel M1 slice), and `LoadedPlugin` carries
    /// no manifest directory a general resolver could read one from —
    /// recorded in the original handoff notes as a spec/tooling gap, not
    /// addressed here.
    fn schema_for(plugin_id: &str) -> Option<super::settings::SettingsSchema> {
        (plugin_id == BRIDGE_B2_PLUGIN_ID).then(ConfigSettingsBridgeB2::schema)
    }

    /// The attached session's public id to attribute `plugin_id`'s next
    /// `LOG_RECORD` to, once session lifecycle (§6.2) routes through this
    /// host — always `None` until then.
    fn log_session(&self, _instance: PluginInstanceId) -> Option<String> {
        None
    }

    /// Creates and registers one plugin instance: resolves `plugin_id`'s
    /// settings schema (currently only bridge B2's own), builds
    /// `init.settings` through `PluginSettingsService` (§6.9), then
    /// `PluginInstance::create`s it with this host as its
    /// `PluginHostHandler`.
    pub fn create_instance(
        self: &Arc<Self>,
        id: PluginInstanceId,
        plugin: &LoadedPlugin,
        data_dir: PathBuf,
        concurrent_pool: Option<Arc<WorkerPool>>,
    ) -> Result<(), HostError> {
        let plugin_id = plugin.manifest().id.clone();
        let schema = Self::schema_for(&plugin_id);
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
        let instance = PluginInstance::create(
            id,
            plugin,
            &init,
            self.clone() as Arc<dyn PluginHostHandler>,
            concurrent_pool,
        )?;
        self.instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(
                id,
                InstanceEntry {
                    instance,
                    plugin_id,
                },
            );
        Ok(())
    }

    /// Destroys every registered instance (daemon shutdown), cancelling
    /// each one's live timers first (§6.11's exit item: "destroy cancels
    /// every timer of the instance"), one instance's hang never extending
    /// another's grace period.
    pub fn destroy_all(&self, grace: Duration) {
        let entries: Vec<(PluginInstanceId, InstanceEntry)> = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .drain()
            .collect();
        for (id, entry) in entries {
            self.timers.cancel_all(id);
            if let Err(error) = entry.instance.destroy(grace) {
                tracing::warn!(%error, "plugin did not destroy within its grace period");
            }
        }
    }

    /// Spawns bridge B2's background `ConfigStore` change watcher (§6.9:
    /// "every later change arrives as `SETTINGS_CHANGED`"), delivering
    /// through this host. A no-op for a `bridge` whose plugin id has no
    /// live instance yet or ever — `settings_changed` below simply finds
    /// nothing to deliver to.
    pub fn watch_bridge_b2(
        self: &Arc<Self>,
        bridge: &Arc<ConfigSettingsBridgeB2>,
        runtime: &Handle,
    ) {
        bridge.watch_config_changes(runtime, Arc::clone(&self.settings), self.clone());
    }

    /// Dispatches `message` directly to `instance`'s own queue, exactly
    /// the per-instance delivery route `TimerDelivery`'s `timer_fired`
    /// uses internally — the hook point a later phase's session (§6.2)
    /// or focus delivery would call once it routes through `PluginHost`
    /// instead of `src/runtime/dev_plugins.rs`'s own instance list.
    pub fn dispatch_to_instance(
        &self,
        instance: PluginInstanceId,
        message: &Message,
    ) -> Result<std::sync::mpsc::Receiver<vosu_status>, HostError> {
        let owned = OwnedMessage::from_message(message, 0)
            .map_err(|error| HostError::InitEncodeFailed(error.to_string()))?;
        let guard = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = guard.get(&instance).ok_or(HostError::UnknownInstance)?;
        entry.instance.dispatch(owned)
    }

    fn dispatch_to(&self, plugin_id: &str, message: &Message) {
        let Ok(owned) = OwnedMessage::from_message(message, 0) else {
            return;
        };
        let guard = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for entry in guard.values().filter(|entry| entry.plugin_id == plugin_id) {
            let _ = entry.instance.dispatch(owned.clone());
        }
    }
}

impl PluginHostHandler for PluginHost {
    fn handle(
        &self,
        instance: PluginInstanceId,
        message: Message,
        _correlation_id: u64,
    ) -> vosu_status {
        match message {
            Message::LogRecord(record) => {
                let plugin_id = self
                    .instances
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .get(&instance)
                    .map(|entry| entry.plugin_id.clone())
                    .unwrap_or_else(|| format!("{instance:?}"));
                let session = self.log_session(instance);
                self.log_router
                    .record(instance, &plugin_id, session.as_deref(), record);
                vosu_status::VOSU_OK
            }
            Message::TimerSet(TimerSet { timer, interval_ms }) => {
                let delivery: Arc<dyn TimerDelivery> = self.arc();
                self.timers.set(instance, timer, interval_ms, delivery)
            }
            Message::TimerCancel(TimerCancel { timer }) => self.timers.cancel(instance, timer),
            other => {
                // Session lifecycle (`SESSION_IDENTIFY`/`SESSION_DECLINE`,
                // §6.2), state reads, scans, and action calls are later
                // milestones: logged, not acted on, exactly as the former
                // `LoggingHandler` did for every opcode.
                tracing::debug!(
                    ?instance,
                    opcode = ?other.opcode(),
                    "plugin sent an unhandled host message (later milestone)"
                );
                vosu_status::VOSU_OK
            }
        }
    }
}

impl TimerDelivery for PluginHost {
    fn timer_fired(&self, instance: PluginInstanceId, timer: u64) {
        let _ = self.dispatch_to_instance(instance, &Message::TimerFired(TimerFired { timer }));
    }
}

impl SettingsChangeDelivery for PluginHost {
    fn settings_changed(&self, plugin_id: &str, message: Message) {
        self.dispatch_to(plugin_id, &message);
    }
}
