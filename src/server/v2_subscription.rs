//! Selected-field `/json/v2` WebSocket subscription protocol and delivery hub.

use std::{
    cell::OnceCell,
    collections::{HashMap, hash_map::Entry},
    hash::{Hash, Hasher},
    io,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use bytes::{BufMut, Bytes, BytesMut, buf::Writer};
use rustc_hash::FxHashMap;
use serde::Serialize;
use serde_json::Value;
use serde_json::ser::{Formatter, Serializer};
use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::api::ApiError;

const OPEN: u8 = 0;
const PENDING: u8 = 1;
const CLOSED: u8 = 2;
const UNCHECKED: u64 = u64::MAX;

pub const INVALID_REQUEST: u16 = 1;
pub const INVALID_POINTER: u16 = 2;
pub const SUBSCRIPTION_TIMEOUT: u16 = 3;
pub const SERIALIZATION_FAILED: u16 = 4;
pub const ALREADY_SUBSCRIBED: u16 = 5;

const INVALID_REQUEST_MESSAGE: &str = "invalid subscription request";
const INVALID_POINTER_MESSAGE: &str = "invalid JSON Pointer";
const SUBSCRIPTION_TIMEOUT_MESSAGE: &str = "subscription timed out";
const SERIALIZATION_FAILED_MESSAGE: &str = "state serialization failed";
const ALREADY_SUBSCRIBED_MESSAGE: &str = "subscription already established";

/// One immutable source snapshot captured at a live publication point.
pub trait V2SubscriptionSnapshot: Send + Sync {
    fn revision(&self) -> u64;
    fn document(&self) -> Result<Option<Value>, ApiError>;
}

/// Supplies live publication notifications and atomic snapshots.
pub trait V2SubscriptionProvider: Send + Sync {
    fn subscribe(&self) -> watch::Receiver<u64>;
    fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum V2SubscriptionOutbound {
    /// Initial or update frame; the transport must call `write_completed` after sending it.
    Text(Bytes),
    /// Application error frame. It is always followed by `Close`.
    Error(Bytes),
    Pong(Bytes),
    Close {
        code: u16,
    },
}

/// A client's outbound queues behind one lock and one wakeup, drained terminal first, then
/// control, then data. Every queue is bounded: terminal frames are queued once per client
/// (`cleaned`), and an occupied control or data slot refuses the frame. Delivery reserves the
/// data slot (`PENDING`) until `write_completed`, so data never needs a second slot.
struct Mailbox {
    state: Mutex<MailboxState>,
    ready: Notify,
}

#[derive(Default)]
struct MailboxState {
    /// Queued in order; the second entry is occupied only while the first is.
    terminal: [Option<V2SubscriptionOutbound>; 2],
    control: Option<V2SubscriptionOutbound>,
    data: Option<V2SubscriptionOutbound>,
    /// The receiver is gone; frames are refused like sends to a closed channel.
    receiver_closed: bool,
    /// The client is gone; the receiver ends once the queued frames are drained.
    senders_closed: bool,
    /// The receiver found nothing queued and may be waiting. Only the first frame or close
    /// after that notifies: the woken receiver checks every queue before it waits again.
    receiver_waiting: bool,
}

impl MailboxState {
    fn pop(&mut self) -> Option<V2SubscriptionOutbound> {
        if let Some(frame) = self.terminal[0].take() {
            self.terminal[0] = self.terminal[1].take();
            return Some(frame);
        }
        self.control.take().or_else(|| self.data.take())
    }
}

impl Mailbox {
    fn new() -> Self {
        Self {
            state: Mutex::new(MailboxState::default()),
            ready: Notify::new(),
        }
    }

    /// Queues `frame` into the slot `select` picks unless it is occupied or the receiver is
    /// gone.
    fn try_send(
        &self,
        select: impl FnOnce(&mut MailboxState) -> Option<&mut Option<V2SubscriptionOutbound>>,
        frame: V2SubscriptionOutbound,
    ) -> Result<(), V2SubscriptionOutbound> {
        let mut state = lock(&self.state);
        if state.receiver_closed {
            return Err(frame);
        }
        let Some(slot @ None) = select(&mut state) else {
            return Err(frame);
        };
        *slot = Some(frame);
        let wake = std::mem::take(&mut state.receiver_waiting);
        drop(state);
        if wake {
            // Stores a permit when the receiver has not started waiting yet, so the wakeup
            // cannot be lost.
            self.ready.notify_one();
        }
        Ok(())
    }

    fn try_send_data(&self, frame: V2SubscriptionOutbound) -> Result<(), V2SubscriptionOutbound> {
        self.try_send(|state| Some(&mut state.data), frame)
    }

    fn try_send_control(&self, frame: V2SubscriptionOutbound) -> bool {
        self.try_send(|state| Some(&mut state.control), frame)
            .is_ok()
    }

    fn try_send_terminal(&self, frame: V2SubscriptionOutbound) {
        let _ = self.try_send(
            |state| state.terminal.iter_mut().find(|slot| slot.is_none()),
            frame,
        );
    }
}

pub struct V2SubscriptionReceiver {
    mailbox: Arc<Mailbox>,
}

impl V2SubscriptionReceiver {
    pub async fn recv(&mut self) -> Option<V2SubscriptionOutbound> {
        loop {
            // A queued frame needs no wait and charges no coop budget. Skipping the budget
            // cannot starve the runtime: at most four frames can be queued.
            {
                let mut state = lock(&self.mailbox.state);
                if let Some(frame) = state.pop() {
                    return Some(frame);
                }
                if state.senders_closed {
                    return None;
                }
                state.receiver_waiting = true;
            }
            // A send between the unlock and this wait sees `receiver_waiting` and leaves a
            // permit, so it is not lost.
            self.mailbox.ready.notified().await;
        }
    }

    pub fn try_recv(&mut self) -> Option<V2SubscriptionOutbound> {
        lock(&self.mailbox.state).pop()
    }
}

impl Drop for V2SubscriptionReceiver {
    fn drop(&mut self) {
        let mut state = lock(&self.mailbox.state);
        state.receiver_closed = true;
        state.terminal = [None, None];
        state.control = None;
        state.data = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscriptionRequestError {
    InvalidRequest,
    InvalidPointer,
    AlreadySubscribed,
}

impl SubscriptionRequestError {
    pub const fn code(self) -> u16 {
        match self {
            Self::InvalidRequest => INVALID_REQUEST,
            Self::InvalidPointer => INVALID_POINTER,
            Self::AlreadySubscribed => ALREADY_SUBSCRIBED,
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::InvalidRequest => INVALID_REQUEST_MESSAGE,
            Self::InvalidPointer => INVALID_POINTER_MESSAGE,
            Self::AlreadySubscribed => ALREADY_SUBSCRIBED_MESSAGE,
        }
    }
}

#[derive(Clone, Debug, Eq)]
struct Plan {
    pointers: Vec<Vec<String>>,
}

impl PartialEq for Plan {
    fn eq(&self, other: &Self) -> bool {
        self.pointers == other.pointers
    }
}

impl Hash for Plan {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.pointers.hash(state);
    }
}

fn compile_pointers(pointers: Vec<String>) -> Result<Plan, SubscriptionRequestError> {
    if !(1..=256).contains(&pointers.len()) || pointers.iter().any(|pointer| pointer.len() > 512) {
        return Err(SubscriptionRequestError::InvalidRequest);
    }

    let mut compiled = Vec::with_capacity(pointers.len());
    for pointer in pointers {
        let tokens = decode_pointer(&pointer).ok_or(SubscriptionRequestError::InvalidPointer)?;
        if tokens.len() > 32 {
            return Err(SubscriptionRequestError::InvalidRequest);
        }
        if compiled.iter().any(|existing: &Vec<String>| {
            existing == &tokens || is_prefix(existing, &tokens) || is_prefix(&tokens, existing)
        }) {
            return Err(SubscriptionRequestError::InvalidPointer);
        }
        compiled.push(tokens);
    }
    Ok(Plan { pointers: compiled })
}

fn decode_pointer(pointer: &str) -> Option<Vec<String>> {
    if pointer.is_empty() {
        return Some(Vec::new());
    }
    let rest = pointer.strip_prefix('/')?;
    rest.split('/').map(decode_token).collect()
}

fn decode_token(token: &str) -> Option<String> {
    let mut decoded = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(ch) = chars.next() {
        if ch != '~' {
            decoded.push(ch);
            continue;
        }
        match chars.next()? {
            '0' => decoded.push('~'),
            '1' => decoded.push('/'),
            _ => return None,
        }
    }
    Some(decoded)
}

fn is_prefix(prefix: &[String], value: &[String]) -> bool {
    prefix.len() < value.len() && value.starts_with(prefix)
}

/// Objects up to this size are searched by comparing keys, which is cheaper than hashing the
/// token with the map's SipHash; keys are unique, so both find the same entry.
const LINEAR_KEY_SEARCH_MAX: usize = 8;

fn traverse<'a>(document: &'a Value, tokens: &[String]) -> Option<&'a Value> {
    let mut current = document;
    for token in tokens {
        current = match current {
            Value::Object(object) if object.len() <= LINEAR_KEY_SEARCH_MAX => object
                .iter()
                .find_map(|(key, value)| (key == token).then_some(value))?,
            Value::Object(object) => object.get(token)?,
            Value::Array(array) => array.get(parse_array_index(token)?)?,
            _ => return None,
        };
    }
    Some(current)
}

fn parse_array_index(token: &str) -> Option<usize> {
    if token == "0" {
        return Some(0);
    }
    if token.starts_with('0')
        || token.is_empty()
        || !token.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    token.parse().ok()
}

/// Values selected by one plan from one document, in pointer order.
type Extraction = Arc<[Option<Value>]>;

fn extract(plan: &Plan, document: Option<&Value>) -> Extraction {
    plan.pointers
        .iter()
        .map(|tokens| document.and_then(|value| traverse(value, tokens)).cloned())
        .collect()
}

/// One selected value, owned by an [`Extraction`] or borrowed from the dispatched document.
trait Selected {
    fn value(&self) -> Option<&Value>;
}

impl Selected for Option<Value> {
    fn value(&self) -> Option<&Value> {
        self.as_ref()
    }
}

impl Selected for Option<&Value> {
    fn value(&self) -> Option<&Value> {
        *self
    }
}

/// Collects into `changed`, in id order, the ids whose value differs from `baseline`.
fn changes(baseline: &[Option<Value>], current: &[impl Selected], changed: &mut Vec<usize>) {
    debug_assert_eq!(baseline.len(), current.len(), "one plan selects both");
    changed.clear();
    changed.extend(
        baseline
            .iter()
            .zip(current)
            .enumerate()
            .filter(|(_, (old, new))| old.as_ref() != new.value())
            .map(|(id, _)| id),
    );
}

/// Records `selected` as a client's baseline. A baseline only this subscription holds is not
/// published, so only the `changed` ids are updated, in place. A shared one is replaced, never
/// mutated: its other holders, and frame memos keyed on its address, rely on its contents.
fn store_baseline(
    baseline: &mut Option<Extraction>,
    selected: &[Option<&Value>],
    changed: &[usize],
) {
    if let Some(values) = baseline.as_mut().and_then(Arc::get_mut)
        && values.len() == selected.len()
    {
        for &id in changed {
            values[id] = selected[id].cloned();
        }
    } else {
        *baseline = Some(selected.iter().map(|value| value.cloned()).collect());
    }
}

/// Number spelling identical to `crate::api::JsFormatter`, which `crate::api` exports only to
/// tests; frames are serialized straight into a [`FrameArena`] rather than through
/// `encode_dto`'s per-frame growing `Vec`.
struct FrameFormatter;

impl Formatter for FrameFormatter {
    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        crate::calculation::write_js_json_number(writer, f64::from(value))
    }

    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + io::Write,
    {
        crate::calculation::write_js_json_number(writer, value)
    }
}

