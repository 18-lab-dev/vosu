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
//! plugin's own `create` runs: a `TIMER_SET`/B2 settings-change delivery
//! can arrive from inside `create` itself, before the real
//! [`PluginInstance`] exists — without this placeholder it would find no
//! instance and be lost for good (a one-shot timer's slot frees once it
//! fires, so the plugin would wait forever for a `TIMER_FIRED` that never
//! comes). `Creating` instead queues every such delivery in `pending`,
//! replayed in order once the real instance takes its place.
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
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use vosu_plugin_api::{
    Bitness, FocusChanged, Message, SessionDecline, TimerCancel, TimerFired, TimerSet, vosu_status,
};

use super::dispatch::OwnedMessage;
use super::focus::{FocusCandidate, FocusEvent, FocusKey, FocusTracker, ForegroundSource};
use super::host_services::PluginHostHandler;
use super::instance::PluginInstance;
use super::logging::PluginLogRouter;
use super::matcher::{CandidateProcess, MatcherEngine, host_platform};
use super::read_plan::OutstandingPlans;
use super::scan::{OutstandingScans, ScanWorkerPool};
use super::session::{DiscoveredProcess, SessionHandle, SessionRegistry};
use super::settings::{
    PluginSettingsService, SettingsChangeDelivery, SettingsSchema, SettingsSchemaSource,
};
use super::timers::TimerDelivery;
use super::timers::TimerRegistry;
use crate::identity::PluginInstanceId;
use crate::logging::Logger;
use crate::state::StateRuntime;

pub use capabilities::validation::{PackageRegistration, ProviderRegistration, ValidationSnapshot};

