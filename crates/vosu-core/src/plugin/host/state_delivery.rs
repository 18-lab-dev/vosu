//! Latest-only state delivery on the existing instance workers. Lock order:
//! host lifecycle locks -> subscriptions -> delivery map -> individual slot.
//! A worker releases all locks before invoking plugin code. Retries retain only
//! weak host/slot references; at most one queued/running/retrying job owns a slot.
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{InstanceSlot, PluginHost};
use crate::plugin::{
    dispatch::{OwnedMessage, PluginCtx},
    host_services::PluginInstanceId,
    session::SessionHandle,
};
use crate::state::{
    CommitOutcome, DemandSnapshot, PendingUpdate, Pointer, SessionKey, SubscriberId, split_update,
};
use vosu_plugin_api::{
    FocusChanged, Message, SessionAttached, SessionDetached, StateSubscribe, VOSU_FRAME_MAX_BYTES,
    vosu_plugin_vtable, vosu_status,
};

#[derive(Default)]
pub(super) struct DeliveryState {
    watchers: HashSet<PluginInstanceId>,
    slots: HashMap<(PluginInstanceId, SessionKey), Arc<Mutex<Slot>>>,
}

#[derive(Default)]
struct Slot {
    queued: bool,
    dead: bool,
    known: bool,
    pending: PendingUpdate,
    attached: Option<SessionAttached>,
    focus: Option<FocusChanged>,
    last_focus: Option<FocusChanged>,
    detached: Option<SessionDetached>,
    demand: Option<DemandSnapshot>,
    last_demand: Option<DemandSnapshot>,
}
impl Slot {
    fn dirty(&self) -> bool {
        !self.pending.is_empty()
            || self.attached.is_some()
            || self.focus.is_some()
            || self.detached.is_some()
            || self.demand.is_some()
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
        let Some(plugin) = self.plugin_id_of(who) else {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        };
        let _serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
        let Some(raw) = request.session else {
            if request.selectors.len() != 1
                || request.selectors[0].ns != "vosu.sessions"
                || !request.selectors[0].pointer.is_empty()
            {
                return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
            }
            let attachments = {
                let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
                let mut delivery = self
                    .state_delivery
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                delivery.watchers.insert(who);
                sessions
                    .identified_sessions()
                    .filter(|(_, owner, _)| owner.as_str() != plugin)
                    .filter_map(|(handle, _, _)| sessions.public_attachment(handle))
                    .collect::<Vec<_>>()
            };
            for attached in attachments {
                self.queue_attachment(who, attached);
            }
            return vosu_status::VOSU_OK;
        };
        let key = SessionKey(raw);
        let handle = SessionHandle(raw);
        let grant = {
            let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            match instances.get(&who) {
                Some(InstanceSlot::Live(entry)) => entry.publisher.read_grant.clone(),
                _ => return vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            }
        };
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let owns = sessions
            .owner_and_pid(handle)
            .is_some_and(|(owner, _)| owner == &plugin);
        let known = self
            .state_delivery
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .slots
            .get(&(who, key))
            .is_some_and(|slot| slot.lock().unwrap_or_else(|p| p.into_inner()).known);
        if !owns && !known {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let Some(state) = self.state.store.current(key) else {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        };
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
        let baseline = subscriptions.baseline(&registry, SubscriberId::Plugin(who.0), &state);
        let slot = self.delivery_slot(who, key);
        {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            pending.pending.clear(); // replacement must not retain old selector keys
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
                slot.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .pending
                    .merge(entries)
                    .expect("subscription is bounded");
                jobs.push((who, slot));
            }
        }
        drop(subscriptions);
        drop(registry);
        for (who, slot) in jobs {
            self.schedule_state_slot(who, outcome.current.key, slot);
        }
        self.refresh_state_demand(outcome.current.key);
    }

