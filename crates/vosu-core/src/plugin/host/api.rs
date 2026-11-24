//! Public adapter lock order: focus -> state_serial -> instances -> sessions ->
//! API readers -> schema -> subscriptions -> store (skipping unneeded locks).
//! `api_committed` takes API readers under the caller's `state_serial` guard.
//! Never call plugins or refresh demand under the API lock. Lifecycle
//! notifications run once while focus serializes the completed transition.
use std::sync::{Arc, Weak, atomic::Ordering};

use tokio::sync::watch;

use super::PluginHost;
use crate::api::{
    ApiError, ApiPlan, ApiReader, ApiStateHost, ApiStateSource, ApiSubscription, ApiUpdate,
    SessionTarget,
};
use crate::plugin::{
    focus::FocusKey,
    session::{SessionHandle, SessionRegistry},
};
use crate::state::{
    Audience, CommitOutcome, InternalGrant, LaneBits, MAX_SUBSCRIPTIONS, SchemaRegistry,
    SessionKey, SessionState, StateObject, StateValue, SubscriberId, project_document,
    project_select,
};

struct ApiHost(Weak<PluginHost>);
impl ApiHost {
    fn host(&self) -> Result<Arc<PluginHost>, ApiError> {
        self.0.upgrade().ok_or(ApiError::Closed)
    }
}
impl ApiStateHost for ApiHost {
    fn sessions(&self) -> Result<Vec<StateValue>, ApiError> {
        self.host()?.api_sessions()
    }
    fn session(&self, target: &SessionTarget) -> Result<StateValue, ApiError> {
        self.host()?.api_session(target)
    }
    fn read(&self, target: &SessionTarget, plan: &ApiPlan) -> Result<ApiUpdate, ApiError> {
        self.host()?.api_read(target, plan)
    }
    fn subscribe(
        &self,
        target: Option<SessionTarget>,
        plan: ApiPlan,
    ) -> Result<ApiSubscription, ApiError> {
        self.host()?.api_subscribe(target, plan)
    }
}
struct ApiUnsubscribe {
    host: Weak<PluginHost>,
    id: u64,
}
impl Drop for ApiUnsubscribe {
    fn drop(&mut self) {
        if let Some(host) = self.host.upgrade() {
            host.api_unsubscribe(self.id);
        }
    }
}

fn resolve(
    sessions: &SessionRegistry,
    focus: Option<FocusKey>,
    target: &SessionTarget,
) -> Option<SessionKey> {
    sessions.identified_sessions().find_map(|(handle, _, _)| {
        let matches = match target {
            SessionTarget::Active => focus == Some(FocusKey(handle.0)),
            SessionTarget::Id(id) => sessions.public_id_of(handle) == Some(id.as_str()),
        };
        matches.then_some(SessionKey(handle.0))
    })
}

fn selection(
    registry: &SchemaRegistry,
    state: Option<&SessionState>,
    plan: &ApiPlan,
) -> Vec<Option<StateValue>> {
    plan.pointers()
        .iter()
        .zip(&plan.selectors)
        .map(|(p, selector)| {
            let state = state?;
            if p.is_root() {
                return Some(StateValue::Object(Arc::new(StateObject::new(
                    project_document(registry, state, Audience::Api),
                ))));
            }
            let (ns, pointer) = selector.as_ref()?;
            project_select(registry, state, ns, pointer, Audience::Api).map(|v| v.into_owned())
        })
        .collect()
}
fn document_selection(document: &StateValue, plan: &ApiPlan) -> Vec<Option<StateValue>> {
    plan.pointers()
        .iter()
        .map(|p| document.get(p.tokens()).cloned())
        .collect()
}

impl PluginHost {
    pub fn api_state_source(self: &Arc<Self>) -> ApiStateSource {
        ApiStateSource(Arc::new(ApiHost(Arc::downgrade(self))))
    }

