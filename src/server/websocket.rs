//! Transport-independent WebSocket routing, publication, and connection lifecycle.

use std::{
    any::Any,
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, mpsc, watch};
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

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Filter {
    Field(String),
    Nested { field: String, keys: Vec<Filter> },
}

/// Upper bounds for sharing filtered payloads between clients within one
/// broadcast pass. Clients beyond either bound are still served; their
/// projections are encoded individually instead of being retained for reuse.
const MAX_SHARED_PROJECTIONS: usize = 64;
const MAX_SHARED_PROJECTION_BYTES: usize = 1 << 20;

/// A client's requested filter list with a hash precomputed when the filters
/// are set, so broadcast passes compare plans without rehashing them.
struct FilterPlan {
    filters: Vec<Filter>,
    hash: u64,
}

impl FilterPlan {
    fn new(filters: Vec<Filter>) -> Self {
        let mut hasher = DefaultHasher::new();
        filters.hash(&mut hasher);
        Self {
            hash: hasher.finish(),
            filters,
        }
    }
}

/// Filtered payloads encoded from one captured snapshot. It lives only for a
/// single broadcast pass, so every reused payload belongs to that generation.
#[derive(Default)]
struct SharedProjections {
    /// Each entry's plan hash, kept apart so a miss scans contiguous words
    /// rather than every plan behind its `Arc`.
    hashes: Vec<u64>,
    entries: Vec<(Arc<FilterPlan>, Option<Bytes>)>,
    bytes: usize,
}

impl SharedProjections {
    fn encode(&mut self, snapshot: &dyn StateSnapshot, plan: &Arc<FilterPlan>) -> Option<Bytes> {
        if let Some((_, payload)) = self
            .hashes
            .iter()
            .zip(&self.entries)
            .find(|(hash, (candidate, _))| **hash == plan.hash && candidate.filters == plan.filters)
            .map(|(_, entry)| entry)
        {
            return payload.clone();
        }
        let payload = snapshot.encode_filtered(&plan.filters).ok();
        let size = payload.as_ref().map_or(0, Bytes::len);
        if self.entries.len() < MAX_SHARED_PROJECTIONS
            && self.bytes.saturating_add(size) <= MAX_SHARED_PROJECTION_BYTES
        {
            self.bytes += size;
            self.hashes.push(plan.hash);
            self.entries.push((Arc::clone(plan), payload.clone()));
        }
        payload
    }
}

thread_local! {
    /// Slots of the broadcast pass running on this thread, if any.
    static PASS_LOCAL: RefCell<Option<Vec<Box<dyn Any>>>> = const { RefCell::new(None) };
}

/// Opens pass-local storage for one synchronous broadcast pass. Dropping it
/// drops everything stored during the pass, so nothing derived from the
/// captured snapshot outlives the pass, even when an encoder panics.
pub(crate) struct BroadcastPass {
    previous: Option<Vec<Box<dyn Any>>>,
}

impl BroadcastPass {
    pub(crate) fn begin() -> Self {
        Self {
            previous: PASS_LOCAL.with(|slots| slots.replace(Some(Vec::new()))),
        }
    }
}

impl Drop for BroadcastPass {
    fn drop(&mut self) {
        let ended = PASS_LOCAL.with(|slots| slots.replace(self.previous.take()));
        // Released after the borrow ends: a stored value's destructor may
        // itself touch pass-local storage.
        drop(ended);
    }
}

