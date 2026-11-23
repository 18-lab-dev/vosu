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
//! An instance is registered under [`InstanceSlot::Creating`] before the
//! plugin's own `create` runs, not only once it returns successfully: a
//! plugin can call `TIMER_SET` (or trigger a B2 settings change) from
//! inside its own `create`, and that timer can fire, or that change can
//! arrive, before `create` returns and the real [`PluginInstance`] exists
//! to deliver to. Without this placeholder, such a delivery would find no
//! instance at all and be discarded for good (a timer's one-shot slot is
//! already freed by the time it fires, so the plugin would then wait
//! forever for a `TIMER_FIRED` that never comes). Every delivery that
//! arrives while `Creating` is queued in its own `pending` list instead,
//! and replayed, in order, once the real instance takes its place.
//!
//! Session lifecycle (§6.2) and focus (`FocusTracker`) delivery, and real
//! process discovery feeding `MatcherEngine`, are later phases: this type
//! does not implement them. [`InstanceSlot::plugin_id`] is already the
//! hook a session-scoped log attribution would read, [`PluginHost::handle`]'s
//! fallback arm is where `SESSION_IDENTIFY`/`SESSION_DECLINE` routing
//! plugs in, and [`PluginHost::log_session`] is where an attached
//! session's public id would come from once session lifecycle routes
//! through here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// One `instances` map slot's lifecycle state. Every state keeps
/// `plugin_id`, so attribution (`LOG_RECORD`'s plugin id, §6.10) is
/// available for the instance's whole registered lifetime — including
/// before `create` returns and after `destroy` has started — not only
/// while [`Self::Live`].
enum InstanceSlot {
    /// Registered, but the plugin's own `create` has not returned yet:
    /// see the module doc comment. `pending` accumulates, in order,
    /// every `TIMER_FIRED`/`SETTINGS_CHANGED` this instance would have
    /// received had it already been [`Self::Live`].
    Creating {
        plugin_id: String,
        pending: Vec<PendingDelivery>,
    },
    /// A running instance, answering dispatch immediately.
    Live(InstanceEntry),
    /// `destroy_all` has removed this instance from active duty (no
    /// further dispatch) and is running or has just run its `destroy`,
    /// but has not yet dropped this slot: kept only so a `LOG_RECORD`
    /// the plugin sends while unwinding (§6.10) is still attributed to
    /// its plugin id instead of falling back to the bare instance id.
    Destroying { plugin_id: String },
}

impl InstanceSlot {
    fn plugin_id(&self) -> &str {
        match self {
            Self::Creating { plugin_id, .. } | Self::Destroying { plugin_id } => plugin_id,
            Self::Live(entry) => &entry.plugin_id,
        }
    }
}

/// One delivery queued against a [`InstanceSlot::Creating`] placeholder,
/// replayed, in order, once the real instance takes its place.
struct PendingDelivery {
    message: Message,
}

/// Grace period `create_instance` gives an instance it destroys itself
/// after losing the `destroy_all` race (module doc comment): there is no
/// caller-supplied `grace` available at that point, so this matches the
/// same 5-second shutdown-grace precedent `instance.rs`'s own
/// `DROP_DESTROY_GRACE` cites.
const SHUTDOWN_RACE_DESTROY_GRACE: Duration = Duration::from_secs(5);

