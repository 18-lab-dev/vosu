//! A state-bus publisher testkit plugin (`docs/v2/plugin-abi.md` §6.6,
//! §6.7, D4; `docs/v2/implementation-plan.md` M2 KT): a `concurrent_dispatch`
//! synthetic game that identifies every session it is attached to, then
//! lets a test drive `VOSU_OP_PUBLISH_FIELD`/`VOSU_OP_PUBLISH_BATCH`
//! submissions against it concurrently from N test-controlled threads
//! ([`spawn_writers`]) — `PluginHost`'s publish-commit path exercised
//! through one real plugin instance instead of calling
//! `vosu_core::state` directly, including the ABI's own concurrent-writer
//! case (plan E7: "no snapshot mixes two batches").
//!
//! Per-instance test state travels through plugin-id-keyed registries set
//! up before `PluginHost::create_instance`, the same pattern `session.rs`
//! already uses. Unlike every other registry here, this plugin's manifest
//! text itself ([`register_manifest`]/[`manifest`]) also travels through
//! such a registry: a session/read-plan/scan testkit plugin always
//! matches the same fixed `[match]` shape (`session_manifest` in
//! `vosu-core`'s own test helpers), but a publisher's `[[provides]]`
//! capability declarations (D4, K0) vary per test case, so there is no
//! single fixed manifest text to hand every test instead.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;

use vosu_plugin_api::{
    ConcurrentPlugin, DemandChanged, HostServices, Init, Message, PluginError, PublishBatchWriter,
    SessionIdentify, VtableHostServices, vosu_opcode, vosu_status,
};

/// One message [`PublisherPlugin`] observed, in arrival order.
#[derive(Debug, Clone, PartialEq)]
pub enum PublisherEvent {
    /// `VOSU_OP_SESSION_ATTACHED` arrived; this plugin always identifies
    /// the session (never declines), so the handle is usable right away.
    Attached {
        handle: u64,
    },
    Detached {
        handle: u64,
    },
    /// `VOSU_OP_DEMAND_CHANGED` for this plugin's own publication.
    Demand(DemandChanged),
}

fn events_registry() -> &'static Mutex<HashMap<String, SyncSender<PublisherEvent>>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, SyncSender<PublisherEvent>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

fn manifest_registry() -> &'static Mutex<HashMap<String, String>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, String>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

fn host_services_registry() -> &'static Mutex<HashMap<String, VtableHostServices>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, VtableHostServices>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

/// Inserts `value` for `plugin_id`, panicking rather than silently
/// overwriting an entry a still-live instance (or an earlier, not yet
/// consumed, registration) may depend on: every testkit registry here is
/// keyed by plugin id, and two registrations sharing one id are a test
/// bug (reuse a fresh synthetic plugin id per test) rather than an
/// intentional replacement.
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

/// Registers this plugin's manifest text for `plugin_id` (module doc
/// comment): call before loading it as a `BundledPlugin`/registering it
/// with the host, since `[[provides]]` varies per test case. Panics on a
/// duplicate live `plugin_id`.
pub fn register_manifest(plugin_id: impl Into<String>, manifest_text: impl Into<String>) {
    insert_unique(manifest_registry(), &plugin_id.into(), manifest_text.into());
}

/// Returns the manifest text [`register_manifest`] stored for
/// `plugin_id`. Panics if none is registered.
pub fn manifest(plugin_id: &str) -> String {
    manifest_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(plugin_id)
        .cloned()
        .unwrap_or_else(|| panic!("no manifest text registered for {plugin_id:?}"))
}

/// The [`VtableHostServices`] [`PublisherPlugin::create`] stored for
/// `plugin_id`, so [`spawn_writers`] can publish from threads this plugin
/// does not itself own. Panics if `plugin_id`'s instance has not finished
/// `create` yet.
pub fn host_services(plugin_id: &str) -> VtableHostServices {
    *host_services_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(plugin_id)
        .unwrap_or_else(|| panic!("no host services registered for {plugin_id:?} yet"))
}

/// Registers a fresh, bounded event channel for `plugin_id`, returning the
/// receiver: call before `PluginHost::create_instance` so the channel
/// exists before the plugin's own `create` can look it up. Panics on a
/// duplicate live `plugin_id`.
pub fn register(plugin_id: impl Into<String>) -> Receiver<PublisherEvent> {
    let plugin_id = plugin_id.into();
    let (tx, rx) = sync_channel(64);
    insert_unique(events_registry(), &plugin_id, tx);
    rx
}

/// One writer thread's per-batch fill callback ([`spawn_writers`]).
pub type Writer = Box<dyn FnMut(&mut PublishBatchWriter) + Send>;

/// Spawns one thread per entry of `writers`, each publishing exactly one
/// `VOSU_OP_PUBLISH_BATCH` for `handle` on `plugin_id`'s instance: builds
/// a fresh [`PublishBatchWriter`], lets the writer closure fill it, then
/// sends the finished batch through [`HostServices::send_flat`] — the
/// allocation-free wire path D4/S2 describe, run concurrently from
/// threads this plugin's own dispatch thread never touches (plan E7).
/// Returns each thread's `JoinHandle`, carrying the `send_flat` status, so
/// a test can join every writer and assert on all of them.
pub fn spawn_writers(
    plugin_id: impl Into<String>,
    handle: u64,
    writers: Vec<Writer>,
) -> Vec<JoinHandle<vosu_status>> {
    let plugin_id = plugin_id.into();
    writers
        .into_iter()
        .map(|mut writer| {
            let host = host_services(&plugin_id);
            std::thread::spawn(move || {
                let mut batch = PublishBatchWriter::new();
                batch.begin(handle);
                writer(&mut batch);
                let mut buf = Vec::new();
                match batch.finish_into(&mut buf) {
                    Ok(()) => host.send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &buf),
                    Err(_) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
                }
            })
        })
        .collect()
}

pub struct PublisherPlugin {
    host: VtableHostServices,
    events: SyncSender<PublisherEvent>,
}

impl ConcurrentPlugin for PublisherPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let events = events_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&init.plugin_id)
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "no publisher event channel registered for {:?}",
                    init.plugin_id
                )
            });
        host_services_registry()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(init.plugin_id.clone(), host);
        Ok(Self { host, events })
    }

    fn on_message(&self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => {
                let identify = SessionIdentify {
                    handle: attached.handle,
                    game: "testkit".to_owned(),
                    variant: "publisher".to_owned(),
                    name: "Publisher Testkit".to_owned(),
                    version: "0.1.0".to_owned(),
                    focus_priority: false,
                };
                let _ = self.host.send(&Message::SessionIdentify(identify), 0);
                let _ = self.events.try_send(PublisherEvent::Attached {
                    handle: attached.handle,
                });
            }
            Message::SessionDetached(detached) => {
                let _ = self.events.try_send(PublisherEvent::Detached {
                    handle: detached.handle,
                });
            }
            Message::DemandChanged(demand) => {
                let _ = self.events.try_send(PublisherEvent::Demand(demand));
            }
            _ => {}
        }
        Ok(())
    }
}

vosu_plugin_api::export_plugin!(bundled concurrent PublisherPlugin);
