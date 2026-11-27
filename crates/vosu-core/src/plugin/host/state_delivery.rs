//! Latest-only state delivery on existing instance workers. Lock order: focus ->
//! delivery serialization -> instances -> sessions -> schema registry ->
//! subscriptions -> delivery map -> action state -> individual slot. Publication
//! validation/build never holds delivery serialization, so it cannot stall identify or SYNC reads.
//! Workers release every lock before plugin code. Retries retain weak host/slot
//! references; at most one queued/running/retrying job owns each slot.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use super::{InstanceSlot, PluginHost};
use crate::actions::InvocationId;
use crate::plugin::{PluginInstanceId, dispatch::OwnedMessage, session::SessionHandle};
use crate::state::{
    CommitOutcome, DemandSnapshot, PendingUpdate, Pointer, SessionKey, SubscriberId,
};
use vosu_plugin_api::{
    FocusChanged, SessionAttached, SessionDetached, StateSubscribe, vosu_status,
};

pub(super) struct DeliveryState {
    watchers: HashSet<PluginInstanceId>,
    ending: HashSet<PluginInstanceId>,
    restart: HashSet<PluginInstanceId>,
    lifecycle_cap: usize,
    slots: HashMap<(PluginInstanceId, SessionKey), Arc<Mutex<Slot>>>,
}

impl Default for DeliveryState {
    fn default() -> Self {
        Self {
            watchers: HashSet::new(),
            ending: HashSet::new(),
            restart: HashSet::new(),
            lifecycle_cap: 4096,
            slots: HashMap::new(),
        }
    }
}

pub(super) enum StateHandleUse {
    Subscribe,
    Publish,
    Demand,
}

#[derive(Default)]
struct Slot {
    /// A job is queued, running or waiting to retry admission.
    queued: bool,
    /// `queued`, but its admission was refused: a retry timer resubmits
    /// it. Taken by whichever of that timer and an arriving detach claims
    /// the resubmission first, so the slot never has two jobs.
    retrying: bool,
    dead: bool,
    known: bool,
    lifecycle: bool,
    pending: PendingUpdate,
    baseline_revision: Option<u64>,
    // Post-commit notifications can arrive out of revision order. Remember one
    // revision per selected key (at most 256); wholesale replacement clears it.
    revisions: Vec<(String, String, u64)>,
    attached: Option<SessionAttached>,
    /// Only ids live here; the action reservation owns the bytes and frees them on timeout.
    actions: Vec<InvocationId>,
    focus: Option<FocusChanged>,
    last_focus: Option<FocusChanged>,
    detached: Option<SessionDetached>,
    demand: Option<DemandSnapshot>,
    last_demand: Option<DemandSnapshot>,
}
impl Slot {
    fn dirty(&self) -> bool {
        !self.pending.is_empty()
            || !self.actions.is_empty()
            || self.attached.is_some()
            || self.focus.is_some()
            || self.detached.is_some()
            || self.demand.is_some()
    }

    fn merge_at(&mut self, revision: u64, mut entries: Vec<vosu_plugin_api::StateEntry>) {
        if self
            .baseline_revision
            .is_some_and(|baseline| revision <= baseline)
        {
            return;
        }
        entries.retain(|entry| {
            if let Some((_, _, previous)) = self
                .revisions
                .iter_mut()
                .find(|(ns, pointer, _)| ns == &entry.ns && pointer == &entry.pointer)
            {
                if *previous >= revision {
                    return false;
                }
                *previous = revision;
            } else {
                self.revisions
                    .push((entry.ns.clone(), entry.pointer.clone(), revision));
            }
            true
        });
        self.pending
            .merge(entries)
            .expect("subscription is bounded");
    }
}

impl PluginHost {
    fn delivery_slot(&self, who: PluginInstanceId, key: SessionKey) -> Arc<Mutex<Slot>> {
        self.state_delivery
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slots
            .entry((who, key))
            .or_default()
            .clone()
    }