/// One registered instance's metadata alongside its [`PluginInstance`]:
/// enough to attribute its logs (§6.10) without re-deriving the plugin id
/// from the manifest on every `LOG_RECORD`.
struct InstanceEntry {
    instance: PluginInstance,
    plugin_id: String,
    /// This instance's own parsed `settings.schema.json` (`registry::
    /// LoadedPlugin::settings_schema`), kept here so this host's
    /// `SettingsSchemaSource` impl can resolve `BRIDGE_B2_PLUGIN_ID`'s
    /// (`vosu.osu`'s) schema from its live instance instead of a copy
    /// bridge B2 used to embed itself (P0).
    settings_schema: Option<SettingsSchema>,
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
    publisher: Arc<state::Publisher>,
    capabilities: Vec<capabilities::ProvidedCapability>,
    replaces: Vec<String>,
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
    /// received had it already been [`Self::Live`]. `settings_schema` is
    /// set up front (review finding: a B2 change arriving during a
    /// blocked `create` must resolve `vosu.osu`'s real schema, not `None`
    /// — a `None` resolution sends an empty, spec-violating effective
    /// settings map instead of skipping delivery).
    Creating {
        plugin_id: String,
        settings_schema: Option<SettingsSchema>,
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
/// attach a session to the winner (§6.2).
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
    /// This OS process occurrence's own start time, in a unit private to
    /// each platform's own source (Linux: `/proc/<pid>/stat`'s
    /// `starttime` field, clock ticks since boot; Windows: the
    /// `FILETIME`-shaped value `GetProcessTimes`/`SYSTEM_PROCESS_INFORMATION`'s
    /// `CreateTime` report) — never compared across platforms, only
    /// against another observation of the same pid on the same host
    /// (`discovery.rs::same_occurrence`, P1: identity is `start_time`,
    /// not this struct's other fields, so a still-running process that
    /// rewrites its own argv is never churned).
    pub start_time: u64,
    /// Linux only: the exact NUL-split argv (A1b/DA); `None` on Windows.
    pub argv: Option<Vec<String>>,
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
    state: Arc<StateRuntime>,
    state_delivery: Mutex<state_delivery::DeliveryState>,
    state_runtime: Handle,
    state_serial: Mutex<()>,
    state_demand_serial: Mutex<()>,
    pub(super) surfaces: Mutex<crate::state::surface::SurfaceReaders>,
    api_readers: Mutex<crate::api::ApiReaders>,
    resource_state: Mutex<resources::ResourceState>,
    action_state: Mutex<actions::ActionState>,
    instances: Mutex<HashMap<PluginInstanceId, InstanceSlot>>,
    /// Notified whenever [`Self::create_instance`] resolves a
    /// `Creating` placeholder (publishes it `Live`, or removes it on a
    /// failed/shutdown-raced `create`) — lets [`Self::destroy_all`]'s own
    /// wait for the last `Creating` placeholders to resolve (P3) block on
    /// this condvar instead of polling `instances` on a fixed sleep.
    /// Each individual wait is itself bounded only as a backstop
    /// (`CREATING_WAIT_BACKSTOP`, in case a notification is ever missed),
    /// not an overall deadline: `destroy_all` keeps re-waiting until
    /// every placeholder has actually resolved.
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
    /// The generic discovery notification seam (`observer.rs`, M6b G0).
    discovery_observer: Mutex<observer::ObserverSlot>,
    /// Session lifecycle bookkeeping (§6.2): PID generations,
    /// `SESSION_IDENTIFY`/`SESSION_DECLINE` processing, the 5-second
    /// identification timeout, `focus_priority` holding.
    sessions: Mutex<SessionRegistry>,
    /// The Focus rule's own state (`docs/v2/api-v1.md` "Focus rule"):
    /// currently focused session and recency fallback.
    focus: Mutex<FocusTracker>,
    /// Which process (if any) owns the OS foreground window, for the
    /// Focus rule: the poller's cached sample in production, synthetic in tests.
    foreground: Arc<dyn ForegroundSource>,
    foreground_updates: tokio::sync::watch::Sender<Option<u32>>,
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
    /// Starts `runtime = "process"` instances (`super::execution`).
    /// Installed once by composition; without it a process package is
    /// refused at creation, never run in-process.
    process_launcher: std::sync::OnceLock<Arc<dyn super::execution::ProcessLauncher>>,
    /// Restart and stop state of every `runtime = "process"` instance
    /// (`supervision.rs`, §7.6 "Restart"). Its lock is a leaf.
    supervisor: Arc<super::process::supervisor::Supervisor>,
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
            None,
            super::session::IDENTIFICATION_TIMEOUT,
            StateRuntime::new(),
        )
    }

    /// Composition injects one shared registry with a persisted secret. The
    /// caller chooses its channel-aware data path and checks
    /// [`crate::state::ReferenceStore::is_persistent`] before starting services.
    pub fn new_with_resources(
        logger: Logger,
        settings: Arc<PluginSettingsService>,
        runtime: Handle,
        resources: Arc<crate::state::ReferenceStore>,
    ) -> Arc<Self> {
        Self::new_inner(
            logger,
            settings,
            runtime,
            None,
            super::session::IDENTIFICATION_TIMEOUT,
            StateRuntime::with_resources(resources),
        )
    }
    /// Bench-only deterministic foreground injection. Optimized benches cannot
    /// use `test-harness` (which rejects release builds); production never enables
    /// `bench-harness`. Tests inherit this seam through `test-harness`.
    #[cfg(any(test, feature = "bench-harness"))]
    pub fn new_with_foreground(
        logger: Logger,
        settings: Arc<PluginSettingsService>,
        runtime: Handle,
        foreground: Arc<super::focus::SyntheticForegroundSource>,
    ) -> Arc<Self> {
        Self::new_inner(
            logger,
            settings,
            runtime,
            Some(foreground),
            super::session::IDENTIFICATION_TIMEOUT,
            StateRuntime::new(),
        )
    }

    /// Test-only entry point: lets a test inject a
    /// `focus::SyntheticForegroundSource` and a short identification
    /// timeout instead of the real 5-second one, without changing
    /// [`Self::new`]'s own public signature (the one production call
    /// site, `src/runtime/lifecycle.rs`, needs neither). `pub(crate)`
    /// rather than `host`-module-private: `plugin::discovery`'s own
    /// tests (a sibling module, not a descendant of `host`) build a test
    /// host the same way `host::tests::helpers::test_host_with` does.
    #[cfg(test)]
    pub(crate) fn new_for_tests(
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
            Some(foreground),
            identification_timeout,
            StateRuntime::new(),
        )
    }

    fn new_inner(
        logger: Logger,
        settings: Arc<PluginSettingsService>,
        runtime: Handle,
        foreground: Option<Arc<dyn ForegroundSource>>,
        identification_timeout: Duration,
        state: StateRuntime,
    ) -> Arc<Self> {
        let (foreground_updates, receiver) = tokio::sync::watch::channel(None);
        let foreground = foreground.unwrap_or_else(|| Arc::new(receiver));
        let host = Arc::new_cyclic(|self_ref| Self {
            self_ref: self_ref.clone(),
            log_router: PluginLogRouter::new(logger, runtime.clone()),
            timers: TimerRegistry::new(runtime.clone()),
            settings,
            state: Arc::new(state),
            state_delivery: Mutex::new(state_delivery::DeliveryState::default()),
            state_runtime: runtime.clone(),
            state_serial: Mutex::new(()),
            state_demand_serial: Mutex::new(()),
            surfaces: Mutex::new(Default::default()),
            api_readers: Mutex::new(Default::default()),
            resource_state: Mutex::new(resources::ResourceState::default()),
            action_state: Mutex::new(actions::ActionState::default()),
            instances: Mutex::new(HashMap::new()),
            instances_changed: Condvar::new(),
            shutting_down: AtomicBool::new(false),
            matcher: Mutex::new(MatcherEngine::new(host_platform())),
            discovery_observer: Mutex::new(observer::ObserverSlot::default()),
            sessions: Mutex::new(SessionRegistry::with_identification_timeout(
                identification_timeout,
            )),
            focus: Mutex::new(FocusTracker::new()),
            foreground,
            foreground_updates,
            read_plan_memory: Mutex::new(HashMap::new()),
            background_pool: read_plans::new_background_pool(),
            read_plan_deadline_ms: AtomicU64::new(
                u64::try_from(read_plans::READ_PLAN_DEADLINE.as_millis()).unwrap_or(u64::MAX),
            ),
            last_dispatch_queue_full_log: Mutex::new(None),
            process_launcher: std::sync::OnceLock::new(),
            supervisor: Arc::default(),
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

    pub fn state(&self) -> &Arc<StateRuntime> {
        &self.state
    }

    /// Latest foreground sample, shared with the interim B1 focus reduction.
    pub fn subscribe_foreground(&self) -> tokio::sync::watch::Receiver<Option<u32>> {
        self.foreground_updates.subscribe()
    }

    /// One OS query per poll; focus recomputation only reads the cached value.
    #[cfg(any(windows, test))]
    pub(crate) fn poll_foreground(&self, source: &dyn ForegroundSource) {
        let pid = source.foreground_pid();
        let changed = self.foreground_updates.send_if_modified(|current| {
            if !super::discovery::foreground_owner_changed(*current, pid) {
                false
            } else {
                *current = pid;
                true
            }
        });
        if changed {
            self.foreground_changed();
        }
    }

    /// `self.self_ref` always upgrades while any `&self` call (this one
    /// included) is running: that call itself is reachable only through a
    /// live `Arc<PluginHost>` strong reference.
    fn arc(&self) -> Arc<Self> {
        self.self_ref
            .upgrade()
            .expect("PluginHost is called only through a live Arc<Self>")
    }

    /// The attached session's public id to attribute `plugin_id`'s next
    /// `LOG_RECORD` to, once session lifecycle (§6.2) routes through this
    /// host — always `None` until then.
    fn log_session(&self, _instance: PluginInstanceId) -> Option<String> {
        None
    }

    /// The Focus rule's current inputs (`docs/v2/api-v1.md` "Focus
    /// rule"): every identified session re-keyed to [`FocusKey`] with its
    /// owning plugin's install order, plus the current `focus_priority`
    /// holder. Locks `sessions` then `matcher` while holding `focus`, a
    /// subsequence of the host order: focus -> instances -> sessions ->
    /// matcher -> store/registry. These guards are released before any
    /// subsequent instance lookup or delivery.
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
    fn deliver_focus_events(&self, events: Vec<FocusEvent>) -> Vec<crate::state::CommitOutcome> {
        let mut committed = Vec::new();
        for event in events {
            let handle = SessionHandle(event.handle.0);
            {
                let _serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(outcome) = self
                    .state
                    .store
                    .set_focused(crate::state::SessionKey(handle.0), event.focused)
                {
                    self.state_committed(&outcome);
                    committed.push(outcome);
                }
            }
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
                self.state_focus_changed(FocusChanged {
                    handle: handle.0,
                    focused: event.focused,
                    foreground: event.foreground,
                });
                self.dispatch_to(
                    &owner,
                    &Message::FocusChanged(FocusChanged {
                        handle: handle.0,
                        focused: event.focused,
                        foreground: event.foreground,
                    }),
                );
            }
        }
        committed
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
        let committed = self.deliver_focus_events(events);
        let api_demand = self.api_transition(focus.focused());
        drop(focus);
        for outcome in committed {
            self.surface_committed(&outcome);
        }
        self.surface_focus_changed();
        for key in api_demand {
            self.refresh_state_demand(key);
        }
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
        self.open_bus_session(handle, focus.focused() == Some(FocusKey(handle.0)));
        self.state_session_identified(handle);
        let committed = self.deliver_focus_events(events);
        let api_demand = self.api_transition(focus.focused());
        drop(focus);
        for outcome in committed {
            self.surface_committed(&outcome);
        }
        self.surface_focus_changed();
        for key in api_demand {
            self.refresh_state_demand(key);
        }
    }

    /// Discovery's own entry point (phase (d2) calls this from a real OS
    /// process list; this phase only provides and tests it): matches `process`
    /// against every registered plugin's `MatcherEngine` (§6.1), attaches a
    /// session to the winner, and delivers `SESSION_ATTACHED`. A no-op when no
    /// matcher accepts it, or `SessionRegistry` already tracks its pid (D6).
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
        // Read-only notification after installed matchers ran (`observer.rs`);
        // no host lock is held here.
        self.notify_observed(&process, outcome.winner.is_some());
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
            name: process.name,
            path: process.path,
            command_line: process.command_line,
            argv: process.argv,
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
            Message::ActionCall(call) => self.handle_action_call(instance, call),
            Message::ActionCancel(cancel) => self.handle_action_cancel(instance, cancel),
            Message::ActionProgress(progress) => self.handle_action_progress(instance, progress),
            Message::ActionResult(result) => self.handle_action_result(instance, result),
            Message::RootDeclare(request) => self.handle_root_declare(instance, request),
            Message::ResourceResolved(reply) => self.handle_resource_reply(
                instance,
                correlation_id,
                resource_requests::ResourceReply::Resolved(reply),
            ),
            Message::StreamChunk(reply) => self.handle_resource_reply(
                instance,
                correlation_id,
                resource_requests::ResourceReply::Chunk(reply),
            ),
            Message::StateSubscribe(request) => self.handle_state_subscribe(instance, request),
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
                        self.resource_session_detached(declined_handle);
                        self.action_session_detached(declined_handle);
                        self.cancel_scans_for_session(&plugin_id, declined_handle);
                        self.state_session_detaching(&detached, Some(&plugin_id));
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
                        self.resource_session_detached(declined_handle);
                        self.action_session_detached(declined_handle);
                        self.cancel_scans_for_session(&plugin_id, declined_handle);
                        self.state_session_detaching(&detached, Some(&plugin_id));
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
            message @ (Message::PublishField(_) | Message::PublishBatch(_)) => {
                let Ok(payload) = message.encode_payload() else {
                    return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
                };
                self.handle_publish_raw(instance, message.opcode(), &payload)
            }
            other => {
                // State/resource/tree/stream/action ops: later milestones, logged only.
                tracing::debug!(
                    ?instance,
                    opcode = ?other.opcode(),
                    "plugin sent an unhandled host message (later milestone)"
                );
                vosu_status::VOSU_OK
            }
        }
    }

    fn handle_read_plan_sync(
        &self,
        instance: PluginInstanceId,
        payload: &[u8],
        flags: u32,
    ) -> Option<vosu_status> {
        Some(self.handle_read_plan_submit_raw(instance, payload, flags))
    }

    fn handle_publish(
        &self,
        instance: PluginInstanceId,
        opcode: vosu_plugin_api::vosu_opcode,
        payload: &[u8],
    ) -> Option<vosu_status> {
        Some(self.handle_publish_raw(instance, opcode, payload))
    }

    fn instance_lost(&self, instance: PluginInstanceId, cause: super::execution::LossCause) {
        self.process_instance_lost(instance, cause);
    }

    /// The shared background pool's `instance` sub-queue: bounded by the
    /// channel's 64 status/`SYNC` requests (§7.6) beside the plan and scan
    /// admission caps, and served round-robin with every other instance.
    fn submit_broker_work(
        &self,
        instance: PluginInstanceId,
        job: super::host_services::BrokerJob,
    ) -> Result<(), super::host_services::BrokerJob> {
        self.background_pool.submit_boxed(instance.0, job);
        Ok(())
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
    /// is already `InstanceSlot::Live`; queues the fire for replay
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

impl SettingsSchemaSource for PluginHost {
    /// `BRIDGE_B2_PLUGIN_ID`'s (`vosu.osu`'s) own `LoadedPlugin::
    /// settings_schema` (P1's real `settings.schema.json`), resolved from
    /// whichever slot the instance is currently registered under —
    /// `Creating` as well as `Live` (review finding: `create` can block
    /// for a while, and a B2 change arriving in that window must still
    /// resolve the real schema, not `None`, or the replayed delivery
    /// once `Live` sends an empty, spec-violating effective settings
    /// map). `None` only once no instance of `plugin_id` is registered
    /// at all (never created, already `Destroying`, or already gone).
    fn schema_for(&self, plugin_id: &str) -> Option<SettingsSchema> {
        self.instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .values()
            .find_map(|slot| match slot {
                InstanceSlot::Live(entry) if entry.plugin_id == plugin_id => {
                    entry.settings_schema.clone()
                }
                InstanceSlot::Creating {
                    plugin_id: creating_id,
                    settings_schema,
                    ..
                } if creating_id == plugin_id => settings_schema.clone(),
                _ => None,
            })
    }
}

mod diagnostics;
pub use diagnostics::DiagnosticSession;
mod action_delivery;
mod actions;
mod api;
mod capabilities;
mod instance_loss;
mod instances;
mod lifecycle;
mod observer;
pub use observer::DiscoveryObserver;
pub(super) mod read_plans;
pub mod resource_requests;
pub mod resources;
mod scans;
mod state;
mod state_delivery;
mod supervision;
mod surface;
pub(crate) use surface::SurfaceRequester;

#[cfg(test)]
mod tests;
