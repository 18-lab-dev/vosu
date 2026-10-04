//! Transport-independent WebSocket routing, publication, and connection lifecycle.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

mod upgrade;

pub use upgrade::{
    MAX_FRAME_BYTES, MAX_MESSAGE_BYTES, UpgradeCallbackTracker, UpgradeDecision, decide_upgrade,
    upgrade_response,
};

pub const MESSAGE_TOO_BIG: u16 = 1009;
pub const GOING_AWAY: u16 = 1001;
pub const PROTOCOL_ERROR: u16 = 1002;

const CLIENT_OPEN: u8 = 0;
const CLIENT_PENDING: u8 = 1;
const CLIENT_CLOSED: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Endpoint {
    V1,
    StreamCompanion,
    V2,
    V2Precise,
    V2Subscribe,
    Commands,
}

impl Endpoint {
    /// Classifies an upgrade target. Call this before `on_upgrade`; `None` must be
    /// returned as the ordinary HTTP 404 response rather than upgraded.
    pub fn classify(path: &str) -> Option<Self> {
        match path {
            "/ws" => Some(Self::V1),
            "/tokens" => Some(Self::StreamCompanion),
            "/websocket/v2" => Some(Self::V2),
            "/websocket/v2/precise" => Some(Self::V2Precise),
            "/websocket/v2/subscribe" => Some(Self::V2Subscribe),
            "/websocket/commands" => Some(Self::Commands),
            _ => None,
        }
    }