    pub(super) fn handle_state_subscribe(
        &self,
        who: PluginInstanceId,
        request: StateSubscribe,
    ) -> vosu_status {
        let _serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
        let Some(raw) = request.session else {
            if request.selectors.len() != 1
                || request.selectors[0].ns != "vosu.sessions"
                || !request.selectors[0].pointer.is_empty()
            {
                return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
            }
            return self.subscribe_sessions_watch(who);
        };
        let key = SessionKey(raw);
        let handle = SessionHandle(raw);
        let (plugin, grant) = {
            let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            match instances.get(&who) {
                Some(InstanceSlot::Live(entry)) => {
                    (entry.plugin_id.clone(), entry.publisher.read_grant.clone())
                }
                _ => return vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            }
        };
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let owns = sessions
            .owner_and_pid(handle)
            .is_some_and(|(owner, _)| owner == &plugin);
        if !owns && !self.state_handle_named(who, key, StateHandleUse::Subscribe) {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let state = self.state.store.current(key);
        if state.is_none() && !owns {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let revision = state.as_ref().map(|state| state.revision);
        let empty = crate::state::SessionState::new(key, 0, Vec::new());
        let state = state.as_deref().unwrap_or(&empty);
        let selectors = request
            .selectors
            .into_iter()
            .map(|s| Pointer::parse(&s.pointer).map(|p| (Arc::from(s.ns), p)))
            .collect::<Result<Vec<_>, _>>();
        let Ok(selectors) = selectors else {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        };
        let registry = self
            .state
            .registry
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let mut subscriptions = self
            .state
            .subscriptions
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if subscriptions
            .replace(
                &registry,
                SubscriberId::Plugin(who.0),
                key,
                selectors,
                grant,
            )
            .is_err()
        {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let baseline = subscriptions.baseline(&registry, SubscriberId::Plugin(who.0), state);
        let slot = self.delivery_slot(who, key);
        {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            pending.pending.clear(); // replacement must not retain old selector keys
            pending.baseline_revision = revision;
            pending.revisions.clear();
            pending
                .pending
                .merge(baseline)
                .expect("validated selector count");
        }
        drop(subscriptions);
        drop(registry);
        drop(sessions);
        self.schedule_state_slot(who, key, slot);
        self.refresh_state_demand(key);
        vosu_status::VOSU_OK
    }

    pub(super) fn state_committed(&self, outcome: &CommitOutcome) {
        let registry = self
            .state
            .registry
            .read()
            .unwrap_or_else(|p| p.into_inner());
        let subscriptions = self
            .state
            .subscriptions
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let changes = subscriptions.changes(&registry, outcome);
        let mut jobs = Vec::new();
        for (who, entries) in changes {
            if let SubscriberId::Plugin(raw) = who {
                let who = PluginInstanceId(raw);
                let slot = self.delivery_slot(who, outcome.current.key);
                {
                    let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
                    pending.merge_at(outcome.current.revision, entries);
                }
                jobs.push((who, slot));
            }
        }
        drop(subscriptions);
        drop(registry);
        for (who, slot) in jobs {
            self.schedule_state_slot(who, outcome.current.key, slot);
        }
        let initial = outcome.previous.namespace("vosu.client").is_none();
        let status_changed = initial
            || (outcome
                .touched
                .iter()
                .any(|(ns, _)| ns.as_ref() == "vosu.status")
                && session_status(&outcome.previous) != session_status(&outcome.current));
        if status_changed {
            self.refresh_state_demand(outcome.current.key);
        }
    }

    pub(crate) fn refresh_state_demand(&self, key: SessionKey) {
        let _serial = self
            .state_demand_serial
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(state) = self.state.store.current(key) else {
            return;
        };
        let status = session_status(&state);
        let demand = self
            .state
            .subscriptions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .demand(key, status);
        let has_watchers = self
            .state_watcher_count
            .load(std::sync::atomic::Ordering::Acquire)
            != 0;
        let (owner, guests) = {
            let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let Some((owner, _)) = sessions.owner_and_pid(SessionHandle(key.0)) else {
                return;
            };
            let mut found = None;
            let mut guests = Vec::new();
            for (id, slot) in instances.iter() {
                let InstanceSlot::Live(entry) = slot else {
                    continue;
                };
                if &entry.plugin_id == owner {
                    found = found.or(Some(*id));
                    if !has_watchers {
                        break;
                    }
                } else if has_watchers
                    && entry.publisher.grant.provides_own_namespace()
                    && self.state_handle_named(*id, key, StateHandleUse::Demand)
                {
                    guests.push((*id, entry.plugin_id.clone()));
                }
            }
            (found, guests)
        };
        if let Some(who) = owner {
            self.offer_state_demand(who, key, self.delivery_slot(who, key), demand);
        }
        if !guests.is_empty() {
            self.refresh_guest_demand(key, status, guests);
        }
    }

    fn offer_state_demand(
        &self,
        who: PluginInstanceId,
        key: SessionKey,
        slot: Arc<Mutex<Slot>>,
        demand: DemandSnapshot,
    ) {
        {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            if pending.last_demand.is_none() && demand == DemandSnapshot::default() {
                pending.last_demand = Some(demand);
                return;
            }
            if pending.last_demand.as_ref() == Some(&demand) {
                return;
            }
            pending.last_demand = Some(demand.clone());
            pending.demand = Some(demand);
        }
        self.schedule_state_slot(who, key, slot);
    }

    /// Actions and watches share `known`/`attached` for read subscriptions.
    /// Only a `vosu.sessions` watch sets `lifecycle`, granting a non-owner
    /// publication and demand access.
    fn named_state_slot(
        &self,
        who: PluginInstanceId,
        key: SessionKey,
        usage: StateHandleUse,
    ) -> Option<Arc<Mutex<Slot>>> {
        let delivery = self
            .state_delivery
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let slot = delivery.slots.get(&(who, key))?;
        let pending = slot.lock().unwrap_or_else(|p| p.into_inner());
        let attachment_queued = pending.attached.is_some();
        let live_watch = pending.lifecycle && !delivery.ending.contains(&who);
        let allowed = match usage {
            StateHandleUse::Subscribe => {
                (!pending.lifecycle || live_watch)
                    && (pending.known || (attachment_queued && !pending.actions.is_empty()))
            }
            StateHandleUse::Publish => live_watch && pending.known,
            StateHandleUse::Demand => live_watch && (pending.known || attachment_queued),
        };
        (!pending.dead && allowed).then(|| slot.clone())
    }

    pub(super) fn state_handle_named(
        &self,
        who: PluginInstanceId,
        key: SessionKey,
        usage: StateHandleUse,
    ) -> bool {
        self.named_state_slot(who, key, usage).is_some()
    }

    pub(super) fn offer_guest_demand(
        &self,
        who: PluginInstanceId,
        key: SessionKey,
        demand: DemandSnapshot,
    ) {
        if let Some(slot) = self.named_state_slot(who, key, StateHandleUse::Demand) {
            self.offer_state_demand(who, key, slot, demand);
        }
    }

    fn end_sessions_watch(&self, who: PluginInstanceId) {
        let (cap, slots) = {
            let mut delivery = self
                .state_delivery
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if !delivery.watchers.remove(&who) {
                return;
            }
            self.state_watcher_count.store(
                delivery.watchers.len(),
                std::sync::atomic::Ordering::Release,
            );
            delivery.ending.insert(who);
            (
                delivery.lifecycle_cap,
                delivery
                    .slots
                    .iter()
                    .filter(|((id, _), slot)| {
                        *id == who && slot.lock().unwrap_or_else(|p| p.into_inner()).lifecycle
                    })
                    .map(|((_, key), slot)| (*key, slot.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        {
            let registry = self
                .state
                .registry
                .read()
                .unwrap_or_else(|p| p.into_inner());
            let mut subscriptions = self
                .state
                .subscriptions
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            for (key, _) in &slots {
                subscriptions
                    .replace(
                        &registry,
                        SubscriberId::Plugin(who.0),
                        *key,
                        Vec::new(),
                        crate::state::InternalGrant::none(),
                    )
                    .expect("empty replacement always unsubscribes");
            }
        }
        tracing::warn!(plugin = ?self.plugin_id_of(who), cap, "vosu.sessions lifecycle backlog exceeded; unsubscribing watcher");
        for (key, slot) in slots {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            pending.focus = None;
            if pending.known || !pending.actions.is_empty() {
                // Revoke handle knowledge before releasing serialization. A
                // restart is deferred until all lost notifications have drained.
                pending.known = false;
                pending.detached = Some(SessionDetached {
                    handle: key.0,
                    reason: vosu_plugin_api::DetachReason::Lost,
                });
                drop(pending);
                self.schedule_terminal(who, key, slot);
            } else {
                pending.dead = true;
                pending.pending.clear();
                drop(pending);
                self.retire_state_slot(who, key, &slot);
            }
            self.refresh_state_demand(key);
        }
    }

    fn retire_state_slot(&self, who: PluginInstanceId, key: SessionKey, slot: &Arc<Mutex<Slot>>) {
        let restart = {
            let mut delivery = self
                .state_delivery
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if delivery
                .slots
                .get(&(who, key))
                .is_some_and(|current| Arc::ptr_eq(current, slot))
            {
                delivery.slots.remove(&(who, key));
            }
            if delivery.ending.contains(&who)
                && !delivery.slots.iter().any(|((id, _), slot)| {
                    *id == who && slot.lock().unwrap_or_else(|p| p.into_inner()).lifecycle
                })
            {
                delivery.ending.remove(&who);
                delivery.restart.remove(&who)
            } else {
                false
            }
        };
        if restart {
            let weak = self.self_ref.clone();
            self.state_runtime.spawn(async move {
                if let Some(host) = weak.upgrade() {
                    host.handle_state_subscribe(
                        who,
                        StateSubscribe {
                            session: None,
                            selectors: vec![vosu_plugin_api::Selector {
                                ns: "vosu.sessions".into(),
                                pointer: String::new(),
                            }],
                        },
                    );
                }
            });
        }
    }

    fn queue_attachment(&self, who: PluginInstanceId, attached: SessionAttached) {
        let key = SessionKey(attached.handle);
        let slot = {
            let mut delivery = self
                .state_delivery
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if !delivery.watchers.contains(&who) {
                return;
            }
            if delivery
                .slots
                .get(&(who, key))
                .is_some_and(|s| s.lock().unwrap_or_else(|p| p.into_inner()).lifecycle)
            {
                return;
            }
            let count = delivery
                .slots
                .iter()
                .filter(|((id, _), slot)| {
                    let slot = slot.lock().unwrap_or_else(|p| p.into_inner());
                    *id == who
                        && slot.lifecycle
                        && (slot.attached.is_some() || slot.detached.is_some())
                })
                .count();
            if count >= delivery.lifecycle_cap {
                drop(delivery);
                self.end_sessions_watch(who);
                return;
            }
            let slot = delivery.slots.entry((who, key)).or_default().clone();
            slot.lock().unwrap_or_else(|p| p.into_inner()).lifecycle = true;
            slot
        };
        {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            pending.focus = Some(FocusChanged {
                handle: key.0,
                focused: self
                    .state
                    .store
                    .current(key)
                    .and_then(|s| {
                        s.namespace("vosu.client")
                            .and_then(|v| v.get(&["focused"]))
                            .cloned()
                    })
                    .is_some_and(|v| matches!(v, crate::state::StateValue::Bool(true))),
                foreground: self
                    .foreground
                    .available()
                    .then(|| self.foreground.foreground_pid() == Some(attached.pid)),
            });
            if !pending.known && pending.attached.is_none() {
                pending.attached = Some(attached);
            }
            pending.last_focus = pending.focus;
        }
        self.schedule_state_slot(who, key, slot);
    }

    pub(super) fn state_session_identified(&self, handle: SessionHandle) {
        let _serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
        self.surface_roster_changed();
        let (attached, owner) = {
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            (
                sessions.public_attachment(handle),
                sessions
                    .owner_and_pid(handle)
                    .map(|(owner, _)| owner.clone()),
            )
        };
        let Some(attached) = attached else {
            return;
        };
        let watchers = self
            .state_delivery
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .watchers
            .iter()
            .copied()
            .collect::<Vec<_>>();
        for who in watchers {
            if self.plugin_id_of(who) != owner {
                self.queue_attachment(who, attached.clone());
            }
        }
        if let Some(current) = self.state.store.current(SessionKey(handle.0)) {
            let touched = current
                .namespaces()
                .map(|(ns, _)| (Arc::from(ns), Pointer::root()))
                .collect();
            self.state_committed(&CommitOutcome {
                previous: Arc::new(crate::state::SessionState::new(current.key, 0, Vec::new())),
                current,
                touched,
            });
        }
    }

    pub(super) fn state_focus_changed(&self, event: FocusChanged) {
        let key = SessionKey(event.handle);
        let slots = {
            let delivery = self
                .state_delivery
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            delivery
                .slots
                .iter()
                .filter(|((who, session), slot)| {
                    *session == key
                        && delivery.watchers.contains(who)
                        && slot.lock().unwrap_or_else(|p| p.into_inner()).lifecycle
                })
                .map(|((who, _), slot)| (*who, slot.clone()))
                .collect::<Vec<_>>()
        };
        for (who, slot) in slots {
            {
                let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
                if pending.last_focus == Some(event) {
                    continue;
                }
                pending.last_focus = Some(event);
                pending.focus = Some(event);
            }
            self.schedule_state_slot(who, key, slot);
        }
    }

    pub(super) fn state_session_detaching(&self, event: &SessionDetached, owner: Option<&str>) {
        let _serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
        let _demand = self
            .state_demand_serial
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let key = SessionKey(event.handle);
        // Close serializes with the store commit mutex, capturing even a commit
        // whose notification has not yet acquired state_serial.
        let final_state = self.state.store.close_session(key);
        if let Some(owner) = owner {
            let owners = self
                .instances
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .iter()
                .filter_map(|(id, slot)| match slot {
                    InstanceSlot::Live(entry) if entry.plugin_id == owner => Some(*id),
                    _ => None,
                })
                .collect::<Vec<_>>();
            for who in owners {
                // Owners learned the handle through the original attach path.
                // Their terminal event must follow any owned state subscription.
                self.delivery_slot(who, key)
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .known = true;
            }
        }
        let slots = {
            let delivery = self
                .state_delivery
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            delivery
                .slots
                .iter()
                .filter(|((_, session), _)| *session == key)
                .map(|((who, _), slot)| (*who, slot.clone()))
                .collect::<Vec<_>>()
        };
        {
            let registry = self
                .state
                .registry
                .read()
                .unwrap_or_else(|p| p.into_inner());
            let mut subscriptions = self
                .state
                .subscriptions
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if let Some(final_state) = final_state {
                for (who, slot) in &slots {
                    let entries = subscriptions.baseline(
                        &registry,
                        SubscriberId::Plugin(who.0),
                        &final_state,
                    );
                    slot.lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .merge_at(final_state.revision, entries);
                }
            }
            subscriptions.remove_session(key);
        }
        let mut overflow = Vec::new();
        for (who, slot) in slots {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            if !pending.known && pending.attached.is_some() && pending.actions.is_empty() {
                pending.dead = true;
                pending.pending.clear();
                drop(pending);
                self.retire_state_slot(who, key, &slot);
                continue;
            }
            // Even a subscriber without vosu.sessions must receive its final pending update.
            if pending.detached.is_none() {
                pending.detached = Some(event.clone());
            }
            pending.demand = None;
            drop(pending);
            {
                let delivery = self
                    .state_delivery
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                if delivery.watchers.contains(&who) {
                    let count = delivery
                        .slots
                        .iter()
                        .filter(|((id, _), slot)| {
                            let slot = slot.lock().unwrap_or_else(|p| p.into_inner());
                            *id == who
                                && slot.lifecycle
                                && (slot.attached.is_some() || slot.detached.is_some())
                        })
                        .count();
                    if count > delivery.lifecycle_cap {
                        overflow.push(who);
                    }
                }
            }
            self.schedule_terminal(who, key, slot);
        }
        drop(_demand);
        for who in overflow {
            self.end_sessions_watch(who);
        }
    }

    /// Stops selecting state for `who`, an instance already out of service:
    /// its subscriptions and session watch end, so nothing new reaches its
    /// delivery slots. The slots themselves stay until
    /// [`Self::state_slots_retired`]: a `SESSION_DETACHED` (and the final
    /// state merged before it) that the teardown's own detach already queued
    /// on `who`'s dispatch must still be delivered by that dispatch's drain
    /// (`docs/v2/plugin-abi.md` §7.6 "Per-instance teardown seam").
    pub(super) fn state_subscriber_removed(&self, who: PluginInstanceId) {
        let _serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
        let sessions = self
            .state
            .subscriptions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove_subscriber(SubscriberId::Plugin(who.0));
        {
            let mut delivery = self
                .state_delivery
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            delivery.watchers.remove(&who);
            self.state_watcher_count.store(
                delivery.watchers.len(),
                std::sync::atomic::Ordering::Release,
            );
            delivery.ending.remove(&who);
            delivery.restart.remove(&who);
        }
        for session in sessions {
            self.refresh_state_demand(session);
        }
    }

    /// Drops `who`'s remaining delivery slots once its dispatch has drained
    /// (or was abandoned): a job still queued for one of them then delivers
    /// nothing.
    pub(super) fn state_slots_retired(&self, who: PluginInstanceId) {
        self.state_delivery
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slots
            .retain(|(id, _), slot| {
                if *id != who {
                    return true;
                }
                let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
                slot.dead = true;
                slot.pending.clear();
                false
            });
    }
}

fn session_status(state: &crate::state::SessionState) -> Option<&str> {
    match state
        .namespace("vosu.status")
        .and_then(|s| s.get(&["status"]))
    {
        Some(crate::state::StateValue::String(s)) => Some(s.as_ref()),
        _ => None,
    }
}

mod actions;
mod drain;
mod watch;

#[cfg(test)]
mod tests;
