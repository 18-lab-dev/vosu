//! Selected-field `/json/v2` WebSocket subscription protocol and delivery hub.

use std::{
    collections::{HashMap, hash_map::Entry},
    hash::{Hash, Hasher},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use bytes::Bytes;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{mpsc, watch};
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

/// One immutable source snapshot captured at a precise publication point.
pub trait V2SubscriptionSnapshot: Send + Sync {
    fn revision(&self) -> u64;
    fn document(&self) -> Result<Option<Value>, ApiError>;
}

/// Supplies precise publication notifications and atomic snapshots.
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

pub struct V2SubscriptionReceiver {
    data: mpsc::Receiver<V2SubscriptionOutbound>,
    control: mpsc::Receiver<V2SubscriptionOutbound>,
    terminal: mpsc::Receiver<V2SubscriptionOutbound>,
}

impl V2SubscriptionReceiver {
    pub async fn recv(&mut self) -> Option<V2SubscriptionOutbound> {
        tokio::select! {
            biased;
            frame = self.terminal.recv() => frame.or_else(|| self.control.try_recv().ok()).or_else(|| self.data.try_recv().ok()),
            frame = self.control.recv() => frame.or_else(|| self.terminal.try_recv().ok()).or_else(|| self.data.try_recv().ok()),
            frame = self.data.recv() => frame.or_else(|| self.terminal.try_recv().ok()).or_else(|| self.control.try_recv().ok()),
        }
    }

    pub fn try_recv(&mut self) -> Option<V2SubscriptionOutbound> {
        self.terminal
            .try_recv()
            .ok()
            .or_else(|| self.control.try_recv().ok())
            .or_else(|| self.data.try_recv().ok())
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

fn traverse<'a>(document: &'a Value, tokens: &[String]) -> Option<&'a Value> {
    let mut current = document;
    for token in tokens {
        current = match current {
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

fn extract(plan: &Plan, document: Option<&Value>) -> Arc<[Option<Value>]> {
    plan.pointers
        .iter()
        .map(|tokens| document.and_then(|value| traverse(value, tokens)).cloned())
        .collect::<Vec<_>>()
        .into()
}

#[derive(Serialize)]
struct ValuePair<'a>(u16, &'a Value);

fn initial_frame(revision: u64, state: &[Option<Value>]) -> Result<Bytes, ApiError> {
    let values: Vec<_> = state
        .iter()
        .enumerate()
        .filter_map(|(id, value)| value.as_ref().map(|value| ValuePair(id as u16, value)))
        .collect();
    crate::api::v2::encode_dto(&(0_u8, revision, values))
}

fn update_frame(
    revision: u64,
    baseline: &[Option<Value>],
    current: &[Option<Value>],
) -> Result<Bytes, ApiError> {
    let mut values = Vec::new();
    let mut removed = Vec::new();
    for (id, (old, new)) in baseline.iter().zip(current).enumerate() {
        if old == new {
            continue;
        }
        if let Some(value) = new {
            values.push(ValuePair(id as u16, value));
        } else {
            removed.push(id as u16);
        }
    }
    crate::api::v2::encode_dto(&(1_u8, revision, values, removed))
}

fn error_frame(code: u16, message: &'static str) -> Bytes {
    crate::api::v2::encode_dto(&(2_u8, code, message))
        .unwrap_or_else(|_| Bytes::from_static(b"[2,4,\"state serialization failed\"]"))
}

struct Client {
    plan: Mutex<Option<Arc<Plan>>>,
    baseline: Mutex<Option<Arc<[Option<Value>]>>>,
    last_queue_at: Mutex<Option<Instant>>,
    checked_revision: AtomicU64,
    state: AtomicU8,
    cleaned: AtomicBool,
    control: mpsc::Sender<V2SubscriptionOutbound>,
    data: mpsc::Sender<V2SubscriptionOutbound>,
    terminal: mpsc::Sender<V2SubscriptionOutbound>,
}

impl Client {
    fn terminate(&self, code: u16, message: &'static str, close_code: u16) {
        let _plan = lock(&self.plan);
        if self.cleaned.swap(true, Ordering::AcqRel) {
            return;
        }
        self.state.store(CLOSED, Ordering::Release);
        let _ = self
            .terminal
            .try_send(V2SubscriptionOutbound::Error(error_frame(code, message)));
        let _ = self
            .terminal
            .try_send(V2SubscriptionOutbound::Close { code: close_code });
    }

    fn close(&self, code: u16) {
        let _plan = lock(&self.plan);
        if self.cleaned.swap(true, Ordering::AcqRel) {
            return;
        }
        self.state.store(CLOSED, Ordering::Release);
        let _ = self
            .terminal
            .try_send(V2SubscriptionOutbound::Close { code });
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
    wake: watch::Sender<u64>,
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
        let (wake, _) = watch::channel(0);
        Self(Arc::new(HubInner {
            provider,
            debounce: ArcSwap::from_pointee(debounce),
            clients: ArcSwap::from_pointee(HashMap::new()),
            plans: Mutex::new(Vec::new()),
            document: Mutex::new(None),
            writes: Mutex::new(()),
            accepting: AtomicBool::new(true),
            next_id: AtomicU64::new(0),
            wake,
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
        let (data, data_rx) = mpsc::channel(1);
        let (control, control_rx) = mpsc::channel(1);
        let (terminal, terminal_rx) = mpsc::channel(2);
        let client = Arc::new(Client {
            plan: Mutex::new(None),
            baseline: Mutex::new(None),
            last_queue_at: Mutex::new(None),
            checked_revision: AtomicU64::new(UNCHECKED),
            state: AtomicU8::new(OPEN),
            cleaned: AtomicBool::new(false),
            data,
            control,
            terminal,
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
            V2SubscriptionReceiver {
                data: data_rx,
                control: control_rx,
                terminal: terminal_rx,
            },
        )
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
        let next = self.0.wake.borrow().wrapping_add(1);
        self.0.wake.send_replace(next);
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
        *lock(&client.plan) = None;
        *lock(&client.baseline) = None;
        *lock(&client.last_queue_at) = None;
    }

    fn prune_plans(&self) {
        let mut plans = lock(&self.0.plans);
        plans.retain(|weak| weak.strong_count() != 0);
        plans.shrink_to_fit();
    }

    fn clear_document_if_unused(&self) {
        let mut document = lock(&self.0.document);
        let clients = self.0.clients.load_full();
        if clients.values().all(|client| lock(&client.plan).is_none()) {
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
        let mut wakes = self.0.wake.subscribe();
        loop {
            if self.0.shutdown.is_cancelled() {
                break;
            }
            let source_revision = *publications.borrow_and_update();
            let deadline = self.dispatch(source_revision);
            tokio::select! {
                _ = self.0.shutdown.cancelled() => break,
                changed = publications.changed() => if changed.is_err() { break; },
                changed = wakes.changed() => if changed.is_err() { break; },
                _ = async {
                    if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await }
                    else { std::future::pending::<()>().await }
                } => {},
            }
        }
    }

    fn dispatch(&self, source_revision: u64) -> Option<Instant> {
        let clients = self.0.clients.load_full();
        let now = Instant::now();
        let debounce = **self.0.debounce.load();
        let mut eligible = Vec::new();
        let mut next_deadline: Option<Instant> = None;
        for (id, client) in clients.iter() {
            let plan = lock(&client.plan).clone();
            let Some(plan) = plan else { continue };
            if client.state.load(Ordering::Acquire) != OPEN {
                continue;
            }
            let initial = lock(&client.baseline).is_none();
            if !initial && client.checked_revision.load(Ordering::Acquire) == source_revision {
                continue;
            }
            if !debounce.is_zero()
                && let Some(queued) = *lock(&client.last_queue_at)
            {
                let Some(deadline) = queued.checked_add(debounce) else {
                    continue;
                };
                if deadline > now {
                    next_deadline = Some(next_deadline.map_or(deadline, |old| old.min(deadline)));
                    continue;
                }
            }
            eligible.push((*id, Arc::clone(client), plan));
        }
        if eligible.is_empty() {
            self.cleanup_retained_state(&clients);
            return next_deadline;
        }

        let cached = lock(&self.0.document)
            .as_ref()
            .filter(|(revision, _)| *revision == source_revision)
            .map(|(revision, document)| (*revision, Arc::clone(document)));
        let (revision, document) = if let Some(cached) = cached {
            cached
        } else {
            let snapshot = self.0.provider.capture();
            let revision = snapshot.revision();
            let document = match snapshot.document() {
                Ok(document) => Arc::new(document),
                Err(ApiError::NotReady) => Arc::new(None),
                Err(ApiError::Serialization(_)) => {
                    drop(eligible);
                    *lock(&self.0.document) = None;
                    for (id, client) in clients.iter() {
                        if lock(&client.plan).is_some() {
                            client.terminate(
                                SERIALIZATION_FAILED,
                                SERIALIZATION_FAILED_MESSAGE,
                                1011,
                            );
                            self.remove(*id);
                        }
                    }
                    self.cleanup_retained_state(&clients);
                    return next_deadline;
                }
            };
            *lock(&self.0.document) = Some((revision, Arc::clone(&document)));
            (revision, document)
        };

        let plan_capacity = lock(&self.0.plans).len().min(eligible.len());
        let mut extracted: HashMap<usize, Arc<[Option<Value>]>> =
            HashMap::with_capacity(plan_capacity);
        let mut encoded: HashMap<(usize, Option<usize>), Result<Bytes, ApiError>> =
            HashMap::with_capacity(plan_capacity);
        for (id, client, plan) in eligible {
            let plan_id = Arc::as_ptr(&plan) as usize;
            let current = match extracted.entry(plan_id) {
                Entry::Occupied(entry) => Arc::clone(entry.get()),
                Entry::Vacant(entry) => {
                    #[cfg(test)]
                    self.0.extractions.fetch_add(1, Ordering::Relaxed);
                    Arc::clone(entry.insert(extract(&plan, document.as_ref().as_ref())))
                }
            };
            let baseline = lock(&client.baseline).clone();
            if baseline.as_deref() == Some(current.as_ref()) {
                client.checked_revision.store(revision, Ordering::Release);
                continue;
            }
            let baseline_id = baseline
                .as_ref()
                .map(|value| Arc::as_ptr(value) as *const () as usize);
            let bytes = encoded
                .entry((plan_id, baseline_id))
                .or_insert_with(|| {
                    #[cfg(test)]
                    {
                        self.0.encodings.fetch_add(1, Ordering::Relaxed);
                        if lock(&self.0.failed_encoding_plan)
                            .as_ref()
                            .is_some_and(|failed| Arc::ptr_eq(failed, &plan))
                        {
                            return Err(ApiError::serialization("fake outer frame failure"));
                        }
                    }
                    match baseline.as_deref() {
                        Some(old) => update_frame(revision, old, &current),
                        None => initial_frame(revision, &current),
                    }
                });
            let bytes = match bytes {
                Ok(bytes) => bytes.clone(),
                Err(_) => {
                    client.terminate(SERIALIZATION_FAILED, SERIALIZATION_FAILED_MESSAGE, 1011);
                    self.remove(id);
                    continue;
                }
            };
            if client
                .state
                .compare_exchange(OPEN, PENDING, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                continue;
            }
            let plan = lock(&client.plan);
            if client.cleaned.load(Ordering::Acquire)
                || client.state.load(Ordering::Acquire) == CLOSED
                || plan.is_none()
            {
                continue;
            }
            #[cfg(test)]
            if let Some((entered, release)) = lock(&self.0.pending_gate).clone() {
                entered.wait();
                release.wait();
            }
            match client.data.try_send(V2SubscriptionOutbound::Text(bytes)) {
                Ok(()) => {
                    *lock(&client.baseline) = Some(current);
                    *lock(&client.last_queue_at) = Some(Instant::now());
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
        drop(encoded);
        drop(extracted);
        drop(document);
        self.cleanup_retained_state(&clients);
        next_deadline
    }
}

impl V2SubscriptionConnection {
    pub fn subscribe(&self, pointers: Vec<String>) -> Result<(), SubscriptionRequestError> {
        let document = lock(&self.hub.0.document);
        let mut slot = lock(&self.client.plan);
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
        *slot = Some(plan);
        drop(slot);
        drop(document);
        self.hub.wake();
        Ok(())
    }

    pub fn write_completed(&self) {
        if self
            .client
            .state
            .compare_exchange(PENDING, OPEN, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.hub.wake();
        }
    }

    pub async fn send_pong(&self, data: Bytes) -> bool {
        self.client
            .control
            .send(V2SubscriptionOutbound::Pong(data))
            .await
            .is_ok()
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
            let _plan = lock(&self.client.plan);
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