    pub(crate) fn refresh_state_demand(&self, key: SessionKey) {
        let _serial = self
            .state_demand_serial
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(state) = self.state.store.current(key) else {
            return;
        };
        let status = state
            .namespace("vosu.status")
            .and_then(|s| s.get(&["status"]));
        let status = match status {
            Some(crate::state::StateValue::String(s)) => Some(s.as_ref()),
            _ => None,
        };
        let demand = self
            .state
            .subscriptions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .demand(key, status);
        let who = {
            let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            let Some((owner, _)) = sessions.owner_and_pid(SessionHandle(key.0)) else {
                return;
            };
            instances.iter().find_map(|(id, slot)| match slot {
                InstanceSlot::Live(entry) if &entry.plugin_id == owner => Some(*id),
                _ => None,
            })
        };
        let Some(who) = who else {
            return;
        };
        let slot = self.delivery_slot(who, key);
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

    fn queue_attachment(&self, who: PluginInstanceId, attached: SessionAttached) {
        let key = SessionKey(attached.handle);
        let slot = self.delivery_slot(who, key);
        {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            if pending.known || pending.attached.is_some() {
                return;
            }
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
            pending.last_focus = pending.focus;
            pending.attached = Some(attached);
        }
        self.schedule_state_slot(who, key, slot);
    }

    pub(super) fn state_session_identified(&self, handle: SessionHandle) {
        let _serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
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
                .filter(|((who, session), _)| *session == key && delivery.watchers.contains(who))
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

    pub(super) fn state_session_detaching(&self, event: &SessionDetached) {
        let _serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
        let _demand = self
            .state_demand_serial
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let key = SessionKey(event.handle);
        self.state
            .subscriptions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove_session(key);
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
        for (who, slot) in slots {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            // Even a subscriber without vosu.sessions must receive its final pending update.
            pending.detached = Some(event.clone());
            pending.demand = None;
            drop(pending);
            self.schedule_state_slot(who, key, slot);
        }
        self.state.store.close_session(key);
    }

    pub(super) fn state_subscriber_removed(&self, who: PluginInstanceId) {
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
            delivery.slots.retain(|(id, _), slot| {
                if *id != who {
                    return true;
                }
                let mut slot = slot.lock().unwrap_or_else(|p| p.into_inner());
                slot.dead = true;
                slot.pending.clear();
                false
            });
        }
        for session in sessions {
            self.refresh_state_demand(session);
        }
    }

    fn schedule_state_slot(&self, who: PluginInstanceId, key: SessionKey, slot: Arc<Mutex<Slot>>) {
        {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            if pending.dead || pending.queued || !pending.dirty() {
                return;
            }
            pending.queued = true;
        }
        self.submit_state_slot(who, key, slot);
    }

    fn submit_state_slot(&self, who: PluginInstanceId, key: SessionKey, slot: Arc<Mutex<Slot>>) {
        let weak = self.self_ref.clone();
        let job_slot = slot.clone();
        let task = Box::new(move |vtable: &vosu_plugin_vtable, ctx: PluginCtx| {
            if let Some(host) = weak.upgrade() {
                host.drain_state_slot(who, key, &job_slot, vtable, ctx);
            }
        });
        let result = {
            let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            match instances.get(&who) {
                Some(InstanceSlot::Live(entry)) => entry.instance.dispatch_task(task),
                _ => {
                    slot.lock().unwrap_or_else(|p| p.into_inner()).dead = true;
                    self.state_delivery
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .slots
                        .remove(&(who, key));
                    return;
                }
            }
        };
        if result.is_err() {
            let weak = self.self_ref.clone();
            let slot = Arc::downgrade(&slot);
            self.state_runtime.spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                if let (Some(host), Some(slot)) = (weak.upgrade(), slot.upgrade()) {
                    if !slot.lock().unwrap_or_else(|p| p.into_inner()).dead {
                        host.submit_state_slot(who, key, slot);
                    }
                }
            });
        }
    }

    fn drain_state_slot(
        &self,
        who: PluginInstanceId,
        key: SessionKey,
        slot: &Arc<Mutex<Slot>>,
        vtable: &vosu_plugin_vtable,
        ctx: PluginCtx,
    ) {
        let (attached, focus, entries, demand, detached, known) = {
            let mut pending = slot.lock().unwrap_or_else(|p| p.into_inner());
            if pending.dead {
                return;
            }
            let attached = pending.attached.take();
            // Mark before dispatch: a subscription submitted reentrantly from
            // SESSION_ATTACHED must already recognize the handle.
            pending.known |= attached.is_some();
            (
                attached,
                pending.focus.take(),
                pending.pending.take(),
                pending.demand.take(),
                pending.detached.take(),
                pending.known,
            )
        };
        let send = |message: Message| {
            if let Ok(message) = OwnedMessage::from_message(&message, 0) {
                message.call(vtable, ctx);
            }
        };
        if let Some(attached) = attached {
            send(Message::SessionAttached(attached));
        }
        if let Some(focus) = focus {
            send(Message::FocusChanged(focus));
        }
        if !entries.is_empty() {
            for update in split_update(key.0, entries, VOSU_FRAME_MAX_BYTES as usize)
                .expect("ABI cap fits errors")
            {
                send(Message::StateUpdate(update));
            }
        }
        if let Some(demand) = demand {
            let demand = vosu_plugin_api::DemandChanged {
                handle: key.0,
                regular: demand.regular,
                precise: demand.precise,
                selectors: demand
                    .selectors
                    .into_iter()
                    .map(|(ns, pointer)| vosu_plugin_api::FieldSelector {
                        ns: ns.to_string(),
                        pointer: pointer.to_string(),
                    })
                    .collect(),
            };
            send(Message::DemandChanged(demand));
        }
        if let Some(detached) = detached {
            if known {
                send(Message::SessionDetached(detached));
            }
            slot.lock().unwrap_or_else(|p| p.into_inner()).dead = true;
            self.state_delivery
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .slots
                .remove(&(who, key));
            return;
        }
        slot.lock().unwrap_or_else(|p| p.into_inner()).queued = false;
        self.schedule_state_slot(who, key, slot.clone());
    }
}
