//! Selected-field `/json/v2` WebSocket subscription protocol and delivery hub.

use std::{
    cell::OnceCell,
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
use rustc_hash::FxHashMap;
use serde_json::Value;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::api::ApiError;

pub use super::subscription_wire::{
    ALREADY_SUBSCRIBED, INVALID_POINTER, INVALID_REQUEST, Outbound as V2SubscriptionOutbound,
    Receiver as V2SubscriptionReceiver, SERIALIZATION_FAILED, SUBSCRIPTION_TIMEOUT,
    SubscriptionRequestError, parse_subscription_request,
};
use super::subscription_wire::{
    ALREADY_SUBSCRIBED_MESSAGE, Extraction, FrameArena, INVALID_POINTER_MESSAGE,
    INVALID_REQUEST_MESSAGE, Mailbox, SERIALIZATION_FAILED_MESSAGE, SUBSCRIPTION_TIMEOUT_MESSAGE,
    Selected, Selection, changes, error_frame, extract, store_baseline, traverse,
};
#[cfg(test)]
use super::subscription_wire::{
    FRAME_ARENA_CAPACITY, LINEAR_KEY_SEARCH_MAX, decode_pointer, initial_frame, parse_array_index,
    update_frame,
};

const OPEN: u8 = 0;
const PENDING: u8 = 1;
const CLOSED: u8 = 2;
const UNCHECKED: u64 = u64::MAX;

/// One immutable source snapshot captured at a live publication point.
pub trait V2SubscriptionSnapshot: Send + Sync {
    fn revision(&self) -> u64;
    /// May omit unselected subtrees, but every requested pointer must resolve exactly
    /// as in the complete V2 document. An empty pointer requests that complete document.
    fn document(&self, pointers: &[&[String]]) -> Result<Option<Value>, ApiError>;
}

/// Supplies live publication notifications and atomic snapshots.
pub trait V2SubscriptionProvider: Send + Sync {
    fn subscribe(&self) -> watch::Receiver<u64>;
    fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot>;
}

#[derive(Clone, Debug, Eq)]
struct Plan {
    selection: Selection,
    demand: vosu_osu_state::SubscriptionDemand,
}

impl PartialEq for Plan {
    fn eq(&self, other: &Self) -> bool {
        self.selection == other.selection
    }
}

impl Hash for Plan {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.selection.hash(state);
    }
}

fn compile_pointers(pointers: Vec<String>) -> Result<Plan, SubscriptionRequestError> {
    let selection = Selection::compile(pointers)?;
    let demand = selection.pointers().iter().fold(
        vosu_osu_state::SubscriptionDemand::default(),
        |demand, pointer| demand.union(pointer_demand(pointer)),
    );
    Ok(Plan { selection, demand })
}

/// Match a selected subtree to fields that actually need faster native sampling.
/// Slow settings, identity, folders and beatmap metadata deliberately have no entry.
fn pointer_demand(pointer: &[String]) -> vosu_osu_state::SubscriptionDemand {
    use vosu_osu_state::SubscriptionDemand;
    let overlaps = |field: &[&str]| pointer.iter().zip(field).all(|(left, right)| left == right);
    let precise = overlaps(&["beatmap", "time", "live"]) || overlaps(&["state"]);
    let regular = [
        ["session", "playTime"].as_slice(),
        &["settings", "bassDensity"],
        &["room"],
    ]
    .iter()
    .any(|field| overlaps(field));
    let gameplay_precise = [
        ["keys"].as_slice(),
        &["play", "hitErrorArray"],
        &["play", "pressHitErrors"],
        &["play", "releaseHitErrors"],
        &["play", "keyHitErrors"],
        &["play", "hitOffsets"],
        &["play", "catcher"],
        &["play", "unstableRate"],
    ]
    .iter()
    .any(|field| overlaps(field));
    let gameplay_regular = [
        ["play", "source"].as_slice(),
        &["play", "failed"],
        &["play", "playerName"],
        &["play", "mode"],
        &["play", "score"],
        &["play", "accuracy"],
        &["play", "healthBar"],
        &["play", "hits"],
        &["play", "combo"],
        &["play", "mods"],
        &["play", "rank"],
        &["play", "pp"],
        &["play", "unstableRate"],
        &["leaderboard"],
        &["rankedPlay"],
        &["tourney"],
        &["performance", "accuracy"],
        &["beatmap", "isKiai"],
        &["beatmap", "isBreak"],
        &["beatmap", "stats", "stars", "live"],
        &["beatmap", "time", "rate"],
        &["beatmap", "time", "running"],
        &["settings", "interfaceVisible"],
        &["settings", "replayUIVisible"],
        &["paused"],
    ]
    .iter()
    .any(|field| overlaps(field));
    SubscriptionDemand {
        regular,
        precise,
        gameplay_regular,
        gameplay_precise,
    }
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
        self.mailbox.close_senders();
    }
}