/// Initial capacity of each frame arena chunk.
const FRAME_ARENA_CAPACITY: usize = 4096;

/// Writes a frame's JSON skeleton directly, with the same punctuation and integer spelling as
/// serde_json's compact output for the equivalent tuple; only selected values go through serde.
struct FrameWriter<'a>(Writer<&'a mut BytesMut>);

impl FrameWriter<'_> {
    fn raw(&mut self, bytes: &'static [u8]) {
        self.0.get_mut().extend_from_slice(bytes);
    }

    fn integer(&mut self, value: u64) -> serde_json::Result<()> {
        FrameFormatter
            .write_u64(&mut self.0, value)
            .map_err(serde_json::Error::io)
    }

    /// Writes `[[id,value],...]` followed by `then`, merging the punctuation around the pairs
    /// into as few writes as possible: `then.0` follows written pairs, `then.1` an empty list.
    fn pairs<'v>(
        &mut self,
        pairs: impl Iterator<Item = (usize, &'v Value)>,
        then: (&'static [u8], &'static [u8]),
    ) -> serde_json::Result<()> {
        /// Ids below ten with their following comma, which spares the formatter.
        const SMALL_IDS: [&[u8]; 10] = [
            b"0,", b"1,", b"2,", b"3,", b"4,", b"5,", b"6,", b"7,", b"8,", b"9,",
        ];
        let mut written = false;
        for (id, value) in pairs {
            self.raw(if written { b",[" } else { b"[[" });
            written = true;
            if let Some(&text) = SMALL_IDS.get(id) {
                self.raw(text);
            } else {
                self.integer(id as u64)?;
                self.raw(b",");
            }
            value.serialize(&mut Serializer::with_formatter(&mut self.0, FrameFormatter))?;
            self.raw(b"]");
        }
        self.raw(if written { then.0 } else { then.1 });
        Ok(())
    }
}

