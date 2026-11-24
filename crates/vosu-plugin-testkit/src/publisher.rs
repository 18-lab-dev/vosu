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
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex};
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

fn host_services_registry() -> &'static Mutex<HashMap<String, LiveHost>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, LiveHost>>> =
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

/// A host handle that refuses calls after plugin destruction. Outstanding
/// calls drain before destroy returns; no lock is held while calling host code.
#[derive(Clone)]
pub struct LiveHost(Arc<HostLifetime>);

struct HostLifetime {
    state: Mutex<(Option<VtableHostServices>, usize)>,
    drained: Condvar,
}

struct HostCall<'a>(&'a HostLifetime);

impl Drop for HostCall<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        state.1 -= 1;
        self.0.drained.notify_all();
    }
}

impl LiveHost {
    fn new(host: VtableHostServices) -> Self {
        Self(Arc::new(HostLifetime {
            state: Mutex::new((Some(host), 0)),
            drained: Condvar::new(),
        }))
    }

    fn call(&self, send: impl FnOnce(VtableHostServices) -> vosu_status) -> vosu_status {
        let host = {
            let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
            let Some(host) = state.0 else {
                return vosu_status::VOSU_ERR_TARGET_GONE;
            };
            state.1 += 1;
            host
        };
        let _call = HostCall(&self.0);
        send(host)
    }

    fn close(&self) {
        let mut state = self.0.state.lock().unwrap_or_else(|p| p.into_inner());
        state.0 = None;
        while state.1 != 0 {
            state = self
                .0
                .drained
                .wait(state)
                .unwrap_or_else(|p| p.into_inner());
        }
    }
}

impl HostServices for LiveHost {
    fn send(&self, message: &Message, correlation_id: u64) -> vosu_status {
        self.call(|host| host.send(message, correlation_id))
    }

    fn send_flat(
        &self,
        opcode: vosu_opcode,
        flags: u32,
        correlation_id: u64,
        payload: &[u8],
    ) -> vosu_status {
        self.call(|host| host.send_flat(opcode, flags, correlation_id, payload))
    }
}

/// Returns a lifetime-checked host handle; panics if no live instance exists.
pub fn host_services(plugin_id: &str) -> LiveHost {
    host_services_registry()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(plugin_id)
        .cloned()
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

/// One writer thread's fill callback. Return `true` to publish this batch
/// and run again, or `false` to stop without publishing another batch.
pub type Writer = Box<dyn FnMut(&mut PublishBatchWriter) -> bool + Send>;

/// Runs each writer on its own thread, reusing its batch and wire buffer.
/// Stops on the first rejected publish or when the callback returns false.
/// Join handles return the final status; retained host handles are invalidated
/// before plugin destruction returns.
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
                let mut buf = Vec::new();
                loop {
                    batch.begin(handle);
                    if !writer(&mut batch) {
                        break vosu_status::VOSU_OK;
                    }
                    if batch.finish_into(&mut buf).is_err() {
                        break vosu_status::VOSU_ERR_INVALID_ARGUMENT;
                    }
                    let status = host.send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &buf);
                    if status != vosu_status::VOSU_OK {
                        break status;
                    }
                }
            })
        })
        .collect()
}

pub struct PublisherPlugin {
    plugin_id: String,
    host: LiveHost,
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
        let host = LiveHost::new(host);
        insert_unique(host_services_registry(), &init.plugin_id, host.clone());
        Ok(Self {
            plugin_id: init.plugin_id,
            host,
            events,
        })
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

impl Drop for PublisherPlugin {
    fn drop(&mut self) {
        self.host.close();
        host_services_registry()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.plugin_id);
        events_registry()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.plugin_id);
        manifest_registry()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.plugin_id);
    }
}

vosu_plugin_api::export_plugin!(bundled concurrent PublisherPlugin);