/// Owns every live plugin instance (replacing a bare `Vec<PluginInstance>`,
/// `src/runtime/dev_plugins.rs`'s former ad-hoc `LoggingHandler`, and its
/// stub `TIMER_SET`/empty `init.settings` answers) and answers the M1 host
/// services' opcodes for all of them.
pub struct PluginHost {
    self_ref: Weak<PluginHost>,
    log_router: PluginLogRouter,
    timers: TimerRegistry,
    settings: Arc<PluginSettingsService>,
    instances: Mutex<HashMap<PluginInstanceId, InstanceSlot>>,
    /// Set once by [`Self::destroy_all`], before it starts draining
    /// `instances`: tells a [`Self::create_instance`] call still racing it
    /// (the plugin's own `create` still running when shutdown started,
    /// module doc comment) to destroy the instance it just built instead
    /// of publishing it `Live` — the fix for the race this field closes.
    shutting_down: AtomicBool,
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
            shutting_down: AtomicBool::new(false),
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
    ///
    /// Registers `id` under [`InstanceSlot::Creating`] *before* calling
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
    /// Races [`Self::destroy_all`] safely: `shutting_down` is checked
    /// twice, both times under the same `instances` lock `destroy_all`
    /// itself takes, so the two calls never disagree about which of them
    /// "saw" the instance first. The early check (before the plugin's own
    /// `create` even runs) rejects a brand new instance outright once
    /// shutdown has started. The second check — after `create` returns,
    /// atomically with the swap that would otherwise publish the instance
    /// `Live` — closes the actual race this method's doc comment
    /// describes: `destroy_all` only ever skips a `Creating` placeholder
    /// (never removes it, so it never stops waiting for this id), so if
    /// this check loses the race, `destroy_all`'s own loop is guaranteed
    /// to still find and tear down the `Live` entry; if it wins the race,
    /// this call destroys the instance itself instead of publishing it,
    /// and removes the placeholder so `destroy_all`'s wait resolves.
    pub fn create_instance(
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
        self.instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(
                id,
                InstanceSlot::Creating {
                    plugin_id: plugin_id.clone(),
                    pending: Vec::new(),
                },
            );

        let created = (|| {
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
            PluginInstance::create(
                id,
                plugin,
                &init,
                self.clone() as Arc<dyn PluginHostHandler>,
                concurrent_pool,
            )
        })();

        let instance = match created {
            Ok(instance) => instance,
            Err(error) => {
                self.timers.cancel_all(id);
                self.instances
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(&id);
                return Err(error);
            }
        };

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
            guard.remove(&id);
            drop(guard);
            self.timers.cancel_all(id);
            if let Err(error) = instance.destroy(SHUTDOWN_RACE_DESTROY_GRACE) {
                tracing::warn!(
                    ?id, %error,
                    "instance created during shutdown did not destroy within its grace period"
                );
            }
            self.timers.cancel_all(id);
            return Err(HostError::ShuttingDown);
        }
        let previous = guard.insert(
            id,
            InstanceSlot::Live(InstanceEntry {
                instance,
                plugin_id,
            }),
        );
        let pending = match previous {
            Some(InstanceSlot::Creating { pending, .. }) => pending,
            _ => Vec::new(),
        };
        drop(guard);
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
            let _ = entry.instance.dispatch(owned);
        }
    }

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
    pub fn destroy_all(&self, grace: Duration) {
        self.shutting_down.store(true, Ordering::Release);
        loop {
            let next = {
                let mut guard = self
                    .instances
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if guard.is_empty() {
                    break;
                }
                let ready = guard
                    .iter()
                    .find(|(_, slot)| !matches!(slot, InstanceSlot::Creating { .. }))
                    .map(|(id, _)| *id);
                let Some(id) = ready else {
                    // Only `Creating` placeholders remain: wait for their
                    // own `create_instance` calls to resolve them (see
                    // this method's doc comment) rather than dropping
                    // them here.
                    drop(guard);
                    #[expect(
                        clippy::disallowed_methods,
                        reason = "a short poll for a Creating placeholder to resolve into Live or disappear; its own create_instance call finishes it (there is no other backstop — a plugin's create is already documented, §4, as never performing unbounded I/O, the same trust this loop relies on)"
                    )]
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                };
                let slot = guard.remove(&id).expect("just read this key from the map");
                match slot {
                    InstanceSlot::Live(entry) => {
                        guard.insert(
                            id,
                            InstanceSlot::Destroying {
                                plugin_id: entry.plugin_id,
                            },
                        );
                        (id, Some(entry.instance))
                    }
                    InstanceSlot::Destroying { .. } => (id, None),
                    InstanceSlot::Creating { .. } => unreachable!("excluded by `find` above"),
                }
            };
            let (id, instance) = next;
            self.timers.cancel_all(id);
            if let Some(instance) = instance {
                if let Err(error) = instance.destroy(grace) {
                    tracing::warn!(?id, %error, "plugin did not destroy within its grace period");
                }
                self.timers.cancel_all(id);
            }
            self.instances
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&id);
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
    ) -> tokio::task::JoinHandle<()> {
        bridge.watch_config_changes(runtime, Arc::clone(&self.settings), self.clone())
    }

    /// Dispatches `message` directly to `instance`'s own queue, exactly
    /// the per-instance delivery route `TimerDelivery`'s `timer_fired`
    /// uses internally — the hook point a later phase's session (§6.2)
    /// or focus delivery would call once it routes through `PluginHost`
    /// instead of `src/runtime/dev_plugins.rs`'s own instance list.
    /// Answers `UnknownInstance` for an id that is not currently
    /// [`InstanceSlot::Live`] (including one still `Creating` or already
    /// `Destroying`): a caller needing `Creating`-aware queuing is
    /// `timer_fired`/`settings_changed`'s own narrower logic, not this
    /// general hook.
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
        match guard.get(&instance) {
            Some(InstanceSlot::Live(entry)) => entry.instance.dispatch(owned),
            _ => Err(HostError::UnknownInstance),
        }
    }

    fn dispatch_to(&self, plugin_id: &str, message: &Message) {
        let Ok(owned) = OwnedMessage::from_message(message, 0) else {
            return;
        };
        let mut guard = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for slot in guard.values_mut() {
            match slot {
                InstanceSlot::Live(entry) if entry.plugin_id == plugin_id => {
                    let _ = entry.instance.dispatch(owned.clone());
                }
                InstanceSlot::Creating {
                    plugin_id: creating_id,
                    pending,
                } if creating_id == plugin_id => pending.push(PendingDelivery {
                    message: message.clone(),
                }),
                _ => {}
            }
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
                    .map(|slot| slot.plugin_id().to_owned())
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
    /// Delivers through the owning instance's own dispatch queue when it
    /// is already [`InstanceSlot::Live`]; queues the fire for replay
    /// (module doc comment) when it is still `Creating`; drops it
    /// silently once it is `Destroying` or gone (`destroy_all` has
    /// already cancelled this instance's timers by the time either state
    /// is reached, so this should not normally be reached either way).
    /// This call itself runs under `TimerRegistry`'s internal lock
    /// (`TimerDelivery`'s contract), so it must not block or re-enter the
    /// registry, and does neither — it only ever queues a message onto a
    /// bounded channel or a `Vec`.
    fn timer_fired(&self, instance: PluginInstanceId, timer: u64) {
        let message = Message::TimerFired(TimerFired { timer });
        let mut guard = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match guard.get_mut(&instance) {
            Some(InstanceSlot::Live(entry)) => {
                if let Ok(owned) = OwnedMessage::from_message(&message, 0) {
                    let _ = entry.instance.dispatch(owned);
                }
            }
            Some(InstanceSlot::Creating { pending, .. }) => {
                pending.push(PendingDelivery { message })
            }
            Some(InstanceSlot::Destroying { .. }) | None => {}
        }
    }
}

impl SettingsChangeDelivery for PluginHost {
    fn settings_changed(&self, plugin_id: &str, message: Message) {
        self.dispatch_to(plugin_id, &message);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Condvar;
    use std::time::Instant;

    use serde_json::{Map, Value};

    use super::super::registry;
    use super::super::settings::{SettingsError, SettingsStorage};
    use super::*;
    use crate::logging::{ConsoleSink, LoggerOptions};

    struct DummyStorage;
    impl SettingsStorage for DummyStorage {
        fn load(&self, _plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
            Ok(Map::new())
        }
        fn store(
            &self,
            _plugin_id: &str,
            _values: &Map<String, Value>,
        ) -> Result<(), SettingsError> {
            Ok(())
        }
    }

    struct SilentConsole;
    impl ConsoleSink for SilentConsole {
        fn is_tty(&self) -> bool {
            false
        }
        fn write_log(&self, _text: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn clear_progress(&self, _lines: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn write_progress(&self, _text: &str) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Returns the backing `TempDir` too, so the caller keeps it alive
    /// for exactly its own test's scope instead of leaking it into
    /// `/tmp` forever.
    fn test_host(runtime: Handle) -> (tempfile::TempDir, Arc<PluginHost>) {
        let log_dir = tempfile::tempdir().unwrap();
        let logger = Logger::new(
            log_dir.path(),
            LoggerOptions {
                version: "0.1.0".to_owned(),
                debug_log: false,
                queue_capacity: 64,
            },
            Arc::new(SilentConsole),
        )
        .unwrap();
        let settings = Arc::new(PluginSettingsService::new(Arc::new(DummyStorage)));
        (log_dir, PluginHost::new(logger, settings, runtime))
    }

    /// Blocks the gated plugin's own `create` until released, so a test
    /// can force a `TIMER_SET` it made from inside `create` to fire —
    /// and be queued, not delivered or lost — strictly before `create`
    /// itself returns: an ordering this gate decides, never how fast
    /// `create` happens to run on whatever machine runs this test.
    struct CreateGate {
        release: Mutex<bool>,
        release_cv: Condvar,
        delivered: Mutex<bool>,
        delivered_cv: Condvar,
    }

    impl CreateGate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                release: Mutex::new(false),
                release_cv: Condvar::new(),
                delivered: Mutex::new(false),
                delivered_cv: Condvar::new(),
            })
        }

        fn wait_for_release(&self) {
            let mut release = self.release.lock().unwrap_or_else(|p| p.into_inner());
            while !*release {
                release = self
                    .release_cv
                    .wait(release)
                    .unwrap_or_else(|p| p.into_inner());
            }
        }

        fn release(&self) {
            *self.release.lock().unwrap_or_else(|p| p.into_inner()) = true;
            self.release_cv.notify_all();
        }

        fn mark_delivered(&self) {
            *self.delivered.lock().unwrap_or_else(|p| p.into_inner()) = true;
            self.delivered_cv.notify_all();
        }

        fn wait_for_delivery(&self, timeout: Duration) -> bool {
            let (guard, result) = self
                .delivered_cv
                .wait_timeout_while(
                    self.delivered.lock().unwrap_or_else(|p| p.into_inner()),
                    timeout,
                    |delivered| !*delivered,
                )
                .unwrap_or_else(|p| p.into_inner());
            drop(guard);
            !result.timed_out()
        }
    }

    fn create_gate_registry() -> &'static Mutex<HashMap<String, Arc<CreateGate>>> {
        static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, Arc<CreateGate>>>> =
            std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
        &REGISTRY
    }

    /// A plugin whose own `create` sets a one-shot timer (§6.11) and then
    /// blocks on this test's [`CreateGate`] before returning, so the test
    /// can hold `create` open across the real timer firing. `on_message`
    /// marks the gate once it actually receives the replayed
    /// `TIMER_FIRED` — the end-to-end proof this finding's fix requires,
    /// not merely that something got queued.
    mod gated_timer_plugin {
        use std::sync::Arc;

        use vosu_plugin_api::{
            HostServices, Init, Message, Plugin, PluginError, TimerFired, TimerSet,
            VtableHostServices, vosu_status,
        };

        use super::{CreateGate, create_gate_registry};

        pub struct GatedTimerPlugin {
            gate: Arc<CreateGate>,
        }

        impl Plugin for GatedTimerPlugin {
            fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
                let status = host.send(
                    &Message::TimerSet(TimerSet {
                        timer: 1,
                        interval_ms: None,
                    }),
                    0,
                );
                if status != vosu_status::VOSU_OK {
                    return Err(status.into());
                }
                let gate = create_gate_registry()
                    .lock()
                    .unwrap()
                    .get(&init.plugin_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        panic!("no create gate registered for {:?}", init.plugin_id)
                    });
                gate.wait_for_release();
                Ok(Self { gate })
            }

            fn on_message(
                &mut self,
                message: Message,
                _correlation_id: u64,
            ) -> Result<(), PluginError> {
                if let Message::TimerFired(TimerFired { timer: 1 }) = message {
                    self.gate.mark_delivered();
                }
                Ok(())
            }
        }

        vosu_plugin_api::export_plugin!(bundled GatedTimerPlugin);
    }

    /// The new-finding P2 race, closed: a `TIMER_SET` the plugin makes
    /// from inside its own `create` fires, and is handed to
    /// `PluginHost::timer_fired`, while `create_instance` has not yet
    /// replaced the `Creating` placeholder with the real instance — this
    /// test gates `create` open until it observes exactly that (the
    /// fire queued in `InstanceSlot::Creating`'s own `pending` list, a
    /// real internal state transition this test waits on, never a fixed
    /// sleep guessing the two have raced) — and then proves the queued
    /// fire still reaches the plugin once `create` finally returns,
    /// rather than being discarded for good.
    #[test]
    fn a_timer_set_during_create_that_fires_before_registration_is_queued_and_replayed() {
        let plugin_id = "acme.gated-timer";
        let gate = CreateGate::new();
        create_gate_registry()
            .lock()
            .unwrap()
            .insert(plugin_id.to_owned(), Arc::clone(&gate));

        let manifest_text = format!(
            "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='Gated Timer'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n"
        );
        let loaded = registry::load_bundled(&[registry::BundledPlugin {
            manifest: &manifest_text,
            vtable: gated_timer_plugin::vosu_plugin_vtable,
        }])
        .expect("loading the gated-timer test plugin");

        let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
        let _enter = runtime.enter();
        let (_log_dir, host) = test_host(runtime.handle().clone());
        let data_dir = tempfile::tempdir().unwrap();
        let id = PluginInstanceId::new(1);

        std::thread::scope(|scope| {
            let host_ref = &host;
            let loaded_ref = &loaded[0];
            let data_dir_path = data_dir.path().to_path_buf();
            let create_thread =
                scope.spawn(move || host_ref.create_instance(id, loaded_ref, data_dir_path, None));

            // Wait for the real one-shot timer (fires after
            // `timers::MIN_TIMER_GRANULARITY`) to have already reached
            // `timer_fired` and queued itself against the still-`Creating`
            // slot — proof the race actually happened before
            // `create_instance` got anywhere near installing the real
            // instance, not an assumption resting on scheduling luck.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let queued = {
                    let guard = host.instances.lock().unwrap_or_else(|p| p.into_inner());
                    matches!(
                        guard.get(&id),
                        Some(InstanceSlot::Creating { pending, .. }) if !pending.is_empty()
                    )
                };
                if queued {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "expected the timer to fire and queue against the Creating placeholder within 5s"
                );
                std::thread::sleep(Duration::from_millis(1));
            }

            gate.release();
            create_thread
                .join()
                .unwrap()
                .expect("creating the gated-timer instance");
        });

        assert!(
            gate.wait_for_delivery(Duration::from_secs(5)),
            "the timer fire queued during create must still reach the plugin once replayed"
        );

        host.destroy_all(Duration::from_secs(5));
    }

    /// A plugin whose own `create` blocks on this test's [`CreateGate`]
    /// (reused from the timer-race fixture above) until released, and
    /// whose `Drop` records that it was actually destroyed — the proof
    /// `destroy_all_does_not_leak_an_instance_still_creating_when_it_races`
    /// needs that losing the shutdown race tears the instance down
    /// instead of merely refusing to publish it.
    mod race_gated_plugin {
        use std::sync::atomic::{AtomicBool, Ordering};

        use vosu_plugin_api::{Init, Message, Plugin, PluginError, VtableHostServices};

        use super::create_gate_registry;

        pub static DESTROYED: AtomicBool = AtomicBool::new(false);

        pub struct RaceGatedPlugin;

        impl Plugin for RaceGatedPlugin {
            fn create(_host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
                let gate = create_gate_registry()
                    .lock()
                    .unwrap()
                    .get(&init.plugin_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        panic!("no create gate registered for {:?}", init.plugin_id)
                    });
                gate.wait_for_release();
                Ok(Self)
            }

            fn on_message(
                &mut self,
                _message: Message,
                _correlation_id: u64,
            ) -> Result<(), PluginError> {
                Ok(())
            }
        }

        impl Drop for RaceGatedPlugin {
            fn drop(&mut self) {
                DESTROYED.store(true, Ordering::SeqCst);
            }
        }

        vosu_plugin_api::export_plugin!(bundled RaceGatedPlugin);
    }

    /// The race this finding's fix closes: `destroy_all` must not drop a
    /// still-`Creating` placeholder and declare itself done while
    /// `create_instance`'s own thread is still inside the plugin's
    /// `create` — otherwise that call's later swap installs a `Live`
    /// entry `destroy_all` has already finished draining, leaking a
    /// running instance forever. Proves both ends: `create_instance`
    /// loses the race with `HostError::ShuttingDown`, and the instance it
    /// built is actually destroyed (not merely unpublished), with no
    /// `Live` entry left behind afterward.
    #[test]
    fn destroy_all_does_not_leak_an_instance_still_creating_when_it_races() {
        let plugin_id = "acme.race-gated";
        let gate = CreateGate::new();
        create_gate_registry()
            .lock()
            .unwrap()
            .insert(plugin_id.to_owned(), Arc::clone(&gate));
        race_gated_plugin::DESTROYED.store(false, Ordering::SeqCst);

        let manifest_text = format!(
            "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='Race Gated'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n"
        );
        let loaded = registry::load_bundled(&[registry::BundledPlugin {
            manifest: &manifest_text,
            vtable: race_gated_plugin::vosu_plugin_vtable,
        }])
        .expect("loading the race-gated test plugin");

        let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
        let _enter = runtime.enter();
        let (_log_dir, host) = test_host(runtime.handle().clone());
        let data_dir = tempfile::tempdir().unwrap();
        let id = PluginInstanceId::new(1);

        std::thread::scope(|scope| {
            let host_ref = &host;
            let loaded_ref = &loaded[0];
            let data_dir_path = data_dir.path().to_path_buf();
            let create_thread =
                scope.spawn(move || host_ref.create_instance(id, loaded_ref, data_dir_path, None));

            // Wait for `create_instance` to register the `Creating`
            // placeholder: the plugin's own `create` is now blocked on
            // the gate.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let creating = matches!(
                    host.instances
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .get(&id),
                    Some(InstanceSlot::Creating { .. })
                );
                if creating {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "expected the Creating placeholder to appear within 5s"
                );
                std::thread::sleep(Duration::from_millis(1));
            }

            let destroy_thread = scope.spawn(|| host_ref.destroy_all(Duration::from_secs(5)));

            // Wait for `destroy_all` to have actually set `shutting_down`
            // before releasing `create`, so this test exercises the exact
            // race window the fix closes rather than letting `create`
            // finish first by scheduling luck.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !host.shutting_down.load(Ordering::Acquire) {
                assert!(
                    Instant::now() < deadline,
                    "expected destroy_all to observe the race within 5s"
                );
                std::thread::sleep(Duration::from_millis(1));
            }

            gate.release();

            let create_result = create_thread.join().unwrap();
            destroy_thread.join().unwrap();

            assert!(
                matches!(create_result, Err(HostError::ShuttingDown)),
                "expected create_instance to lose the shutdown race, got {create_result:?}"
            );
        });

        assert!(
            race_gated_plugin::DESTROYED.load(Ordering::SeqCst),
            "the instance created during the shutdown race must still be destroyed, not leaked"
        );
        assert!(
            host.instances
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_empty(),
            "destroy_all must not leave a Live entry behind after racing create_instance"
        );
    }
}