    fn api_check_open(&self) -> Result<(), ApiError> {
        if self.shutting_down.load(Ordering::Acquire) {
            Err(ApiError::Closed)
        } else {
            Ok(())
        }
    }

    // Summaries come from core-owned vosu.client, not plugin-provided JSON.
    // Normalize focused against the single authoritative completed transition.
    fn api_document(&self, sessions: &SessionRegistry, focus: Option<FocusKey>) -> StateValue {
        let values = sessions
            .identified_sessions()
            .filter_map(|(handle, _, _)| {
                let state = self.state.store.current(SessionKey(handle.0))?;
                let StateValue::Object(summary) = state.namespace("vosu.client")? else {
                    return None;
                };
                let fields = summary
                    .iter()
                    .map(|(k, v)| {
                        (
                            Arc::from(k),
                            if k == "focused" {
                                StateValue::Bool(focus == Some(FocusKey(handle.0)))
                            } else {
                                v.clone()
                            },
                        )
                    })
                    .collect();
                Some((
                    Arc::from(sessions.public_id_of(handle)?),
                    StateValue::Object(Arc::new(StateObject::new(fields))),
                ))
            })
            .collect();
        StateValue::Object(Arc::new(StateObject::new(values)))
    }

    pub(crate) fn api_sessions(&self) -> Result<Vec<StateValue>, ApiError> {
        let focus = self.focus.lock().unwrap();
        let sessions = self.sessions.lock().unwrap();
        self.api_check_open()?;
        let StateValue::Object(document) = self.api_document(&sessions, focus.focused()) else {
            unreachable!()
        };
        Ok(document.iter().map(|(_, v)| v.clone()).collect())
    }
    pub(crate) fn api_session(&self, target: &SessionTarget) -> Result<StateValue, ApiError> {
        let plan = ApiPlan::new(&["/vosu.client"])?;
        self.api_read(target, &plan)?
            .values
            .remove(0)
            .ok_or(ApiError::UnknownSession)
    }
    pub(crate) fn api_read(
        &self,
        target: &SessionTarget,
        plan: &ApiPlan,
    ) -> Result<ApiUpdate, ApiError> {
        let focus = self.focus.lock().unwrap();
        let sessions = self.sessions.lock().unwrap();
        self.api_check_open()?;
        let key = resolve(&sessions, focus.focused(), target).ok_or(ApiError::UnknownSession)?;
        let api = self.api_readers.lock().unwrap();
        let state = self
            .state
            .store
            .current(key)
            .ok_or(ApiError::UnknownSession)?;
        let registry = self.state.registry.read().unwrap();
        Ok(ApiUpdate {
            session: sessions
                .public_id_of(SessionHandle(key.0))
                .map(str::to_owned),
            revision: api.revision,
            binding: 0,
            values: selection(&registry, Some(&state), plan),
            lanes: plan.lanes(&registry),
            ended: false,
            error: None,
        })
    }