/// Frames encoded during one dispatch, split off shared chunks so each frame costs no
/// allocation of its own and clones only bump a reference count. A queued frame pins at most
/// its chunk: `FRAME_ARENA_CAPACITY`, or the larger buffer an oversized frame needed.
#[derive(Default)]
struct FrameArena {
    buffer: Option<BytesMut>,
    /// The revision last written and its digits: a dispatch writes one revision into every
    /// frame, so it is formatted once.
    revision: Option<(u64, Vec<u8>)>,
}

impl FrameArena {
    /// Encodes `kind` (`[0,` or `[1,`), the revision, and a comma, followed by what `write`
    /// writes.
    fn encode(
        &mut self,
        kind: &'static [u8],
        revision: u64,
        write: impl FnOnce(&mut FrameWriter<'_>) -> serde_json::Result<()>,
    ) -> Result<Bytes, ApiError> {
        if self
            .revision
            .as_ref()
            .is_none_or(|(cached, _)| *cached != revision)
        {
            let mut digits = Vec::new();
            FrameFormatter
                .write_u64(&mut digits, revision)
                .map_err(|error| ApiError::serialization(error.to_string()))?;
            self.revision = Some((revision, digits));
        }
        let digits = self.revision.as_ref().map_or(&[][..], |(_, digits)| digits);
        let buffer = self
            .buffer
            .get_or_insert_with(|| BytesMut::with_capacity(FRAME_ARENA_CAPACITY));
        buffer.extend_from_slice(kind);
        buffer.extend_from_slice(digits);
        buffer.extend_from_slice(b",");
        let written = write(&mut FrameWriter((&mut *buffer).writer()));
        match written {
            Ok(()) => Ok(buffer.split().freeze()),
            Err(error) => {
                buffer.clear();
                Err(ApiError::serialization(error.to_string()))
            }
        }
    }

    /// `[0,revision,[[id,value],...]]` with every present value.
    fn initial(&mut self, revision: u64, state: &[impl Selected]) -> Result<Bytes, ApiError> {
        self.encode(b"[0,", revision, |frame| {
            frame.pairs(
                state
                    .iter()
                    .enumerate()
                    .filter_map(|(id, value)| value.value().map(|value| (id, value))),
                (b"]]", b"[]]"),
            )
        })
    }

    /// `[1,revision,[[id,value],...],[id,...]]`: of the `changed` ids, those now present with
    /// their values, then those now absent.
    fn update(
        &mut self,
        revision: u64,
        current: &[impl Selected],
        changed: &[usize],
    ) -> Result<Bytes, ApiError> {
        self.encode(b"[1,", revision, |frame| {
            frame.pairs(
                changed
                    .iter()
                    .filter_map(|&id| current[id].value().map(|value| (id, value))),
                (b"],[", b"[],["),
            )?;
            let removed = changed.iter().filter(|&&id| current[id].value().is_none());
            for (index, &id) in removed.enumerate() {
                if index > 0 {
                    frame.raw(b",");
                }
                frame.integer(id as u64)?;
            }
            frame.raw(b"]]");
            Ok(())
        })
    }
}

#[cfg(test)]
fn initial_frame(revision: u64, state: &[Option<Value>]) -> Result<Bytes, ApiError> {
    FrameArena::default().initial(revision, state)
}

#[cfg(test)]
fn update_frame(
    revision: u64,
    baseline: &[Option<Value>],
    current: &[Option<Value>],
) -> Result<Bytes, ApiError> {
    let mut changed = Vec::new();
    changes(baseline, current, &mut changed);
    FrameArena::default().update(revision, current, &changed)
}

fn error_frame(code: u16, message: &'static str) -> Bytes {
    crate::api::v2::encode_dto(&(2_u8, code, message))
        .unwrap_or_else(|_| Bytes::from_static(b"[2,4,\"state serialization failed\"]"))
}

/// An established selection and its delivery state; released together on close.
struct Subscription {
    plan: Arc<Plan>,
    baseline: Option<Arc<[Option<Value>]>>,
}

/// `Client::queued_at` before the client's first frame.
const NEVER_QUEUED: u64 = u64::MAX;

struct Client {
    subscription: Mutex<Option<Subscription>>,
    checked_revision: AtomicU64,
    /// When the dispatch that queued the latest data frame finished, in nanoseconds after
    /// `HubInner::epoch`, or `NEVER_QUEUED`. Debounce counts from here.
    queued_at: AtomicU64,
    state: AtomicU8,
    cleaned: AtomicBool,
    mailbox: Arc<Mailbox>,
}

impl Client {
    fn terminate(&self, code: u16, message: &'static str, close_code: u16) {
        let _subscription = lock(&self.subscription);
        if self.cleaned.swap(true, Ordering::AcqRel) {
            return;
        }
        self.state.store(CLOSED, Ordering::Release);
        self.mailbox
            .try_send_terminal(V2SubscriptionOutbound::Error(error_frame(code, message)));
        self.mailbox
            .try_send_terminal(V2SubscriptionOutbound::Close { code: close_code });
    }