/// Counts established selections, not frames: pending and debounced clients still
/// request reader work. Updated under the client's subscription lock.
#[derive(Default)]
struct DemandCounts {
    lanes: [usize; 4],
}

impl DemandCounts {
    fn change(&mut self, demand: vosu_osu_state::SubscriptionDemand, add: bool) {
        for (count, selected) in self.lanes.iter_mut().zip([
            demand.regular,
            demand.precise,
            demand.gameplay_regular,
            demand.gameplay_precise,
        ]) {
            if selected {
                if add {
                    *count += 1;
                } else {
                    *count -= 1;
                }
            }
        }
    }

    fn aggregate(&self) -> vosu_osu_state::SubscriptionDemand {
        vosu_osu_state::SubscriptionDemand {
            regular: self.lanes[0] != 0,
            precise: self.lanes[1] != 0,
            gameplay_regular: self.lanes[2] != 0,
            gameplay_precise: self.lanes[3] != 0,
        }
    }
}

struct CapturedDocument {
    revision: u64,
    // Weak references pin allocation identities without retaining closed subscriptions.
    plans: Vec<Weak<Plan>>,
    document: Option<Value>,
}

impl CapturedDocument {
    fn includes(&self, plan: &Arc<Plan>) -> bool {
        self.plans
            .iter()
            .any(|included| included.as_ptr() == Arc::as_ptr(plan))
    }
}

