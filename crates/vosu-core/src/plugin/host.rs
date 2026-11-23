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
//! Session lifecycle (§6.2, [`super::session::SessionRegistry`]) and the
//! Focus rule (`docs/v2/api-v1.md`, [`super::focus::FocusTracker`]) route
//! through this host too: [`PluginHost::process_appeared`]/
//! [`PluginHost::process_exited`] are discovery's own entry points (a
//! later phase feeds them from the real OS process list; this phase's
//! own tests drive them with synthetic [`ProcessRecord`]s), and
//! [`PluginHost::handle`] routes `SESSION_IDENTIFY`/`SESSION_DECLINE`.
//! Read plans (§6.3, including the §4 `VOSU_READ_PLAN_SYNC` exception,
//! `read_plans.rs`) and scans (§6.4, `scans.rs`) both route through this
//! host too, sharing `read_plans.rs`'s own per-session cached process
//! handle ([`PluginHost::session_memory`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use vosu_plugin_api::{
    Bitness, FocusChanged, Init, Message, SessionDecline, TimerCancel, TimerFired, TimerSet,
    encode, vosu_status,
};

use super::dispatch::{OwnedMessage, WorkerPool};
use super::error::HostError;
use super::focus::{
    FocusCandidate, FocusEvent, FocusKey, FocusTracker, ForegroundSource, OsForegroundSource,
};
use super::host_services::{PluginHostHandler, PluginInstanceId};
use super::instance::PluginInstance;
use super::logging::PluginLogRouter;
use super::matcher::{CandidateProcess, MatcherEngine, host_platform};
use super::read_plan::OutstandingPlans;
use super::registry::LoadedPlugin;
use super::scan::{OutstandingScans, ScanWorkerPool};
use super::session::{DiscoveredProcess, SessionHandle, SessionRegistry};
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
    /// This instance's own asynchronous (non-`VOSU_READ_PLAN_SYNC`) read
    /// plans (§6.3, `read_plans.rs`): capped at
    /// [`super::read_plan::MAX_OUTSTANDING_ASYNC_PLANS`], freed on
    /// `READ_PLAN_CANCEL` or the owning session's detach.
    outstanding_plans: Arc<OutstandingPlans>,
    /// This instance's own `VOSU_OP_SCAN_REQUEST`s (§6.4, `scans.rs`):
    /// capped at [`super::scan::MAX_OUTSTANDING_SCANS`], freed on
    /// delivery or the owning session's detach — a scan has no
    /// plugin-initiated cancel opcode of its own (`scans.rs`'s own module
    /// doc comment).
    outstanding_scans: Arc<OutstandingScans>,
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

