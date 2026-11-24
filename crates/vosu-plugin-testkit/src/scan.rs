//! A signature-scan testkit plugin (`docs/v2/plugin-abi.md` §6.4):
//! identifies every session it is attached to, then lets a test drive
//! `VOSU_OP_SCAN_REQUEST` submissions against it from a background thread
//! this plugin owns (the ABI opcode is plugin -> host, so only plugin code
//! can emit it) — `PluginHost`'s scan routing exercised through one real
//! plugin instance instead of calling `scan.rs`/`host/scans.rs` directly.
//!
//! Per-instance test state travels through plugin-id-keyed registries set
//! up before `PluginHost::create_instance`, the same pattern
//! `read_plan.rs`/`session.rs` already use.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use vosu_plugin_api::{
    HostServices, Init, Message, Plugin, PluginError, ScanRequest, ScanResult, SessionIdentify,
    VtableHostServices, vosu_status,
};

/// One outcome [`ScanPlugin`] observed or produced, in order.
#[derive(Debug)]
pub enum ScanEvent {
    /// `VOSU_OP_SESSION_ATTACHED` arrived; this plugin always identifies
    /// the session (never declines), so the handle is usable right away.
    Attached { handle: u64 },
    /// `send`'s own return value for a `VOSU_OP_SCAN_REQUEST`.
    SubmitStatus(vosu_status),
    /// The `VOSU_OP_SCAN_RESULT` a submission produced, with the
    /// `correlation_id` it arrived under (§6.4: "a scan is correlated
    /// like every other CBOR request/reply pair").
    Result {
        correlation_id: u64,
        result: ScanResult,
    },
}

/// One command a test sends this plugin's background submitter thread, so
/// `VOSU_OP_SCAN_REQUEST` never runs on this plugin's own serialized
/// dispatch thread.
pub struct ScanCommand {
    pub correlation_id: u64,
    pub session: u64,
    pub module: Option<String>,
    pub pattern: String,
    pub non_zero_mask: bool,
    pub all_matches: bool,
}

fn events_registry() -> &'static Mutex<HashMap<String, SyncSender<ScanEvent>>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, SyncSender<ScanEvent>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

type CommandsRegistry = Mutex<HashMap<String, Receiver<ScanCommand>>>;

fn commands_registry() -> &'static CommandsRegistry {
    static REGISTRY: std::sync::LazyLock<CommandsRegistry> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

/// Registers a fresh event channel and command channel for `plugin_id`,
/// returning the event receiver and the command sender: call before
/// `PluginHost::create_instance` so both exist before the plugin's own
/// `create` can look them up.
pub fn register(plugin_id: impl Into<String>) -> (Receiver<ScanEvent>, SyncSender<ScanCommand>) {
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
        .insert(plugin_id, commands_rx);
    (events_rx, commands_tx)
}

pub struct ScanPlugin {
    host: VtableHostServices,
    events: SyncSender<ScanEvent>,
}

impl Plugin for ScanPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        // Consumed (`remove`): frees the plugin id for a later, different
        // test to reuse without tripping `register`'s duplicate check.
        let events = events_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&init.plugin_id)
            .unwrap_or_else(|| panic!("no scan event channel registered for {:?}", init.plugin_id));
        let commands = commands_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&init.plugin_id)
            .unwrap_or_else(|| {
                panic!(
                    "no scan command channel registered for {:?}",
                    init.plugin_id
                )
            });

        let reader_host = host;
        let reader_events = events.clone();
        std::thread::spawn(move || {
            while let Ok(command) = commands.recv() {
                let status = reader_host.scan(
                    command.correlation_id,
                    ScanRequest {
                        session: command.session,
                        module: command.module,
                        pattern: command.pattern,
                        non_zero_mask: command.non_zero_mask,
                        all_matches: command.all_matches,
                    },
                );
                let _ = reader_events.send(ScanEvent::SubmitStatus(status));
            }
        });

        Ok(Self { host, events })
    }

    fn on_message(&mut self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => {
                let _ = self.events.send(ScanEvent::Attached {
                    handle: attached.handle,
                });
                let identify = SessionIdentify {
                    handle: attached.handle,
                    game: "testkit".to_owned(),
                    variant: "scan".to_owned(),
                    name: "Scan Testkit".to_owned(),
                    version: "0.1.0".to_owned(),
                    focus_priority: false,
                };
                let _ = self.host.send(&Message::SessionIdentify(identify), 0);
            }
            Message::ScanResult(result) => {
                let _ = self.events.send(ScanEvent::Result {
                    correlation_id,
                    result,
                });
            }
            _ => {}
        }
        Ok(())
    }
}

vosu_plugin_api::export_plugin!(bundled ScanPlugin);