    fn close(&self, code: u16) {
        let _subscription = lock(&self.subscription);
        if self.cleaned.swap(true, Ordering::AcqRel) {
            return;
        }
        self.state.store(CLOSED, Ordering::Release);
        self.mailbox
            .try_send_terminal(V2SubscriptionOutbound::Close { code });
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let wake = {
            let mut state = lock(&self.mailbox.state);
            state.senders_closed = true;
            std::mem::take(&mut state.receiver_waiting)
        };
        if wake {
            self.mailbox.ready.notify_one();
        }
    }
}

struct HubInner {
    provider: Arc<dyn V2SubscriptionProvider>,
    debounce: ArcSwap<Duration>,
    clients: ArcSwap<HashMap<u64, Arc<Client>>>,
    plans: Mutex<Vec<Weak<Plan>>>,
    document: Mutex<Option<(u64, Arc<Option<Value>>)>>,
    writes: Mutex<()>,
    accepting: AtomicBool,
    next_id: AtomicU64,
    wake: Notify,
    /// Origin of `Client::queued_at`.
    epoch: Instant,
    /// Set by the first `wake` since the run loop last armed `wake`; later wakes in the same
    /// window are already covered by that notification.
    wake_pending: AtomicBool,
    /// Whether any open client has an established selection.
    demand: watch::Sender<bool>,
    shutdown: CancellationToken,
    #[cfg(test)]
    extractions: AtomicU64,
    #[cfg(test)]
    encodings: AtomicU64,
    #[cfg(test)]
    failed_encoding_plan: Mutex<Option<Arc<Plan>>>,
    #[cfg(test)]
    pending_gate: Mutex<Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>>,
}

#[derive(Clone)]
pub struct V2SubscriptionHub(Arc<HubInner>);

pub struct V2SubscriptionConnection {
    id: u64,
    hub: V2SubscriptionHub,
    client: Arc<Client>,
}

impl V2SubscriptionHub {
    pub fn new(provider: Arc<dyn V2SubscriptionProvider>, debounce: Duration) -> Self {
        Self(Arc::new(HubInner {
            provider,
            debounce: ArcSwap::from_pointee(debounce),
            clients: ArcSwap::from_pointee(HashMap::new()),
            plans: Mutex::new(Vec::new()),
            document: Mutex::new(None),
            writes: Mutex::new(()),
            accepting: AtomicBool::new(true),
            next_id: AtomicU64::new(0),
            wake: Notify::new(),
            epoch: Instant::now(),
            wake_pending: AtomicBool::new(false),
            demand: watch::Sender::new(false),
            shutdown: CancellationToken::new(),
            #[cfg(test)]
            extractions: AtomicU64::new(0),
            #[cfg(test)]
            encodings: AtomicU64::new(0),
            #[cfg(test)]
            failed_encoding_plan: Mutex::new(None),
            #[cfg(test)]
            pending_gate: Mutex::new(None),
        }))
    }

