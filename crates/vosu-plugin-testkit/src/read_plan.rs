//! A read-plan testkit plugin (`docs/v2/plugin-abi.md` §6.3, §4's
//! `VOSU_READ_PLAN_SYNC` exception, D5): identifies every session it is
//! attached to, then lets a test drive async and `VOSU_READ_PLAN_SYNC`
//! submissions against it from a background thread this plugin owns (the
//! ABI opcode is plugin -> host, so only plugin code can emit it) —
//! `PluginHost`'s read-plan routing exercised through one real plugin
//! instance instead of calling `read_plan.rs`/`host/read_plans.rs`
//! directly.
//!
//! Per-instance test state travels through plugin-id-keyed registries set
//! up before `PluginHost::create_instance`, the same pattern `session.rs`
//! already uses.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use vosu_plugin_api::{
    HostServices, Init, Message, PlanBase, Plugin, PluginError, ReadPlanCancel, ReadPlanResult,
    SessionIdentify, Stage, VtableHostServices, vosu_status,
};
use vosu_plugin_internal::{PlanMemory, PlanMemoryError};

/// One outcome [`ReadPlanPlugin`] observed or produced, in order.
#[derive(Debug)]
pub enum ReadPlanEvent {
    /// `VOSU_OP_SESSION_ATTACHED` arrived; this plugin always identifies
    /// the session (never declines), so the handle is usable right away.
    Attached { handle: u64 },
    /// The `VOSU_OP_READ_PLAN_RESULT` an asynchronous submission produced.
    AsyncResult(ReadPlanResult),
    /// `send`'s own return value for an asynchronous
    /// `VOSU_OP_READ_PLAN_SUBMIT` or `VOSU_OP_READ_PLAN_CANCEL`.
    SubmitStatus(vosu_status),
    /// [`PlanMemory::read`]'s own result for a `VOSU_READ_PLAN_SYNC`
    /// submission, run on this plugin's background reader thread (never
    /// its dispatch thread, D5's own requirement).
    SyncResult(Result<ReadPlanResult, PlanMemoryError>),
    /// `VOSU_OP_SESSION_DETACHED` arrived (F6: cancel-before-detach
    /// ordering tests read this against a preceding `AsyncResult`/
    /// `SyncResult` to prove the already-in-flight answer for a
    /// declined/detached handle is delivered strictly before this).
    Detached { handle: u64 },
}

/// One command a test sends this plugin's background reader thread, so a
/// `VOSU_READ_PLAN_SYNC` submission ([`Self::SubmitSync`]) never runs on
/// this plugin's own serialized dispatch thread.
pub enum ReadPlanCommand {
    SubmitAsync {
        plan: u64,
        handle: u64,
        root: PlanBase,
        stages: Vec<Stage>,
    },
    SubmitSync {
        plan: u64,
        handle: u64,
        root: PlanBase,
        stages: Vec<Stage>,
    },
    Cancel {
        plan: u64,
    },
}

/// Per-plugin-id test behavior: whether `on_message`'s own
/// `SESSION_ATTACHED` handling should, before identifying, attempt a
/// `VOSU_READ_PLAN_SYNC` submission directly from the dispatch thread
/// itself — the one case D5 refuses
/// (`VOSU_ERR_INVALID_ARGUMENT`, `PluginHost`'s own
/// `DispatchThreadScope`). `None` (the default) skips this.
#[derive(Debug, Clone, Default)]
pub struct ReadPlanPluginConfig {
    pub sync_on_attach: Option<(u64, PlanBase, Vec<Stage>)>,
}

fn events_registry() -> &'static Mutex<HashMap<String, SyncSender<ReadPlanEvent>>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, SyncSender<ReadPlanEvent>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

type CommandsRegistry = Mutex<HashMap<String, Receiver<ReadPlanCommand>>>;

fn commands_registry() -> &'static CommandsRegistry {
    static REGISTRY: std::sync::LazyLock<CommandsRegistry> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

fn host_services_registry() -> &'static Mutex<HashMap<String, VtableHostServices>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, VtableHostServices>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

/// The [`VtableHostServices`] [`ReadPlanPlugin::create`] stored for
/// `plugin_id`, so a test can call [`PlanMemory::read_flat`] directly from
/// its own thread instead of through this plugin's background reader
/// thread — e.g. to control allocation tracking (a `thread_local`
/// mechanism) on that one thread itself rather than on a thread the
/// plugin owns and the test cannot otherwise instrument. Panics if
/// `plugin_id`'s instance has not finished `create` yet.
pub fn host_services(plugin_id: &str) -> VtableHostServices {
    *host_services_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(plugin_id)
        .unwrap_or_else(|| panic!("no host services registered for {plugin_id:?} yet"))
}
fn config_registry() -> &'static Mutex<HashMap<String, ReadPlanPluginConfig>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, ReadPlanPluginConfig>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