    pub(crate) fn api_subscribe(
        self: &Arc<Self>,
        target: Option<SessionTarget>,
        plan: ApiPlan,
    ) -> Result<ApiSubscription, ApiError> {
        let focus = self.focus.lock().unwrap();
        let sessions = self.sessions.lock().unwrap();
        self.api_check_open()?;
        let key = match target.as_ref() {
            Some(target @ SessionTarget::Id(_)) => {
                Some(resolve(&sessions, focus.focused(), target).ok_or(ApiError::UnknownSession)?)
            }
            Some(target @ SessionTarget::Active) => resolve(&sessions, focus.focused(), target),
            None => None,
        };
        let mut api = self.api_readers.lock().unwrap();
        self.api_check_open()?;
        if api.readers.len() >= MAX_SUBSCRIPTIONS {
            return Err(ApiError::Capacity);
        }
        let id = api.next.checked_add(1).ok_or(ApiError::Capacity)?;
        let registry = self.state.registry.read().unwrap();
        let demand = if target.is_some() {
            plan.demand(&registry)
        } else {
            Vec::new()
        };
        if let Some(key) = key {
            self.state
                .subscriptions
                .lock()
                .unwrap()
                .replace(
                    &registry,
                    SubscriberId::Api(id),
                    key,
                    demand.clone(),
                    InternalGrant::none(),
                )
                .map_err(|_| ApiError::Capacity)?;
        }
        let state = key.and_then(|key| self.state.store.current(key));
        let values = if target.is_none() {
            document_selection(&self.api_document(&sessions, focus.focused()), &plan)
        } else {
            selection(&registry, state.as_deref(), &plan)
        };
        let initial = ApiUpdate {
            session: key.and_then(|k| sessions.public_id_of(SessionHandle(k.0)).map(str::to_owned)),
            revision: api.revision,
            binding: if target.is_some() { api.revision } else { 0 },
            values,
            lanes: if target.is_some() {
                plan.lanes(&registry)
            } else {
                LaneBits::default()
            },
            ended: false,
            error: None,
        };
        let (updates, receiver) = watch::channel(Arc::new(initial));
        api.next = id;
        api.readers.push(ApiReader {
            id,
            target,
            key,
            plan,
            demand,
            updates,
        });
        drop(registry);
        drop(api);
        drop(sessions);
        drop(focus);
        if let Some(key) = key {
            self.refresh_state_demand(key);
        }
        Ok(ApiSubscription::new(
            receiver,
            ApiUnsubscribe {
                host: Arc::downgrade(self),
                id,
            },
        ))
    }

    pub(crate) fn api_unsubscribe(&self, id: u64) {
        let mut api = self.api_readers.lock().unwrap();
        api.readers.retain(|r| r.id != id);
        let removed = self
            .state
            .subscriptions
            .lock()
            .unwrap()
            .remove_subscriber(SubscriberId::Api(id));
        drop(api);
        for key in removed {
            self.refresh_state_demand(key);
        }
    }

    pub(super) fn api_committed(&self, outcome: &CommitOutcome) {
        let mut api = self.api_readers.lock().unwrap();
        if !api
            .readers
            .iter()
            .any(|r| r.target.is_some() && r.key == Some(outcome.current.key))
        {
            return;
        }
        let Some(state) = self.state.store.current(outcome.current.key) else {
            return;
        };
        let registry = self.state.registry.read().unwrap();
        api.revision = api.revision.checked_add(1).expect("API sequence exhausted");
        let revision = api.revision;
        let mut demand_changed = false;
        for reader in api
            .readers
            .iter_mut()
            .filter(|r| r.target.is_some() && r.key == Some(state.key))
        {
            let lanes = reader.plan.lanes(&registry);
            if !reader.plan.demand_matches(&registry, &reader.demand)
                || reader.updates.borrow().lanes != lanes
            {
                let demand = reader.plan.demand(&registry);
                let mut subscriptions = self.state.subscriptions.lock().unwrap();
                if subscriptions
                    .replace(
                        &registry,
                        SubscriberId::Api(reader.id),
                        state.key,
                        demand.clone(),
                        InternalGrant::none(),
                    )
                    .is_err()
                {
                    subscriptions.remove_subscriber(SubscriberId::Api(reader.id));
                    reader.updates.send_replace(Arc::new(ApiUpdate {
                        session: None,
                        revision,
                        binding: revision,
                        values: vec![None; reader.plan.pointers().len()],
                        lanes: LaneBits::default(),
                        ended: true,
                        error: Some(ApiError::Capacity),
                    }));
                    reader.key = None;
                    demand_changed = true;
                    continue;
                }
                reader.demand = demand;
                demand_changed = true;
            }
            let values = selection(&registry, Some(&state), &reader.plan);
            let current = reader.updates.borrow();
            if current.ended || (current.values == values && current.lanes == lanes) {
                continue;
            }
            let session = current.session.clone();
            let binding = current.binding;
            drop(current);
            reader.updates.send_replace(Arc::new(ApiUpdate {
                session,
                revision,
                binding,
                values,
                lanes,
                ended: false,
                error: None,
            }));
        }
        api.readers.retain(|r| !r.updates.borrow().ended);
        drop(registry);
        drop(api);
        if demand_changed {
            self.refresh_state_demand(state.key);
        }
    }