    pub fn connect(&self) -> (V2SubscriptionConnection, V2SubscriptionReceiver) {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let mailbox = Arc::new(Mailbox::new());
        let client = Arc::new(Client {
            subscription: Mutex::new(None),
            checked_revision: AtomicU64::new(UNCHECKED),
            queued_at: AtomicU64::new(NEVER_QUEUED),
            state: AtomicU8::new(OPEN),
            cleaned: AtomicBool::new(false),
            mailbox: Arc::clone(&mailbox),
        });
        {
            let _write = lock(&self.0.writes);
            if self.0.accepting.load(Ordering::Acquire) {
                let mut clients = HashMap::clone(&self.0.clients.load());
                clients.insert(id, Arc::clone(&client));
                self.0.clients.store(Arc::new(clients));
            } else {
                client.close(1001);
            }
        }
        self.wake();
        (
            V2SubscriptionConnection {
                id,
                hub: self.clone(),
                client,
            },
            V2SubscriptionReceiver { mailbox },
        )
    }

    /// Whether any client has an established selection; state workers sample
    /// at the subscription interval while it is true.
    pub fn demand(&self) -> watch::Receiver<bool> {
        self.0.demand.subscribe()
    }

    pub fn set_debounce(&self, debounce: Duration) {
        self.0.debounce.store(Arc::new(debounce));
        self.wake();
    }

    pub fn close_all(&self, code: u16) {
        self.0.accepting.store(false, Ordering::Release);
        let clients = {
            let _write = lock(&self.0.writes);
            self.0.clients.swap(Arc::new(HashMap::new()))
        };
        for client in clients.values() {
            client.close(code);
            Self::release_client(client);
        }
        self.prune_plans();
        *lock(&self.0.document) = None;
        self.wake();
    }

    pub fn shutdown(&self) {
        self.0.accepting.store(false, Ordering::Release);
        self.0.shutdown.cancel();
        self.wake();
    }

    pub fn client_count(&self) -> usize {
        self.0.clients.load().len()
    }

    fn wake(&self) {
        if !self.0.wake_pending.swap(true, Ordering::SeqCst) {
            self.0.wake.notify_waiters();
        }
    }

    /// `wake` after `write_completed` reopened a client with a sequentially consistent
    /// exchange. A wake already pending covers it without another read-modify-write: the
    /// reopening exchange, the load below, the run loop's clearing exchange, and dispatch's
    /// state load are all `SeqCst`. If the run loop clears `wake_pending` after this load, the
    /// dispatch that follows observes the reopened state. If it cleared it before, the `true`
    /// read here was set by a later `wake` that found it clear and notified, which starts
    /// another clear and dispatch; that repeats until a clear follows this load.
    fn wake_reopened(&self) {
        if !self.0.wake_pending.load(Ordering::SeqCst) {
            self.wake();
        }
    }

    fn remove(&self, id: u64) {
        let removed = {
            let _write = lock(&self.0.writes);
            let mut clients = HashMap::clone(&self.0.clients.load());
            let removed = clients.remove(&id);
            self.0.clients.store(Arc::new(clients));
            removed
        };
        if let Some(client) = removed {
            Self::release_client(&client);
        }
        self.prune_plans();
        self.clear_document_if_unused();
        self.wake();
    }