/// Runs `f` with the current broadcast pass's slot of type `T`, which starts
/// as `None`. Every projection in a pass is encoded from the same captured
/// snapshot, so a `StateSnapshot` may keep work derived from that snapshot
/// here for the next client; it must still check that a stored value belongs
/// to the snapshot it is encoding. Returns `None` outside a pass, where no
/// such scope exists, and on reentrant use, which must not alias the slot.
pub(crate) fn with_pass_local<T: 'static, R>(f: impl FnOnce(&mut Option<T>) -> R) -> Option<R> {
    PASS_LOCAL.with(|slots| {
        let mut slots = slots.try_borrow_mut().ok()?;
        let slots = slots.as_mut()?;
        let position = match slots.iter().position(|slot| slot.is::<Option<T>>()) {
            Some(position) => position,
            None => {
                slots.push(Box::new(None::<T>));
                slots.len() - 1
            }
        };
        let slot = slots[position]
            .downcast_mut::<Option<T>>()
            .expect("slot type was checked");
        Some(f(slot))
    })
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
    filters: ArcSwap<FilterPlan>,
    /// Whether another client had a filter plan with the same hash at the
    /// last plan or membership change; only such clients go through a pass's
    /// shared projections. A mark made stale by a concurrent change costs
    /// only performance: shared lookups compare whole plans, and an unmarked
    /// client gets its own encoding.
    shares_plan: AtomicBool,
    data: Arc<DataSlot>,
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

    fn set_filters(&self, filters: Vec<Filter>) {
        self.filters.store(Arc::new(FilterPlan::new(filters)));
        self.last_generation.store(u64::MAX, Ordering::Release);
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
        if self.state.load(Ordering::Acquire) == CLIENT_CLOSED || !self.data.try_send(data) {
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

/// The one state frame queued for a client. Delivery reserves the client
/// (`PENDING`) until `write_completed`, so it never needs a second slot; a
/// mutex slot plus `Notify` keeps that bounded contract without a channel's
/// semaphore and block list. The same structure serves the v2 subscription
/// endpoint.
struct DataSlot {
    state: Mutex<DataState>,
    ready: Notify,
}

#[derive(Default)]
struct DataState {
    frame: Option<Bytes>,
    /// The receiver is gone; frames are refused like sends to a closed channel.
    closed: bool,
}

impl DataSlot {
    fn new() -> Self {
        Self {
            state: Mutex::new(DataState::default()),
            ready: Notify::new(),
        }
    }

    /// Queues `frame` unless the slot is occupied or the receiver is gone.
    fn try_send(&self, frame: Bytes) -> bool {
        let mut state = lock_unpoisoned(&self.state);
        if state.closed || state.frame.is_some() {
            return false;
        }
        state.frame = Some(frame);
        drop(state);
        // Stores a permit when the receiver is not waiting yet, so the wakeup
        // cannot be lost.
        self.ready.notify_one();
        true
    }

    fn try_recv(&self) -> Option<Outbound> {
        lock_unpoisoned(&self.state).frame.take().map(Outbound::Text)
    }
}

pub struct OutboundReceiver {
    data: Arc<DataSlot>,
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
        // A frame that is already queued is taken in the select's priority order
        // without registering cancellation and channel wakers. A closed channel
        // falls through so the select reports it exactly as before. Cancellation
        // is checked once afterwards: a frame taken from a cancelled receiver is
        // discarded for the terminal frame, which is what the select returns.
        let ready = match self.control.try_recv() {
            Ok(frame) => Some(frame),
            Err(mpsc::error::TryRecvError::Empty) => self.data.try_recv(),
            Err(mpsc::error::TryRecvError::Disconnected) => None,
        };
        let frame = match ready {
            Some(frame) => Some(frame),
            // The client's senders close together, so a closed control channel
            // also ends data.
            None => loop {
                tokio::select! {
                    biased;
                    _ = self.cancellation.cancelled() => break None,
                    frame = self.control.recv() => break frame,
                    () = self.data.ready.notified() => {
                        // A permit can outlive the frame it announced.
                        if let Some(frame) = self.data.try_recv() {
                            break Some(frame);
                        }
                    }
                }
            },
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
            .or_else(|| self.data.try_recv())
    }
}

impl Drop for OutboundReceiver {
    fn drop(&mut self) {
        let mut state = lock_unpoisoned(&self.data.state);
        state.closed = true;
        state.frame = None;
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
    /// Set while a `writable` change is published but not yet consumed by
    /// `run`; see `wake_writable`.
    writable_pending: AtomicBool,
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

/// Marks each client in `clients` whose filter plan hash another filtered
/// client shares. Callers hold `client_writes`, so marks follow the latest
/// plan or membership change.
fn mark_shared_plans(clients: &HashMap<ClientId, Arc<Client>>) {
    let hash = |client: &Client| {
        let plan = client.filters.load();
        (client.publishes_state && !plan.filters.is_empty()).then_some(plan.hash)
    };
    let mut uses: HashMap<u64, usize> = HashMap::with_capacity(clients.len());
    for client in clients.values() {
        if let Some(hash) = hash(client) {
            *uses.entry(hash).or_default() += 1;
        }
    }
    for client in clients.values() {
        let shared = hash(client).is_some_and(|hash| uses.get(&hash).is_some_and(|&uses| uses > 1));
        client.shares_plan.store(shared, Ordering::Relaxed);
    }
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
            writable_pending: AtomicBool::new(false),
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
        let data = Arc::new(DataSlot::new());
        // Allow ordinary command bursts without disconnecting an otherwise
        // healthy recipient before its writer gets scheduled.
        let (control, control_receiver) = mpsc::channel(16);
        let cancellation = CancellationToken::new();
        let terminal = Arc::new(Mutex::new(None));
        let client = Arc::new(Client {
            publishes_state,
            filters: ArcSwap::from_pointee(FilterPlan::new(Vec::new())),
            shares_plan: AtomicBool::new(false),
            data: Arc::clone(&data),
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
                data,
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

    /// Refreshes every client's `shares_plan` mark after a plan change.
    fn mark_shared_plans(&self) {
        let _write = lock_unpoisoned(&self.0.client_writes);
        mark_shared_plans(&self.0.clients.load());
    }

    pub fn wake(&self) {
        let next = self.0.wake.borrow().wrapping_add(1);
        self.0.wake.send_replace(next);
    }
    fn wake_writable(&self) {
        let Some(snapshot) = self.0.provider.capture(self.0.schema) else {
            return;
        };
        let generation = snapshot.generation();
        // An unconsumed change already carrying this ceiling makes `run` read
        // every client's state after this caller's OPEN transition: the SeqCst
        // fences pair with the one after `run` clears the flag. Re-sending it
        // would only repeat the watch's eight-shard waiter notification.
        std::sync::atomic::fence(Ordering::SeqCst);
        if self.0.writable_pending.load(Ordering::SeqCst) && *self.0.writable.borrow() == generation
        {
            return;
        }
        self.0.writable_pending.store(true, Ordering::SeqCst);
        self.0.writable.send_replace(generation);
    }

    /// Marks the observed `writable` change as consumed. Client states are read
    /// only after this fence, so acknowledgements that skipped notifying are seen.
    fn writable_consumed(&self) {
        self.0.writable_pending.store(false, Ordering::SeqCst);
        std::sync::atomic::fence(Ordering::SeqCst);
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
        // Subscribing marks any earlier change as seen; nothing will consume it.
        self.writable_consumed();
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
                        self.writable_consumed();
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
                        self.writable_consumed();
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
                let mut projections = SharedProjections::default();
                let _pass = BroadcastPass::begin();
                for client in clients.values() {
                    if !client.publishes_state
                        || client.state.load(Ordering::Acquire) != CLIENT_OPEN
                    {
                        continue;
                    }
                    let plan = client.filters.load();
                    let payload = if plan.filters.is_empty() {
                        cached.as_ref().map(|(_, bytes)| bytes.clone())
                    } else if client.shares_plan.load(Ordering::Relaxed) {
                        projections.encode(snapshot.as_ref(), &plan)
                    } else {
                        // No other client uses this plan, so nothing would
                        // ever reuse a retained copy.
                        snapshot.encode_filtered(&plan.filters).ok()
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
        self.client.set_filters(filters);
        self.hub.mark_shared_plans();
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
            mark_shared_plans(&clients);
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
        self.client.set_filters(filters);
        self.hub.mark_shared_plans();
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
        mark_shared_plans(&clients);
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