struct HubInner {
    provider: Arc<dyn V2SubscriptionProvider>,
    debounce: ArcSwap<Duration>,
    clients: ArcSwap<HashMap<u64, Arc<Client>>>,
    plans: Mutex<Vec<Weak<Plan>>>,
    document: Mutex<Option<Arc<CapturedDocument>>>,
    writes: Mutex<()>,
    accepting: AtomicBool,
    next_id: AtomicU64,
    wake: Notify,
    /// Origin of `Client::queued_at`.
    epoch: Instant,
    /// Set by the first `wake` since the run loop last armed `wake`; later wakes in the same
    /// window are already covered by that notification.
    wake_pending: AtomicBool,
    /// Union of producing lanes requested by open clients' established selections.
    demand: watch::Sender<vosu_osu_state::SubscriptionDemand>,
    demand_counts: Mutex<DemandCounts>,
    shutdown: CancellationToken,
    #[cfg(test)]
    extractions: AtomicU64,
    #[cfg(test)]
    encodings: AtomicU64,
    #[cfg(test)]
    failed_encoding_plan: Mutex<Option<Arc<Plan>>>,
    #[cfg(test)]
    pending_gate: Mutex<Option<(Arc<tests::Barrier>, Arc<tests::Barrier>)>>,
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
            demand: watch::Sender::new(vosu_osu_state::SubscriptionDemand::default()),
            demand_counts: Mutex::new(DemandCounts::default()),
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
            V2SubscriptionReceiver::new(mailbox),
        )
    }

    /// Producing lanes requested by established selections. Slow selectors leave
    /// this empty; gameplay-only lane demand is applied only while playing.
    pub fn demand(&self) -> watch::Receiver<vosu_osu_state::SubscriptionDemand> {
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
            self.release_client(client);
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
            self.release_client(&client);
        }
        self.prune_plans();
        self.clear_document_if_unused();
        self.wake();
    }

    fn release_client(&self, client: &Client) {
        if let Some(subscription) = lock(&client.subscription).take() {
            self.change_demand(subscription.plan.demand, false);
        }
    }

    fn change_demand(&self, demand: vosu_osu_state::SubscriptionDemand, add: bool) {
        let mut counts = lock(&self.0.demand_counts);
        counts.change(demand, add);
        let subscribed = counts.aggregate();
        // Publish under the count lock so concurrent subscribe/close operations
        // cannot publish an older aggregate after a newer one.
        self.0.demand.send_if_modified(|current| {
            let changed = *current != subscribed;
            *current = subscribed;
            changed
        });
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
        self.0
            .demand
            .send_replace(vosu_osu_state::SubscriptionDemand::default());
    }

    fn dispatch(&self, source_revision: u64) -> Option<Instant> {
        let clients = self.0.clients.load_full();
        let now = Instant::now();
        let debounce = **self.0.debounce.load();
        let mut next_deadline: Option<Instant> = None;
        // Captured at the first eligible client, so a pass that can deliver nothing builds no
        // document. A cell, so values borrowed from the document outlive later clients.
        let source: OnceCell<Arc<CapturedDocument>> = OnceCell::new();
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
            let captured = match source.get() {
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
            let revision = captured.revision;
            let document = captured.document.as_ref();
            let Some(Subscription { plan, baseline }) = guard.as_mut() else {
                continue;
            };
            // A selection established during this projection may not be in its union.
            // subscribe already woke the runner; let that pass deliver its initial frame.
            if !captured.includes(plan) {
                continue;
            }
            let previous = baseline.as_deref();
            // Only this subscription holds a plan whose count is one, so no other client in
            // this dispatch can reuse its extraction or frame; skip the memo maps. A stale count
            // can only cost recomputation.
            let (current, frame) = if Arc::strong_count(plan) == 1 {
                #[cfg(test)]
                self.0.extractions.fetch_add(1, Ordering::Relaxed);
                selected.clear();
                selected.extend(
                    plan.selection
                        .pointers()
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
                        let current = extract(&plan.selection, document);
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

    /// Captures once per revision and established selection set. A new plan also
    /// refreshes the projection when the source revision has not changed.
    fn capture_source(&self, source_revision: u64) -> Option<Arc<CapturedDocument>> {
        let cached = lock(&self.0.document);
        let plans: Vec<_> = lock(&self.0.plans)
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        if let Some(document) = cached.as_ref().filter(|document| {
            document.revision == source_revision && plans.iter().all(|plan| document.includes(plan))
        }) {
            return Some(Arc::clone(document));
        }
        // Never hold a registry lock while calling the provider.
        drop(cached);
        let pointers: Vec<_> = plans
            .iter()
            .flat_map(|plan| plan.selection.pointers().iter().map(Vec::as_slice))
            .collect();
        let snapshot = self.0.provider.capture();
        let revision = snapshot.revision();
        let document = match snapshot.document(&pointers) {
            Ok(document) => document,
            Err(ApiError::NotReady) => None,
            Err(ApiError::Serialization(_)) => {
                *lock(&self.0.document) = None;
                return None;
            }
        };
        let captured = Arc::new(CapturedDocument {
            revision,
            plans: plans.iter().map(Arc::downgrade).collect(),
            document,
        });
        *lock(&self.0.document) = Some(Arc::clone(&captured));
        Some(captured)
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
        #[cfg_attr(not(test), expect(unused_variables, reason = "test hook"))] plan: &Arc<Plan>,
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
        self.hub.change_demand(plan.demand, true);
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