    fn release_client(client: &Client) {
        *lock(&client.subscription) = None;
    }

    fn prune_plans(&self) {
        let mut plans = lock(&self.0.plans);
        plans.retain(|weak| weak.strong_count() != 0);
        plans.shrink_to_fit();
    }

    fn clear_document_if_unused(&self) {
        let mut document = lock(&self.0.document);
        let clients = self.0.clients.load_full();
        if clients
            .values()
            .all(|client| lock(&client.subscription).is_none())
        {
            *document = None;
        }
    }

    fn cleanup_retained_state(&self, initial_clients: &Arc<HashMap<u64, Arc<Client>>>) {
        let clients = self.0.clients.load_full();
        if Arc::ptr_eq(initial_clients, &clients) {
            return;
        }
        drop(clients);

        self.clear_document_if_unused();
        self.prune_plans();
    }

    fn intern_plan(&self, plan: Plan) -> Arc<Plan> {
        let mut plans = lock(&self.0.plans);
        let mut interned = None;
        plans.retain(|weak| {
            let Some(active) = weak.upgrade() else {
                return false;
            };
            if interned.is_none() && *active == plan {
                interned = Some(active);
            }
            true
        });
        interned.unwrap_or_else(|| {
            let plan = Arc::new(plan);
            plans.push(Arc::downgrade(&plan));
            plan
        })
    }

    pub async fn run(&self) {
        let mut publications = self.0.provider.subscribe();
        loop {
            if self.0.shutdown.is_cancelled() {
                break;
            }
            // Register before dispatch so writes completed during it cannot be lost. Clearing
            // `wake_pending` after registering makes the next `wake` notify this waiter, and
            // the acquire makes state changes published before earlier wakes visible to dispatch.
            // `SeqCst` for `wake_reopened`.
            let wake = self.0.wake.notified();
            self.0.wake_pending.swap(false, Ordering::SeqCst);
            let source_revision = *publications.borrow_and_update();
            let deadline = self.dispatch(source_revision);
            self.refresh_demand();
            tokio::select! {
                _ = self.0.shutdown.cancelled() => break,
                changed = publications.changed() => if changed.is_err() { break; },
                _ = tokio::task::coop::cooperative(wake) => {},
                _ = async {
                    if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await }
                    else { std::future::pending::<()>().await }
                } => {},
            }
        }
        self.0.demand.send_replace(false);
    }

    fn refresh_demand(&self) {
        let subscribed = self.0.clients.load().values().any(|client| {
            client.state.load(Ordering::Acquire) != CLOSED && lock(&client.subscription).is_some()
        });
        self.0.demand.send_if_modified(|current| {
            let changed = *current != subscribed;
            *current = subscribed;
            changed
        });
    }