/// One process discovery (phase (d2)) observed, enough to match it
/// against every registered game plugin's `[match]` table (§6.1) and
/// attach a session to the winner (§6.2):
/// [`PluginHost::process_appeared`]'s own parameter. Synthetic in this
/// phase's tests (`docs/v2/implementation-plan.md` M1 Exit: "Tests drive
/// matching with synthetic process records").
#[derive(Debug, Clone)]
pub struct ProcessRecord {
    pub pid: u32,
    /// The executable file name only (no directory), e.g. `osu!.exe`.
    pub name: String,
    /// The full executable path.
    pub path: String,
    /// The joined argv.
    pub command_line: String,
    pub bitness: Bitness,
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
    instances: Mutex<HashMap<PluginInstanceId, InstanceSlot>>,
    /// Notified whenever [`Self::create_instance`] resolves a
    /// `Creating` placeholder (publishes it `Live`, or removes it on a
    /// failed/shutdown-raced `create`) — lets [`Self::destroy_all`]'s own
    /// wait for the last `Creating` placeholders to resolve (P3) block on
    /// this condvar, bounded by a deadline, instead of polling
    /// `instances` on a fixed sleep.
    instances_changed: Condvar,
    /// Set once by [`Self::destroy_all`], before it starts draining
    /// `instances`: tells a [`Self::create_instance`] call still racing it
    /// (the plugin's own `create` still running when shutdown started,
    /// module doc comment) to destroy the instance it just built instead
    /// of publishing it `Live` — the fix for the race this field closes.
    shutting_down: AtomicBool,
    /// Every enabled game plugin's compiled `[match]` table, in install
    /// order (§6.1, D18), registered by [`Self::create_instance`].
    matcher: Mutex<MatcherEngine>,
    /// Session lifecycle bookkeeping (§6.2): PID generations,
    /// `SESSION_IDENTIFY`/`SESSION_DECLINE` processing, the 5-second
    /// identification timeout, `focus_priority` holding.
    sessions: Mutex<SessionRegistry>,
    /// The Focus rule's own state (`docs/v2/api-v1.md` "Focus rule"):
    /// currently focused session and recency fallback.
    focus: Mutex<FocusTracker>,
    /// Which process (if any) owns the OS foreground window, for the
    /// Focus rule: [`OsForegroundSource`] in production,
    /// `focus::SyntheticForegroundSource` in tests.
    foreground: Arc<dyn ForegroundSource>,
    /// Read-plan routing's per-session cached process handle (§6.3,
    /// `read_plans.rs`): opened once per [`SessionHandle`], reused until
    /// the session detaches. A module table is deliberately not cached
    /// alongside it (`scan.rs`'s own documented rule: fresh per call).
    /// F5: holds a `SessionMemorySlot` (its own doc comment), not the
    /// open result directly, so the actual open never runs under this
    /// lock.
    read_plan_memory: Mutex<HashMap<SessionHandle, Arc<read_plans::SessionMemorySlot>>>,
    /// Bounded worker pool async read plans AND scans both execute on
    /// (§4, §6.4): one shared [`ScanWorkerPool`] instance
    /// ([`read_plans::new_background_pool`]) rather than one per opcode —
    /// both are off the hot path, so splitting worker threads between
    /// them buys nothing but doubles the idle OS threads every
    /// `PluginHost` keeps parked.
    background_pool: ScanWorkerPool,
    /// [`read_plans::READ_PLAN_DEADLINE`] in milliseconds; mutable only so
    /// a test can shorten it (`read_plans::set_read_plan_deadline_for_tests`).
    read_plan_deadline_ms: AtomicU64,
    /// Last time this host logged a dropped delivery caused by a full
    /// per-instance dispatch queue (P3): rate-limited to once per second
    /// total (not per instance) so a plugin stuck with a full queue,
    /// dropping every `TIMER_FIRED`/`SESSION_DETACHED`/read-plan-or-scan
    /// result sent to it, cannot flood the log the way an unrated
    /// per-drop `tracing::warn!` would.
    last_dispatch_queue_full_log: Mutex<Option<Instant>>,
}

impl PluginHost {
    /// `runtime` backs both the log router's background summary-flush
    /// task and the timer registry's spawned timer tasks (`Handle::spawn`
    /// works from any thread, unlike the ambient `tokio::spawn`, so a
    /// plugin instance's own dispatch thread — a plain OS thread, not a
    /// Tokio runtime, §4 — can still drive both through this one host),
    /// plus the 5-second session-identification sweep task (§6.2) this
    /// constructor also spawns.
    pub fn new(logger: Logger, settings: Arc<PluginSettingsService>, runtime: Handle) -> Arc<Self> {
        Self::new_inner(
            logger,
            settings,
            runtime,
            Arc::new(OsForegroundSource),
            super::session::IDENTIFICATION_TIMEOUT,
        )
    }

    /// Test-only entry point: lets a test inject a
    /// `focus::SyntheticForegroundSource` and a short identification
    /// timeout instead of the real 5-second one, without changing
    /// [`Self::new`]'s own public signature (the one production call
    /// site, `src/runtime/lifecycle.rs`, needs neither).
    #[cfg(test)]
    fn new_for_tests(
        logger: Logger,
        settings: Arc<PluginSettingsService>,
        runtime: Handle,
        foreground: Arc<dyn ForegroundSource>,
        identification_timeout: Duration,
    ) -> Arc<Self> {
        Self::new_inner(
            logger,
            settings,
            runtime,
            foreground,
            identification_timeout,
        )
    }