    /// Called exactly once after both focus flips, with the focus lock held.
    /// Returns demand keys to refresh only after the caller releases focus.
    pub(super) fn api_transition(&self, focused: Option<FocusKey>) -> Vec<SessionKey> {
        let sessions = self.sessions.lock().unwrap();
        let mut api = self.api_readers.lock().unwrap();
        if api.readers.is_empty() {
            return Vec::new();
        }
        let registry = self.state.registry.read().unwrap();
        let mut subscriptions = self.state.subscriptions.lock().unwrap();
        api.revision = api.revision.checked_add(1).expect("API sequence exhausted");
        let revision = api.revision;
        let document = api
            .readers
            .iter()
            .any(|r| r.target.is_none())
            .then(|| self.api_document(&sessions, focused));
        let mut changed = Vec::new();
        for reader in &mut api.readers {
            let Some(target) = &reader.target else {
                let values = document_selection(document.as_ref().unwrap(), &reader.plan);
                if reader.updates.borrow().values != values {
                    reader.updates.send_replace(Arc::new(ApiUpdate {
                        session: None,
                        revision,
                        binding: 0,
                        values,
                        lanes: LaneBits::default(),
                        ended: false,
                        error: None,
                    }));
                }
                continue;
            };
            let key = resolve(&sessions, focused, target);
            let rebound = key != reader.key;
            let mut error = None;
            if rebound {
                changed.extend(subscriptions.remove_subscriber(SubscriberId::Api(reader.id)));
                reader.demand = reader.plan.demand(&registry);
                if let Some(key) = key {
                    if subscriptions
                        .replace(
                            &registry,
                            SubscriberId::Api(reader.id),
                            key,
                            reader.demand.clone(),
                            InternalGrant::none(),
                        )
                        .is_err()
                    {
                        error = Some(ApiError::Capacity)
                    } else {
                        changed.push(key);
                    }
                }
                reader.key = if error.is_some() { None } else { key };
            }
            let state = reader.key.and_then(|k| self.state.store.current(k));
            let values = selection(&registry, state.as_deref(), &reader.plan);
            let ended =
                error.is_some() || (key.is_none() && matches!(target, SessionTarget::Id(_)));
            if rebound || ended || reader.updates.borrow().values != values {
                let binding = if rebound {
                    revision
                } else {
                    reader.updates.borrow().binding
                };
                reader.updates.send_replace(Arc::new(ApiUpdate {
                    session: reader
                        .key
                        .and_then(|k| sessions.public_id_of(SessionHandle(k.0)).map(str::to_owned)),
                    revision,
                    binding,
                    values,
                    lanes: reader.plan.lanes(&registry),
                    ended,
                    error,
                }));
            }
        }
        api.readers.retain(|r| !r.updates.borrow().ended);
        changed.sort_unstable_by_key(|k| k.0);
        changed.dedup();
        changed
    }

    pub(super) fn api_shutdown(&self) {
        let mut api = self.api_readers.lock().unwrap();
        let mut subscriptions = self.state.subscriptions.lock().unwrap();
        api.revision = api.revision.checked_add(1).expect("API sequence exhausted");
        let revision = api.revision;
        for reader in api.readers.drain(..) {
            subscriptions.remove_subscriber(SubscriberId::Api(reader.id));
            reader.updates.send_replace(Arc::new(ApiUpdate {
                session: None,
                revision,
                binding: if reader.target.is_some() { revision } else { 0 },
                values: vec![None; reader.plan.pointers().len()],
                lanes: LaneBits::default(),
                ended: true,
                error: Some(ApiError::Closed),
            }));
        }
    }
}