    fn dispatch(&self, source_revision: u64) -> Option<Instant> {
        let clients = self.0.clients.load_full();
        let now = Instant::now();
        let debounce = **self.0.debounce.load();
        let mut next_deadline: Option<Instant> = None;
        // Captured at the first eligible client, so a pass that can deliver nothing builds no
        // document. A cell, so values borrowed from the document outlive later clients.
        let source: OnceCell<(u64, Arc<Option<Value>>)> = OnceCell::new();
        // Memo entries pin their plan, so its address cannot be reused by a plan interned
        // while this dispatch runs.
        let mut extracted: FxHashMap<usize, (Arc<Plan>, Extraction)> = FxHashMap::default();
        let mut encoded: FxHashMap<(usize, Option<usize>), Result<Option<Bytes>, ApiError>> =
            FxHashMap::default();
        let mut arena = FrameArena::default();
        // An unshared plan's values, borrowed from the document: they are compared and encoded
        // in place and cloned only into the baseline, and only where they changed.
        let mut selected: Vec<Option<&Value>> = Vec::new();
        // Ids of the values that differ from the baseline of the client being encoded.
        let mut changed: Vec<usize> = Vec::new();
        // Clients that received a frame; their queue instant is stamped once, after the loop.
        let mut queued: Vec<&Client> = Vec::new();
        for (id, client) in clients.iter() {
            // `SeqCst` for `wake_reopened`; a plain load on x86-64.
            if client.state.load(Ordering::SeqCst) != OPEN {
                continue;
            }
            let checked = client.checked_revision.load(Ordering::Acquire) == source_revision;
            if checked && source_revision != UNCHECKED {
                continue;
            }
            // One lock covers the eligibility check, reading the baseline, reserving the queue
            // slot, and recording the delivery; closing takes it too, so a released
            // subscription is never restored.
            let mut guard = lock(&client.subscription);
            let Some(subscription) = guard.as_ref() else {
                continue;
            };
            if checked && subscription.baseline.is_some() {
                continue;
            }
            let queued_at = client.queued_at.load(Ordering::Acquire);
            if !debounce.is_zero() && queued_at != NEVER_QUEUED {
                let Some(deadline) = self
                    .0
                    .epoch
                    .checked_add(Duration::from_nanos(queued_at))
                    .and_then(|queued| queued.checked_add(debounce))
                else {
                    continue;
                };
                if deadline > now {
                    next_deadline = Some(next_deadline.map_or(deadline, |old| old.min(deadline)));
                    continue;
                }
            }
            let (revision, document) = match source.get() {
                Some(captured) => captured,
                None => {
                    // `subscribe` takes the document lock before a subscription lock; release
                    // this client first so dispatch never inverts that order. Only a close can
                    // change the subscription meanwhile, and the re-lock below observes it.
                    drop(guard);
                    let Some(captured) = self.capture_source(source_revision) else {
                        self.fail_serialization(&clients);
                        return next_deadline;
                    };
                    guard = lock(&client.subscription);
                    source.get_or_init(|| captured)
                }
            };
            let revision = *revision;
            let document = document.as_ref().as_ref();
            let Some(Subscription { plan, baseline }) = guard.as_mut() else {
                continue;
            };
            let previous = baseline.as_deref();
            // Only this subscription holds a plan whose count is one, so no other client in
            // this dispatch can reuse its extraction or frame; skip the memo maps. A stale count
            // can only cost recomputation.
            let (current, frame) = if Arc::strong_count(plan) == 1 {
                #[cfg(test)]
                self.0.extractions.fetch_add(1, Ordering::Relaxed);
                selected.clear();
                selected.extend(
                    plan.pointers
                        .iter()
                        .map(|tokens| document.and_then(|value| traverse(value, tokens))),
                );
                let frame = self.encode_frame(
                    &mut arena,
                    plan,
                    revision,
                    previous,
                    &selected,
                    &mut changed,
                );
                (None, frame.map_err(drop))
            } else {
                let plan_id = Arc::as_ptr(plan) as usize;
                let current = match extracted.entry(plan_id) {
                    Entry::Occupied(entry) => Arc::clone(&entry.get().1),
                    Entry::Vacant(entry) => {
                        #[cfg(test)]
                        self.0.extractions.fetch_add(1, Ordering::Relaxed);
                        let current = extract(plan, document);
                        Arc::clone(&entry.insert((Arc::clone(plan), current)).1)
                    }
                };
                let baseline_id = previous.map(|value| value.as_ptr() as usize);
                let frame = match encoded.entry((plan_id, baseline_id)).or_insert_with(|| {
                    self.encode_frame(&mut arena, plan, revision, previous, &current, &mut changed)
                }) {
                    Ok(frame) => Ok(frame.clone()),
                    Err(_) => Err(()),
                };
                (Some(current), frame)
            };
            let bytes = match frame {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    client.checked_revision.store(revision, Ordering::Release);
                    continue;
                }
                Err(()) => {
                    drop(guard);
                    client.terminate(SERIALIZATION_FAILED, SERIALIZATION_FAILED_MESSAGE, 1011);
                    self.remove(*id);
                    continue;
                }
            };
            if client.cleaned.load(Ordering::Acquire)
                || client
                    .state
                    .compare_exchange(OPEN, PENDING, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
            {
                continue;
            }
            #[cfg(test)]
            if let Some((entered, release)) = lock(&self.0.pending_gate).clone() {
                entered.wait();
                release.wait();
            }
            match client
                .mailbox
                .try_send_data(V2SubscriptionOutbound::Text(bytes))
            {
                Ok(()) => {
                    match current {
                        Some(current) => *baseline = Some(current),
                        None => store_baseline(baseline, &selected, &changed),
                    }
                    queued.push(client);
                    client.checked_revision.store(revision, Ordering::Release);
                }
                Err(_) => {
                    let _ = client.state.compare_exchange(
                        PENDING,
                        OPEN,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                }
            }
        }
        if !queued.is_empty() {
            // One clock read after every queue operation above: debounce never counts from
            // before a frame was queued, at the cost of counting from up to one dispatch later.
            let elapsed = Instant::now().saturating_duration_since(self.0.epoch);
            let stamp = u64::try_from(elapsed.as_nanos()).unwrap_or(NEVER_QUEUED - 1);
            for client in queued {
                client.queued_at.store(stamp, Ordering::Release);
            }
        }
        drop(selected);
        drop(encoded);
        drop(extracted);
        drop(source);
        self.cleanup_retained_state(&clients);
        next_deadline
    }

    /// The document for `source_revision`, built at most once per revision; `None` when it
    /// failed to serialize.
    fn capture_source(&self, source_revision: u64) -> Option<(u64, Arc<Option<Value>>)> {
        let cached = lock(&self.0.document)
            .as_ref()
            .filter(|(revision, _)| *revision == source_revision)
            .map(|(revision, document)| (*revision, Arc::clone(document)));
        if cached.is_some() {
            return cached;
        }
        let snapshot = self.0.provider.capture();
        let revision = snapshot.revision();
        let document = match snapshot.document() {
            Ok(document) => Arc::new(document),
            Err(ApiError::NotReady) => Arc::new(None),
            Err(ApiError::Serialization(_)) => {
                *lock(&self.0.document) = None;
                return None;
            }
        };
        *lock(&self.0.document) = Some((revision, Arc::clone(&document)));
        Some((revision, document))
    }

    /// Ends every subscribed client after the source document failed to serialize.
    fn fail_serialization(&self, clients: &Arc<HashMap<u64, Arc<Client>>>) {
        for (id, client) in clients.iter() {
            if lock(&client.subscription).is_some() {
                client.terminate(SERIALIZATION_FAILED, SERIALIZATION_FAILED_MESSAGE, 1011);
                self.remove(*id);
            }
        }
        self.cleanup_retained_state(clients);
    }

    /// Encodes `current` against a client's `baseline`, or `None` when nothing changed.
    /// `changed` receives the ids that differ from `baseline`.
    fn encode_frame(
        &self,
        arena: &mut FrameArena,
        #[cfg_attr(not(test), allow(unused_variables))] plan: &Arc<Plan>,
        revision: u64,
        baseline: Option<&[Option<Value>]>,
        current: &[impl Selected],
        changed: &mut Vec<usize>,
    ) -> Result<Option<Bytes>, ApiError> {
        changed.clear();
        if let Some(old) = baseline {
            changes(old, current, changed);
            if changed.is_empty() {
                return Ok(None);
            }
        }
        #[cfg(test)]
        {
            self.0.encodings.fetch_add(1, Ordering::Relaxed);
            if lock(&self.0.failed_encoding_plan)
                .as_ref()
                .is_some_and(|failed| Arc::ptr_eq(failed, plan))
            {
                return Err(ApiError::serialization("fake outer frame failure"));
            }
        }
        match baseline {
            Some(_) => arena.update(revision, current, changed),
            None => arena.initial(revision, current),
        }
        .map(Some)
    }
}

impl V2SubscriptionConnection {
    pub fn subscribe(&self, pointers: Vec<String>) -> Result<(), SubscriptionRequestError> {
        let document = lock(&self.hub.0.document);
        let mut slot = lock(&self.client.subscription);
        if self.client.cleaned.load(Ordering::Acquire)
            || self.client.state.load(Ordering::Acquire) == CLOSED
        {
            return Err(SubscriptionRequestError::AlreadySubscribed);
        }
        if slot.is_some() {
            drop(slot);
            drop(document);
            self.terminate_application(ALREADY_SUBSCRIBED, ALREADY_SUBSCRIBED_MESSAGE, 1008);
            return Err(SubscriptionRequestError::AlreadySubscribed);
        }
        let plan = self.hub.intern_plan(compile_pointers(pointers)?);
        *slot = Some(Subscription {
            plan,
            baseline: None,
        });
        drop(slot);
        drop(document);
        self.hub.wake();
        Ok(())
    }