    fn new_inner(
        logger: Logger,
        settings: Arc<PluginSettingsService>,
        runtime: Handle,
        foreground: Arc<dyn ForegroundSource>,
        identification_timeout: Duration,
    ) -> Arc<Self> {
        let host = Arc::new_cyclic(|self_ref| Self {
            self_ref: self_ref.clone(),
            log_router: PluginLogRouter::new(logger, runtime.clone()),
            timers: TimerRegistry::new(runtime.clone()),
            settings,
            instances: Mutex::new(HashMap::new()),
            instances_changed: Condvar::new(),
            shutting_down: AtomicBool::new(false),
            matcher: Mutex::new(MatcherEngine::new(host_platform())),
            sessions: Mutex::new(SessionRegistry::with_identification_timeout(
                identification_timeout,
            )),
            focus: Mutex::new(FocusTracker::new()),
            foreground,
            read_plan_memory: Mutex::new(HashMap::new()),
            background_pool: read_plans::new_background_pool(),
            read_plan_deadline_ms: AtomicU64::new(
                u64::try_from(read_plans::READ_PLAN_DEADLINE.as_millis()).unwrap_or(u64::MAX),
            ),
            last_dispatch_queue_full_log: Mutex::new(None),
        });
        // P2-4: spawned only after `new_cyclic` has returned a real
        // `Arc<Self>` — never from inside its own closure, where no
        // strong `Arc` exists yet. A multi-thread runtime can poll a
        // task spawned from inside `new_cyclic` immediately (even
        // concurrently with `new_cyclic` itself still running), so
        // `identification_sweep_loop`'s first `host.upgrade()` could
        // observe `None` and return for good before construction even
        // finished — the §6.2 "logs once" timeout warning then never
        // fires again. `Arc::downgrade` here instead always has a live
        // `Arc` to weaken.
        runtime.spawn(identification_sweep_loop(Arc::downgrade(&host)));
        host
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
        let matcher_table = plugin.manifest().matcher.clone();
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
            self.instances_changed.notify_all();
            self.timers.cancel_all(id);
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
        if let Some(table) = matcher_table {
            // F3: registered only now, atomically with publishing `Live`
            // under this same `instances` lock — never while `id` was
            // only `Creating`, so `process_appeared` can never attach a
            // session to a plugin instance that might still fail to come
            // up. A refusal here (a duplicate id already registered)
            // tears this instance down exactly like the two paths above:
            // it never got a chance to publish `Live` either.
            if let Err(error) = self
                .matcher
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .register(plugin_id.clone(), table)
            {
                guard.remove(&id);
                drop(guard);
                self.instances_changed.notify_all();
                self.timers.cancel_all(id);
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
        let previous = guard.insert(
            id,
            InstanceSlot::Live(InstanceEntry {
                instance,
                plugin_id,
                outstanding_plans: Arc::new(OutstandingPlans::new()),
                outstanding_scans: OutstandingScans::new(),
            }),
        );
        let pending = match previous {
            Some(InstanceSlot::Creating { pending, .. }) => pending,
            _ => Vec::new(),
        };
        drop(guard);
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

    fn dispatch_to(&self, plugin_id: &str, message: &Message) {
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
    fn plugin_id_of(&self, instance: PluginInstanceId) -> Option<String> {
        self.instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&instance)
            .map(|slot| slot.plugin_id().to_owned())
    }

    /// The Focus rule's current inputs (`docs/v2/api-v1.md` "Focus
    /// rule"): every identified session re-keyed to [`FocusKey`] with its
    /// owning plugin's install order, plus the current `focus_priority`
    /// holder. Locks `sessions` then `matcher`, in that order — callers
    /// (P2-6) already hold `focus` before calling this, so the full lock
    /// order every focus-recomputing call site below uses is `focus`,
    /// then `sessions`, then `matcher`, so two such calls never deadlock
    /// against each other.
    fn focus_inputs(&self) -> (Vec<FocusCandidate>, Option<FocusKey>) {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let matcher = self
            .matcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let candidates = sessions
            .identified_sessions()
            .map(|(handle, owner, pid)| FocusCandidate {
                handle: FocusKey(handle.0),
                pid,
                install_order: matcher.install_order_of(owner),
            })
            .collect();
        let priority_holder = sessions.priority_holder().map(|handle| FocusKey(handle.0));
        (candidates, priority_holder)
    }

    /// Delivers every `VOSU_OP_FOCUS_CHANGED` event in `events`'s own
    /// order (loss before gain, §6.2 "Delivery ordering"), to each
    /// session's owning plugin. Called while the caller (P2-6,
    /// [`Self::recompute_focus`]/[`Self::identify_focus`]) still holds
    /// `focus`, so a concurrent recompute can never interleave with, or
    /// reorder, this delivery; every send `dispatch_to` makes is already
    /// a non-blocking `try_send` (`SerialWorker::submit`/`WorkerPool`'s
    /// own queue), so holding `focus` across it never risks blocking on a
    /// slow or stuck plugin.
    fn deliver_focus_events(&self, events: Vec<FocusEvent>) {
        for event in events {
            let handle = SessionHandle(event.handle.0);
            let owner = {
                let sessions = self
                    .sessions
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                sessions
                    .owner_and_pid(handle)
                    .map(|(owner, _)| owner.clone())
            };
            if let Some(owner) = owner {
                self.dispatch_to(
                    &owner,
                    &Message::FocusChanged(FocusChanged {
                        handle: handle.0,
                        focused: event.focused,
                    }),
                );
            }
        }
    }

    /// Recomputes focus from the current session set and delivers any
    /// resulting `FOCUS_CHANGED` events: call after any change to which
    /// sessions are identified or who holds `focus_priority`. Holds
    /// `focus` for the whole call, including delivery (P2-6): taking
    /// `sessions`/`matcher` (inside [`Self::focus_inputs`]) only after
    /// `focus` is already held, and delivering only while still holding
    /// it, closes the race where a concurrent recompute running from an
    /// older snapshot could finish second and deliver events out of the
    /// §6.2 "loss before gain" order relative to this one.
    fn recompute_focus(&self) {
        let mut focus = self
            .focus
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (candidates, priority_holder) = self.focus_inputs();
        let events = focus.recompute(&candidates, priority_holder, self.foreground.as_ref());
        self.deliver_focus_events(events);
        drop(focus);
    }

    /// `SESSION_IDENTIFY` just exposed `handle`: §6.2 "a plugin that
    /// needs [focus] waits for the first `FOCUS_CHANGED` (sent once,
    /// promptly, after the session is identified, even if focus does not
    /// change relative to the previous state)" — `FocusTracker::recompute`
    /// alone would miss that case, so this always uses
    /// `on_session_identified` instead of [`Self::recompute_focus`]. Same
    /// `focus`-held-through-delivery discipline as `recompute_focus`
    /// (P2-6).
    fn identify_focus(&self, handle: SessionHandle) {
        let mut focus = self
            .focus
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let (candidates, priority_holder) = self.focus_inputs();
        let events = focus.on_session_identified(
            FocusKey(handle.0),
            &candidates,
            priority_holder,
            self.foreground.as_ref(),
        );
        self.deliver_focus_events(events);
        drop(focus);
    }

    /// Discovery's own entry point (phase (d2) calls this from a real OS
    /// process list; this phase only provides and tests it): matches
    /// `process` against every registered game plugin's `MatcherEngine`
    /// (§6.1), attaches a session to the winner (`SessionRegistry::attach`,
    /// §6.2), and delivers `SESSION_ATTACHED` to it. A no-op when no
    /// plugin's matcher accepts `process`, or when `SessionRegistry`
    /// already tracks its pid (a rescan of a still-running process, D6).
    pub fn process_appeared(&self, process: ProcessRecord) {
        let candidate = CandidateProcess {
            name: process.name.clone(),
            path: process.path.clone(),
            command_line: process.command_line.clone(),
        };
        let outcome = self
            .matcher
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .match_candidate(&candidate);
        let Some(winner) = outcome.winner else {
            return;
        };
        if !outcome.shadowed.is_empty() {
            tracing::warn!(
                pid = process.pid,
                winner = %winner,
                shadowed = ?outcome.shadowed,
                "multiple plugins matched this process; the earliest installed one wins (D18)"
            );
        }
        // `outcome.winner` plus `outcome.shadowed`, in that order, is
        // already the full install-order candidate list `attach` wants
        // (`MatchOutcome`'s own doc comment).
        let mut candidates = Vec::with_capacity(1 + outcome.shadowed.len());
        candidates.push(winner.clone());
        candidates.extend(outcome.shadowed);
        let discovered = DiscoveredProcess {
            pid: process.pid,
            bitness: process.bitness,
        };
        let attached = self
            .sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .attach(discovered, candidates, Instant::now());
        if let Some(attached) = attached {
            self.dispatch_to(&winner, &Message::SessionAttached(attached.message));
        }
    }
}
impl PluginHostHandler for PluginHost {
    fn handle(
        &self,
        instance: PluginInstanceId,
        message: Message,
        correlation_id: u64,
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
            Message::SessionIdentify(identify) => {
                let Some(plugin_id) = self.plugin_id_of(instance) else {
                    return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
                };
                let handle = SessionHandle(identify.handle);
                let outcome = {
                    self.sessions
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .identify(&plugin_id, handle, identify, Instant::now())
                };
                match outcome {
                    Ok(outcome) => {
                        if outcome.focus_priority_ignored {
                            tracing::warn!(
                                ?handle,
                                %plugin_id,
                                "focus_priority requested while another session already holds it; ignored, not refused (§6.2)"
                            );
                        }
                        self.identify_focus(handle);
                        vosu_status::VOSU_OK
                    }
                    Err(error) => error.into(),
                }
            }
            Message::SessionDecline(SessionDecline { handle }) => {
                let Some(plugin_id) = self.plugin_id_of(instance) else {
                    return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
                };
                let handle = SessionHandle(handle);
                let outcome = {
                    self.sessions
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .decline(&plugin_id, handle, Instant::now())
                };
                match outcome {
                    Ok(super::session::DeclineOutcome::ReOffered {
                        detached,
                        reoffered,
                    }) => {
                        let declined_handle = SessionHandle(detached.handle);
                        // P3: cancel before dispatching `SESSION_DETACHED`
                        // so a result or scan answer already queued for
                        // this handle is delivered before it, never
                        // after, on serial dispatch.
                        self.cancel_read_plans_for_session(&plugin_id, declined_handle);
                        self.cancel_scans_for_session(&plugin_id, declined_handle);
                        self.dispatch_to(&plugin_id, &Message::SessionDetached(detached));
                        self.forget_read_plan_session(declined_handle);
                        // The re-offered handle's own `owner` is already
                        // the next install-order candidate (`decline`'s
                        // own doc comment), so look it up the same way
                        // every other session lookup does rather than
                        // re-deriving it from the matcher.
                        let next_owner = {
                            self.sessions
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .owner_and_pid(reoffered.handle)
                                .map(|(owner, _)| owner.clone())
                        };
                        if let Some(next_owner) = next_owner {
                            self.dispatch_to(
                                &next_owner,
                                &Message::SessionAttached(reoffered.message),
                            );
                        }
                        vosu_status::VOSU_OK
                    }
                    Ok(super::session::DeclineOutcome::Dropped { detached }) => {
                        let declined_handle = SessionHandle(detached.handle);
                        // P3: same cancel-before-dispatch ordering as the
                        // `ReOffered` arm above.
                        self.cancel_read_plans_for_session(&plugin_id, declined_handle);
                        self.cancel_scans_for_session(&plugin_id, declined_handle);
                        self.dispatch_to(&plugin_id, &Message::SessionDetached(detached));
                        self.forget_read_plan_session(declined_handle);
                        vosu_status::VOSU_OK
                    }
                    Err(error) => error.into(),
                }
            }
            Message::ReadPlanSubmit(submit) => self.handle_read_plan_submit(instance, submit),
            Message::ReadPlanCancel(cancel) => self.handle_read_plan_cancel(instance, cancel),
            Message::ScanRequest(request) => {
                self.handle_scan_request(instance, correlation_id, request)
            }
            other => {
                // State reads (`STATE_SUBSCRIBE`), resource/tree/stream
                // operations, and action calls are later milestones:
                // logged, not acted on, exactly as the former
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

/// Background task spawned once per [`PluginHost`] (module doc comment,
/// §6.2): sweeps every still-pending session for the 5-second
/// identification timeout on a tokio interval, logging each one once.
/// Stops once `host` no longer upgrades (the host was dropped).
async fn identification_sweep_loop(host: Weak<PluginHost>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let Some(host) = host.upgrade() else {
            return;
        };
        let timed_out = {
            host.sessions
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .sweep_identification_timeouts(Instant::now())
        };
        for handle in timed_out {
            tracing::warn!(
                ?handle,
                "session did not identify within 5s of SESSION_ATTACHED; keeping it hidden (§6.2)"
            );
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
                if let Ok(owned) = OwnedMessage::from_message(&message, 0)
                    && let Err(error) = entry.instance.dispatch(owned)
                {
                    self.log_dispatch_dropped(instance, message.opcode(), &error);
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

mod lifecycle;
mod read_plans;
mod scans;

#[cfg(test)]
mod tests;