/// Registers a fresh event channel and command channel for `plugin_id`
/// with the default config (no dispatch-thread SYNC attempt), returning
/// the event receiver and the command sender: call before
/// `PluginHost::create_instance` so both exist before the plugin's own
/// `create` can look them up.
pub fn register(
    plugin_id: impl Into<String>,
) -> (Receiver<ReadPlanEvent>, SyncSender<ReadPlanCommand>) {
    register_with_config(plugin_id, ReadPlanPluginConfig::default())
}

/// Same as [`register`], with explicit [`ReadPlanPluginConfig`].
pub fn register_with_config(
    plugin_id: impl Into<String>,
    config: ReadPlanPluginConfig,
) -> (Receiver<ReadPlanEvent>, SyncSender<ReadPlanCommand>) {
    let plugin_id = plugin_id.into();
    let (events_tx, events_rx) = sync_channel(64);
    let (commands_tx, commands_rx) = sync_channel(64);
    let mut events = events_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    assert!(
        !events.contains_key(&plugin_id),
        "testkit plugin id {plugin_id:?} is already registered: ids must be unique per test \
         (a process-wide registry keyed by plugin id silently overwrites the earlier test's \
         event channel when two tests share one)"
    );
    events.insert(plugin_id.clone(), events_tx);
    drop(events);
    commands_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(plugin_id.clone(), commands_rx);
    config_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(plugin_id, config);
    (events_rx, commands_tx)
}

pub struct ReadPlanPlugin {
    host: VtableHostServices,
    events: SyncSender<ReadPlanEvent>,
    config: ReadPlanPluginConfig,
}

impl Plugin for ReadPlanPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        // Consumed (`remove`), same reasoning as `session.rs::create`: frees
        // the plugin id in these two registries for a later, different test
        // to reuse without tripping `register_with_config`'s duplicate check.
        let events = events_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&init.plugin_id)
            .unwrap_or_else(|| {
                panic!(
                    "no read-plan event channel registered for {:?}",
                    init.plugin_id
                )
            });
        let commands = commands_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&init.plugin_id)
            .unwrap_or_else(|| {
                panic!(
                    "no read-plan command channel registered for {:?}",
                    init.plugin_id
                )
            });
        let config = config_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&init.plugin_id)
            .unwrap_or_default();

        host_services_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(init.plugin_id.clone(), host);

        let reader_host = host;
        let reader_events = events.clone();
        std::thread::spawn(move || {
            loop {
                let command = match commands.recv_timeout(std::time::Duration::from_secs(10)) {
                    Ok(command) => command,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        panic!("testkit command worker received no command within 10s")
                    }
                };
                match command {
                    ReadPlanCommand::SubmitAsync {
                        plan,
                        handle,
                        root,
                        stages,
                    } => {
                        let status = reader_host.submit_read_plan(plan, handle, root, stages);
                        let _ = reader_events.send(ReadPlanEvent::SubmitStatus(status));
                    }
                    ReadPlanCommand::SubmitSync {
                        plan,
                        handle,
                        root,
                        stages,
                    } => {
                        let result =
                            PlanMemory::new().read(&reader_host, plan, handle, root, stages);
                        let _ = reader_events.send(ReadPlanEvent::SyncResult(result));
                    }
                    ReadPlanCommand::Cancel { plan } => {
                        let status =
                            reader_host.send(&Message::ReadPlanCancel(ReadPlanCancel { plan }), 0);
                        let _ = reader_events.send(ReadPlanEvent::SubmitStatus(status));
                    }
                }
            }
        });

        Ok(Self {
            host,
            events,
            config,
        })
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => {
                let _ = self.events.send(ReadPlanEvent::Attached {
                    handle: attached.handle,
                });
                if let Some((plan, root, stages)) = self.config.sync_on_attach.clone() {
                    // Attempted directly from `on_message`'s own call —
                    // this plugin's dispatch thread — the one case D5
                    // refuses (`VOSU_ERR_INVALID_ARGUMENT`).
                    let status = self.host.send(
                        &Message::ReadPlanSubmit(vosu_plugin_api::ReadPlanSubmit {
                            plan,
                            handle: attached.handle,
                            root,
                            stages,
                            sync: true,
                        }),
                        0,
                    );
                    let _ = self.events.send(ReadPlanEvent::SubmitStatus(status));
                }
                let identify = SessionIdentify {
                    handle: attached.handle,
                    game: "testkit".to_owned(),
                    variant: "read_plan".to_owned(),
                    name: "Read Plan Testkit".to_owned(),
                    version: "0.1.0".to_owned(),
                    focus_priority: false,
                };
                let _ = self.host.send(&Message::SessionIdentify(identify), 0);
            }
            Message::ReadPlanResult(result) => {
                let _ = self.events.send(ReadPlanEvent::AsyncResult(result));
            }
            Message::SessionDetached(detached) => {
                let _ = self.events.send(ReadPlanEvent::Detached {
                    handle: detached.handle,
                });
            }
            _ => {}
        }
        Ok(())
    }
}

vosu_plugin_internal::export_plugin!(bundled ReadPlanPlugin);