    pub fn schema(self) -> Option<Schema> {
        match self {
            Self::V1 => Some(Schema::V1),
            Self::StreamCompanion => Some(Schema::StreamCompanion),
            Self::V2 => Some(Schema::V2),
            Self::V2Precise => Some(Schema::V2Precise),
            Self::V2Subscribe => None,
            Self::Commands => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Schema {
    V1,
    StreamCompanion,
    V2,
    V2Precise,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Filter {
    Field(String),
    Nested { field: String, keys: Vec<Filter> },
}

/// One immutable state generation. Every projection in one broadcast pass is
/// derived from the same captured object.
pub trait StateSnapshot: Send + Sync + 'static {
    fn generation(&self) -> u64;
    fn encode_unfiltered(&self) -> Result<Bytes, String>;
    fn encode_filtered(&self, filters: &[Filter]) -> Result<Bytes, String>;
}

/// The state boundary used by all four schema hubs. Implementations normally
/// capture the latest immutable `CombinedSnapshot`.
pub trait StateProvider: Send + Sync + 'static {
    fn poll_rate(&self, schema: Schema) -> Duration;
    fn subscribe(&self, schema: Schema) -> watch::Receiver<u64>;
    fn capture(&self, schema: Schema) -> Option<Arc<dyn StateSnapshot>>;
}

pub type ClientId = u64;

#[derive(Clone, Debug, PartialEq)]
pub enum Outbound {
    /// A state snapshot frame. The transport must call `write_completed`
    /// after this frame's send future completes.
    Text(Bytes),
    /// A command response frame. This is control traffic and must not alter
    /// state-frame backpressure bookkeeping.
    Reply(Bytes),
    Binary(Bytes),
    Pong(Bytes),
    Close {
        code: u16,
        reason: &'static str,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Inbound {
    Text { data: Bytes, finished: bool },
    Binary { data: Bytes, finished: bool },
    Continuation { data: Bytes, finished: bool },
    Ping(Bytes),
    Pong(Bytes),
    Close,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FragmentKind {
    Text,
    Binary,
}

#[derive(Default)]
struct Fragments {
    kind: Option<FragmentKind>,
    bytes: Vec<u8>,
}

struct Client {
    publishes_state: bool,
    filters: ArcSwap<Vec<Filter>>,
    data: mpsc::Sender<Outbound>,
    control: mpsc::Sender<Outbound>,
    state: AtomicU8,
    last_generation: AtomicU64,
    cleaned: AtomicBool,
    cancellation: CancellationToken,
    terminal: Arc<Mutex<Option<Outbound>>>,
}

impl Client {
    fn terminate(&self, terminal: Outbound) {
        if !self.cleaned.swap(true, Ordering::AcqRel) {
            self.state.store(CLIENT_CLOSED, Ordering::Release);
            *lock_unpoisoned(&self.terminal) = Some(terminal);
            self.cancellation.cancel();
        }
    }
    fn terminate_for_shutdown(&self, code: u16) {
        self.cleaned.store(true, Ordering::Release);
        self.state.store(CLIENT_CLOSED, Ordering::Release);
        *lock_unpoisoned(&self.terminal) = Some(Outbound::Close {
            code,
            reason: "Server shutting down",
        });
        self.cancellation.cancel();
    }

    fn send_data(&self, data: Bytes, generation: u64, only_if_new: bool) -> bool {
        if only_if_new && self.last_generation.load(Ordering::Acquire) == generation {
            return false;
        }
        if self
            .state
            .compare_exchange(
                CLIENT_OPEN,
                CLIENT_PENDING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        if self.state.load(Ordering::Acquire) == CLIENT_CLOSED
            || self.data.try_send(Outbound::Text(data)).is_err()
        {
            let _ = self.state.compare_exchange(
                CLIENT_PENDING,
                CLIENT_OPEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return false;
        }
        self.last_generation.store(generation, Ordering::Release);
        true
    }

    async fn send_control(&self, frame: Outbound) -> bool {
        if self.state.load(Ordering::Acquire) == CLIENT_CLOSED {
            return false;
        }
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => false,
            result = self.control.send(frame) => result.is_ok(),
        }
    }
}

pub struct OutboundReceiver {
    data: mpsc::Receiver<Outbound>,
    control: mpsc::Receiver<Outbound>,
    cancellation: CancellationToken,
    terminal: Arc<Mutex<Option<Outbound>>>,
    terminated: bool,
}

impl OutboundReceiver {
    pub async fn recv(&mut self) -> Option<Outbound> {
        if self.terminated {
            return None;
        }
        let cancellation = self.cancellation.clone();
        let frame = tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            frame = self.control.recv() => frame,
            frame = self.data.recv() => frame,
        };
        if self.cancellation.is_cancelled() {
            self.terminated = true;
            return lock_unpoisoned(&self.terminal).take();
        }
        frame
    }

    pub fn try_recv(&mut self) -> Option<Outbound> {
        if self.terminated {
            return None;
        }
        if self.cancellation.is_cancelled() {
            self.terminated = true;
            return lock_unpoisoned(&self.terminal).take();
        }
        self.control
            .try_recv()
            .ok()
            .or_else(|| self.data.try_recv().ok())
    }
}

struct HubInner {
    schema: Schema,
    provider: Arc<dyn StateProvider>,
    clients: ArcSwap<HashMap<ClientId, Arc<Client>>>,
    client_writes: Mutex<()>,
    accepting: AtomicBool,
    next_id: AtomicU64,
    wake: watch::Sender<u64>,
    writable: watch::Sender<u64>,
    shutdown: CancellationToken,
}

#[derive(Clone)]
pub struct WebSocketHub(Arc<HubInner>);

pub struct Connection {
    id: ClientId,
    hub: WebSocketHub,
    client: Arc<Client>,
    fragments: Fragments,
}

/// Cloneable control-plane access that does not require the receive-side
/// `Connection` mutex.
#[derive(Clone)]
pub struct ConnectionControl {
    id: ClientId,
    client: Arc<Client>,
    hub: WebSocketHub,
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl WebSocketHub {
    pub fn new(schema: Schema, provider: Arc<dyn StateProvider>) -> Self {
        let (wake, _) = watch::channel(0);
        let (writable, _) = watch::channel(0);
        Self(Arc::new(HubInner {
            schema,
            provider,
            clients: ArcSwap::from_pointee(HashMap::new()),
            client_writes: Mutex::new(()),
            accepting: AtomicBool::new(true),
            next_id: AtomicU64::new(1),
            wake,
            writable,
            shutdown: CancellationToken::new(),
        }))
    }

    /// Registers a transport writer. The channel is deliberately one frame
    /// deep: data publication never creates a per-client backlog.
    pub fn connect(&self) -> (Connection, OutboundReceiver) {
        self.connect_inner(true)
    }

    pub fn connect_commands(&self) -> (Connection, OutboundReceiver) {
        self.connect_inner(false)
    }

    fn connect_inner(&self, publishes_state: bool) -> (Connection, OutboundReceiver) {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let (data, data_receiver) = mpsc::channel(1);
        // Allow ordinary command bursts without disconnecting an otherwise
        // healthy recipient before its writer gets scheduled.
        let (control, control_receiver) = mpsc::channel(16);
        let cancellation = CancellationToken::new();
        let terminal = Arc::new(Mutex::new(None));
        let client = Arc::new(Client {
            publishes_state,
            filters: ArcSwap::from_pointee(Vec::new()),
            data,
            control,
            state: AtomicU8::new(CLIENT_OPEN),
            last_generation: AtomicU64::new(u64::MAX),
            cleaned: AtomicBool::new(false),
            cancellation: cancellation.clone(),
            terminal: Arc::clone(&terminal),
        });
        {
            let _write = lock_unpoisoned(&self.0.client_writes);
            if self.0.accepting.load(Ordering::Acquire) {
                let mut clients = HashMap::clone(&self.0.clients.load());
                clients.insert(id, Arc::clone(&client));
                self.0.clients.store(Arc::new(clients));
            } else {
                client.terminate_for_shutdown(1001);
            }
        }
        self.wake();
        (
            Connection {
                id,
                hub: self.clone(),
                client,
                fragments: Fragments::default(),
            },
            OutboundReceiver {
                data: data_receiver,
                control: control_receiver,
                cancellation,
                terminal,
                terminated: false,
            },
        )
    }

    pub fn client_count(&self) -> usize {
        self.0.clients.load().len()
    }

    pub fn wake(&self) {
        let next = self.0.wake.borrow().wrapping_add(1);
        self.0.wake.send_replace(next);
    }
    fn wake_writable(&self) {
        let Some(snapshot) = self.0.provider.capture(self.0.schema) else {
            return;
        };
        self.0.writable.send_replace(snapshot.generation());
    }

    /// Seals the hub and closes every registered transport. The registry is
    /// cleared while holding the same lock used by `connect`, so no client can
    /// race into the hub after the seal point.
    pub async fn close_all(&self, code: u16) {
        {
            let _write = lock_unpoisoned(&self.0.client_writes);
            self.0.accepting.store(false, Ordering::Release);
            let clients = self.0.clients.load_full();
            self.0.clients.store(Arc::new(HashMap::new()));
            for client in clients.values() {
                client.terminate_for_shutdown(code);
            }
        }
        self.wake();
    }

    pub fn shutdown(&self) {
        self.0.shutdown.cancel();
    }

    /// Runs the schema publication loop. Spawn exactly one per schema.
    pub async fn run(&self) {
        let mut state_events = self.0.provider.subscribe(self.0.schema);
        let mut local_wake = self.0.wake.subscribe();
        let mut writable = self.0.writable.subscribe();
        let mut cached: Option<(u64, Bytes)> = None;
        let mut next_send_at = tokio::time::Instant::now();
        let mut only_new_generation = false;
        let mut generation_ceiling = None;
        let mut force_delivery = false;

        loop {
            if self.0.shutdown.is_cancelled() {
                break;
            }

            let clients = self.0.clients.load_full();
            let has_state_clients = clients.values().any(|client| client.publishes_state);
            let has_writable = clients.values().any(|client| {
                client.publishes_state && client.state.load(Ordering::Acquire) == CLIENT_OPEN
            });

            if !has_state_clients {
                next_send_at = tokio::time::Instant::now();
                tokio::select! {
                    _ = self.0.shutdown.cancelled() => break,
                    changed = state_events.changed() => {
                        if changed.is_err() { break; }
                    },
                    changed = local_wake.changed() => {
                        if changed.is_err() { break; }
                    },
                    changed = writable.changed() => {
                        if changed.is_err() { break; }
                    },
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {},
                }
                continue;
            }

            if !force_delivery && tokio::time::Instant::now() < next_send_at {
                let wake_delivery = tokio::select! {
                    _ = self.0.shutdown.cancelled() => break,
                    changed = state_events.changed() => {
                        if changed.is_err() { break; }
                        None
                    },
                    changed = local_wake.changed() => {
                        if changed.is_err() { break; }
                        Some((true, None, true))
                    },
                    changed = writable.changed() => {
                        if changed.is_err() { break; }
                        Some((true, Some(*writable.borrow()), true))
                    },
                    _ = tokio::time::sleep_until(next_send_at) => Some((false, None, false)),
                };
                if let Some((only_if_new, ceiling, forced)) = wake_delivery {
                    only_new_generation = only_if_new;
                    generation_ceiling = ceiling;
                    force_delivery = forced;
                }
                continue;
            }

            let cadence_due = !force_delivery;
            if has_writable && let Some(snapshot) = self.0.provider.capture(self.0.schema) {
                let generation = snapshot.generation();
                if generation_ceiling.is_some_and(|ceiling| generation > ceiling) {
                    only_new_generation = false;
                    generation_ceiling = None;
                    force_delivery = false;
                    continue;
                }
                if cached.as_ref().is_none_or(|(seen, _)| *seen != generation) {
                    match snapshot.encode_unfiltered() {
                        Ok(bytes) => cached = Some((generation, bytes)),
                        Err(_) => cached = None,
                    }
                }

                for client in clients.values() {
                    if !client.publishes_state
                        || client.state.load(Ordering::Acquire) != CLIENT_OPEN
                    {
                        continue;
                    }
                    let filters = client.filters.load();
                    let payload = if filters.is_empty() {
                        cached.as_ref().map(|(_, bytes)| bytes.clone())
                    } else {
                        snapshot.encode_filtered(&filters).ok()
                    };
                    if let Some(payload) = payload {
                        client.send_data(payload, generation, only_new_generation);
                    }
                }
            }

            generation_ceiling = None;
            force_delivery = false;
            only_new_generation = false;
            if cadence_due {
                next_send_at =
                    tokio::time::Instant::now() + self.0.provider.poll_rate(self.0.schema);
            }
        }
    }
}

impl Connection {
    pub fn id(&self) -> ClientId {
        self.id
    }

    pub fn control_handle(&self) -> ConnectionControl {
        ConnectionControl {
            id: self.id,
            client: Arc::clone(&self.client),
            hub: self.hub.clone(),
        }
    }

    pub fn set_filters(&self, filters: Vec<Filter>) {
        self.client.filters.store(Arc::new(filters));
        self.client
            .last_generation
            .store(u64::MAX, Ordering::Release);
        self.hub.wake();
    }

    /// The transport writer must call this after its state send future completes.
    pub fn write_completed(&self) {
        if self
            .client
            .state
            .compare_exchange(
                CLIENT_PENDING,
                CLIENT_OPEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.hub.wake_writable();
        }
    }

    pub async fn send_text(&self, bytes: Bytes) -> bool {
        let sent = self.client.send_control(Outbound::Reply(bytes)).await;
        if !sent {
            self.cleanup();
        }
        sent
    }

    /// Enqueues a control reply without waiting; callers on shared workers
    /// must not let one full client queue stall unrelated clients.
    pub fn try_send_text(&self, bytes: Bytes) -> bool {
        self.client.state.load(Ordering::Acquire) != CLIENT_CLOSED
            && self.client.control.try_send(Outbound::Reply(bytes)).is_ok()
    }

    /// Consumes a transport frame. Complete text/binary messages are returned
    /// to the command processor; protocol/control frames are handled here.
    pub async fn receive(&mut self, frame: Inbound) -> Option<Bytes> {
        match frame {
            Inbound::Ping(data) => {
                if !self.client.send_control(Outbound::Pong(data)).await {
                    self.cleanup();
                }
                None
            }
            Inbound::Pong(_) => None,
            Inbound::Close | Inbound::Error => {
                self.cleanup();
                None
            }
            Inbound::Text { data, finished } => {
                self.start_fragment(FragmentKind::Text, data, finished)
            }
            Inbound::Binary { data, finished } => {
                self.start_fragment(FragmentKind::Binary, data, finished)
            }
            Inbound::Continuation { data, finished } => self.continue_fragment(data, finished),
        }
    }

    pub fn cleanup(&self) {
        self.finish(None);
    }

    fn finish(&self, terminal: Option<Outbound>) {
        if !self.client.cleaned.load(Ordering::Acquire) {
            match terminal {
                Some(terminal) => self.client.terminate(terminal),
                None => {
                    if !self.client.cleaned.swap(true, Ordering::AcqRel) {
                        self.client.state.store(CLIENT_CLOSED, Ordering::Release);
                        self.client.cancellation.cancel();
                    }
                }
            }
            let _write = lock_unpoisoned(&self.hub.0.client_writes);
            let mut clients = HashMap::clone(&self.hub.0.clients.load());
            clients.remove(&self.id);
            self.hub.0.clients.store(Arc::new(clients));
            self.hub.wake();
        }
    }

    fn start_fragment(&mut self, kind: FragmentKind, data: Bytes, finished: bool) -> Option<Bytes> {
        if self.fragments.kind.is_some() {
            self.close_with(PROTOCOL_ERROR, "Protocol error");
            return None;
        }
        if data.len() > MAX_FRAME_BYTES {
            self.close_with(MESSAGE_TOO_BIG, "Message too big");
            return None;
        }
        if finished {
            return Some(data);
        }
        self.fragments.kind = Some(kind);
        self.fragments.bytes.extend_from_slice(&data);
        None
    }

    fn continue_fragment(&mut self, data: Bytes, finished: bool) -> Option<Bytes> {
        if self.fragments.kind.is_none() {
            self.close_with(PROTOCOL_ERROR, "Protocol error");
            return None;
        }
        if data.len() > MAX_FRAME_BYTES
            || self.fragments.bytes.len().saturating_add(data.len()) > MAX_MESSAGE_BYTES
        {
            self.close_with(MESSAGE_TOO_BIG, "Message too big");
            return None;
        }
        self.fragments.bytes.extend_from_slice(&data);
        if finished {
            self.fragments.kind = None;
            return Some(Bytes::from(std::mem::take(&mut self.fragments.bytes)));
        }
        None
    }

    fn close_with(&mut self, code: u16, reason: &'static str) {
        self.fragments = Fragments::default();
        self.finish(Some(Outbound::Close { code, reason }));
    }
}

impl ConnectionControl {
    pub fn set_filters(&self, filters: Vec<Filter>) {
        self.client.filters.store(Arc::new(filters));
        self.client
            .last_generation
            .store(u64::MAX, Ordering::Release);
        self.hub.wake();
    }

    /// Enqueues a control reply without waiting; a full recipient queue is
    /// reported to the caller so that recipient can be cleaned up.
    pub fn try_send_text(&self, bytes: Bytes) -> bool {
        self.client.state.load(Ordering::Acquire) != CLIENT_CLOSED
            && self.client.control.try_send(Outbound::Reply(bytes)).is_ok()
    }

    pub fn cleanup(&self) {
        if !self.client.cleaned.swap(true, Ordering::AcqRel) {
            self.client.state.store(CLIENT_CLOSED, Ordering::Release);
            self.client.cancellation.cancel();
        }
        let _write = lock_unpoisoned(&self.hub.0.client_writes);
        let mut clients = HashMap::clone(&self.hub.0.clients.load());
        clients.remove(&self.id);
        self.hub.0.clients.store(Arc::new(clients));
        self.hub.wake();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests;
