//! A state-bus subscriber testkit plugin (`docs/v2/plugin-abi.md` §6.12,
//! §6.2 MG; `docs/v2/implementation-plan.md` M2 KT): lets a test drive
//! `VOSU_OP_STATE_SUBSCRIBE` submissions against it from a background
//! thread this plugin owns (plugin -> host, so only plugin code can emit
//! it), and records every `VOSU_OP_STATE_UPDATE`/`VOSU_OP_SESSION_*`/
//! `VOSU_OP_FOCUS_CHANGED` it receives — `PluginHost`'s subscription and
//! fan-out path exercised through one real plugin instance instead of
//! calling `vosu_core::state` directly. This plugin never calls
//! `VOSU_OP_SESSION_IDENTIFY`: MG's non-owning case (a `vosu.sessions`
//! subscriber fanned events for sessions it never owns) must never see
//! this plugin call `identify_session`/`decline_session` on a handle it
//! was only told about, so it stays a pure observer of whatever
//! `SESSION_ATTACHED`/`SESSION_DETACHED`/`FOCUS_CHANGED`/`STATE_UPDATE`
//! messages reach it.
//!
//! `STATE_UPDATE`'s `more: true` parts split one logical update across
//! several messages (§6.12): [`SubscriberPlugin`] accumulates `values` per
//! session and reports one combined [`SubscriberEvent::Update`] only once
//! the matching `more: false` part completes it.
//!
//! Per-instance test state travels through plugin-id-keyed registries set
//! up before `PluginHost::create_instance`, the same pattern
//! `read_plan.rs`/`scan.rs` already use.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use vosu_plugin_api::{
    FocusChanged, HostServices, Init, Message, Plugin, PluginError, Selector, SessionAttached,
    SessionDetached, StateEntry, StateUpdate, VtableHostServices, vosu_status,
};

/// One message [`SubscriberPlugin`] observed, in arrival order.
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriberEvent {
    Attached(SessionAttached),
    Detached(SessionDetached),
    Focus(FocusChanged),
    /// One reassembled `STATE_UPDATE`: every part's `values`, in arrival
    /// order, once the matching `more: false` part completed it.
    Update {
        session: u64,
        values: Vec<StateEntry>,
    },
    /// `send`'s own return value for a `VOSU_OP_STATE_SUBSCRIBE`.
    SubscribeStatus(vosu_status),
}

/// One command a test sends this plugin's background subscriber thread,
/// so `VOSU_OP_STATE_SUBSCRIBE` never runs on this plugin's own serialized
/// dispatch thread. Mirrors `StateSubscribe`'s own replace-wholesale
/// semantics: each command replaces this plugin's previous selection for
/// `session`.
pub struct SubscribeCommand {
    pub session: Option<u64>,
    pub selectors: Vec<Selector>,
}

fn events_registry() -> &'static Mutex<HashMap<String, SyncSender<SubscriberEvent>>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, SyncSender<SubscriberEvent>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

type CommandsRegistry = Mutex<HashMap<String, Receiver<SubscribeCommand>>>;

fn commands_registry() -> &'static CommandsRegistry {
    static REGISTRY: std::sync::LazyLock<CommandsRegistry> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

/// Inserts `value` for `plugin_id`, panicking rather than silently
/// overwriting an entry a still-live instance may depend on: every
/// testkit registry here is keyed by plugin id, and two registrations
/// sharing one id are a test bug (reuse a fresh synthetic plugin id per
/// test) rather than an intentional replacement.
fn insert_unique<V>(registry: &Mutex<HashMap<String, V>>, plugin_id: &str, value: V) {
    match registry
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .entry(plugin_id.to_owned())
    {
        Entry::Occupied(_) => {
            panic!("testkit plugin id {plugin_id:?} is already registered live")
        }
        Entry::Vacant(slot) => {
            slot.insert(value);
        }
    }
}

/// Registers a fresh event channel and command channel for `plugin_id`,
/// returning the event receiver and the command sender: call before
/// `PluginHost::create_instance` so both exist before the plugin's own
/// `create` can look them up. Panics on a duplicate live `plugin_id`.
pub fn register(
    plugin_id: impl Into<String>,
) -> (Receiver<SubscriberEvent>, SyncSender<SubscribeCommand>) {
    let plugin_id = plugin_id.into();
    let (events_tx, events_rx) = sync_channel(64);
    let (commands_tx, commands_rx) = sync_channel(64);
    insert_unique(events_registry(), &plugin_id, events_tx);
    insert_unique(commands_registry(), &plugin_id, commands_rx);
    (events_rx, commands_tx)
}

pub struct SubscriberPlugin {
    plugin_id: String,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    events: SyncSender<SubscriberEvent>,
    /// Parts accumulated for an in-progress `more: true` `STATE_UPDATE`
    /// delivery, by session, until the matching `more: false` part
    /// completes it (module doc comment).
    pending: HashMap<u64, Vec<StateEntry>>,
}

impl Plugin for SubscriberPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let events = events_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&init.plugin_id)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "no subscriber event channel registered for {:?}",
                    init.plugin_id
                )
            });
        let commands = commands_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&init.plugin_id)
            .unwrap_or_else(|| {
                panic!(
                    "no subscriber command channel registered for {:?}",
                    init.plugin_id
                )
            });

        let reader_host = host;
        let reader_events = events.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_stopped = stopped.clone();
        let thread = std::thread::spawn(move || {
            while !thread_stopped.load(Ordering::Acquire) {
                match commands.recv_timeout(Duration::from_millis(10)) {
                    Ok(command) => {
                        if thread_stopped.load(Ordering::Acquire) {
                            break;
                        }
                        let status =
                            reader_host.subscribe_state(command.session, command.selectors);
                        let _ = reader_events.try_send(SubscriberEvent::SubscribeStatus(status));
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        Ok(Self {
            plugin_id: init.plugin_id,
            stopped,
            thread: Some(thread),
            events,
            pending: HashMap::new(),
        })
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => {
                let _ = self.events.try_send(SubscriberEvent::Attached(attached));
            }
            Message::SessionDetached(detached) => {
                self.pending.remove(&detached.handle);
                let _ = self.events.try_send(SubscriberEvent::Detached(detached));
            }
            Message::FocusChanged(focus) => {
                let _ = self.events.try_send(SubscriberEvent::Focus(focus));
            }
            Message::StateUpdate(StateUpdate {
                session,
                mut values,
                more,
            }) => {
                self.pending.entry(session).or_default().append(&mut values);
                if !more {
                    let values = self.pending.remove(&session).unwrap_or_default();
                    let _ = self
                        .events
                        .try_send(SubscriberEvent::Update { session, values });
                }
            }
            _ => {}
        }
        Ok(())
    }
}

// Event channels are bounded and deliberately lossy when full: tests must
// drain them or fail on their bounded receive, never stall host dispatch.
// Shutdown stops command consumption and joins before the borrowed host dies.
impl Drop for SubscriberPlugin {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        events_registry()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.plugin_id);
    }
}

vosu_plugin_api::export_plugin!(bundled SubscriberPlugin);