    pub fn write_completed(&self) {
        if self
            .client
            .state
            .compare_exchange(PENDING, OPEN, Ordering::SeqCst, Ordering::Acquire)
            .is_ok()
        {
            self.hub.wake_reopened();
        }
    }

    pub fn send_pong(&self, data: Bytes) -> bool {
        self.client
            .mailbox
            .try_send_control(V2SubscriptionOutbound::Pong(data))
    }

    pub fn invalid_request(&self) {
        self.terminate_application(INVALID_REQUEST, INVALID_REQUEST_MESSAGE, 1008);
    }

    pub fn invalid_pointer(&self) {
        self.terminate_application(INVALID_POINTER, INVALID_POINTER_MESSAGE, 1008);
    }

    pub fn unsupported_data(&self) {
        self.close(1003);
    }

    pub fn protocol_error(&self, code: u16) {
        debug_assert!(matches!(code, 1002 | 1009));
        self.close(code);
    }

    pub fn terminate_application(&self, code: u16, message: &'static str, close_code: u16) {
        self.client.terminate(code, message, close_code);
        self.hub.remove(self.id);
    }

    pub fn subscription_timed_out(&self) {
        self.terminate_application(SUBSCRIPTION_TIMEOUT, SUBSCRIPTION_TIMEOUT_MESSAGE, 1008);
    }

    pub fn close(&self, code: u16) {
        self.client.close(code);
        self.hub.remove(self.id);
    }

    pub fn cleanup(&self) {
        let remove = {
            let _subscription = lock(&self.client.subscription);
            if self.client.cleaned.swap(true, Ordering::AcqRel) {
                false
            } else {
                self.client.state.store(CLOSED, Ordering::Release);
                true
            }
        };
        if remove {
            self.hub.remove(self.id);
        }
    }
}

impl Drop for V2SubscriptionConnection {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Parses one complete client text message as a selector request.
pub fn parse_subscription_request(data: &[u8]) -> Result<Vec<String>, SubscriptionRequestError> {
    serde_json::from_slice(data).map_err(|_| SubscriptionRequestError::InvalidRequest)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
