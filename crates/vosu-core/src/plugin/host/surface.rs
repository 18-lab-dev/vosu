//! D3 adapter. All registrations use StateRuntime's ABI subscription registry.
use std::sync::{Arc, Weak};

use tokio::sync::watch;
use vosu_plugin_api::{Selector, StateEntry};
use vosu_surface::state::{
    SessionTarget, StateReadHost, SurfaceError, SurfaceSubscription, SurfaceUpdate,
};

use super::{InstanceSlot, PluginHost};
use crate::state::surface::{self, SurfaceReader};
use crate::state::{CommitOutcome, InternalGrant, MAX_SUBSCRIPTIONS, SessionKey, SubscriberId};

mod capture;
mod routes;

struct SurfaceHost(Weak<PluginHost>);
struct Unsubscribe {
    host: Weak<PluginHost>,
    id: u64,
}
impl Drop for Unsubscribe {
    fn drop(&mut self) {
        if let Some(host) = self.host.upgrade() {
            host.surface_unsubscribe(self.id);
        }
    }
}

impl StateReadHost for SurfaceHost {
    fn subscribe(
        &self,
        reader: &str,
        target: SessionTarget,
        selectors: Vec<Selector>,
    ) -> Result<SurfaceSubscription, SurfaceError> {
        self.0
            .upgrade()
            .ok_or(SurfaceError::UnknownReader)?
            .surface_subscribe(reader, target, selectors, None)
    }

    fn read(
        &self,
        reader: &str,
        target: SessionTarget,
        selector: &Selector,
    ) -> Result<StateEntry, SurfaceError> {
        let host = self.0.upgrade().ok_or(SurfaceError::UnknownReader)?;
        let grant = host.surface_grant(reader)?;
        let focus = host.focus.lock().unwrap();
        let sessions = host.sessions.lock().unwrap();
        let key = resolve(&sessions, focus.focused(), &target)?;
        let registry = host.state.registry.read().unwrap();
        let selectors = surface::selectors(&registry, std::slice::from_ref(selector))?;
        let state = key.and_then(|key| host.state.store.current(key));
        Ok(surface::selection(&registry, state.as_deref(), &selectors, &grant).remove(0))
    }
}

fn resolve(
    sessions: &crate::plugin::session::SessionRegistry,
    focused: Option<crate::plugin::focus::FocusKey>,
    target: &SessionTarget,
) -> Result<Option<SessionKey>, SurfaceError> {
    match target {
        SessionTarget::Focused => Ok(focused.map(|key| SessionKey(key.0))),
        SessionTarget::Id(id) => sessions
            .identified_sessions()
            .find(|(handle, _, _)| sessions.public_id_of(*handle) == Some(id.as_str()))
            .map(|(handle, _, _)| Some(SessionKey(handle.0)))
            .ok_or(SurfaceError::UnknownSession),
    }
}

impl PluginHost {
    /// Returns a non-owning adapter: neither the global slot nor subscriptions
    /// keep a shut-down plugin host alive.
    pub fn surface_host(self: &Arc<Self>) -> Arc<dyn StateReadHost> {
        Arc::new(SurfaceHost(Arc::downgrade(self)))
    }

    fn surface_grant(&self, reader: &str) -> Result<InternalGrant, SurfaceError> {
        self.instances
            .lock()
            .unwrap()
            .values()
            .find_map(|slot| match slot {
                InstanceSlot::Live(entry)
                    if entry.plugin_id == reader && entry.publisher.bundled =>
                {
                    Some(entry.publisher.read_grant.clone())
                }
                _ => None,
            })
            .ok_or(SurfaceError::UnknownReader)
    }

    fn surface_subscribe(
        self: &Arc<Self>,
        reader: &str,
        target: SessionTarget,
        selectors: Vec<Selector>,
        expected_instance: Option<crate::identity::PluginInstanceId>,
    ) -> Result<SurfaceSubscription, SurfaceError> {
        // Focus/session/instance locks precede the surface registry. Destroy
        // removes the instance before surface cleanup, preventing a late insert.
        let focus = self.focus.lock().unwrap();
        let instances = self.instances.lock().unwrap();
        let sessions = self.sessions.lock().unwrap();
        let publisher = instances
            .iter()
            .find_map(|(instance, slot)| match slot {
                InstanceSlot::Live(entry)
                    if entry.plugin_id == reader
                        && entry.publisher.bundled
                        && expected_instance.is_none_or(|expected| expected == *instance) =>
                {
                    Some(entry.publisher.clone())
                }
                _ => None,
            })
            .ok_or(SurfaceError::UnknownReader)?;
        let grant = publisher.read_grant.clone();
        let read_fields = expected_instance.map(|_| publisher.read_fields.clone());
        let key = resolve(&sessions, focus.focused(), &target)?;
        let mut surfaces = self.surfaces.lock().unwrap();
        if surfaces.readers.len() >= MAX_SUBSCRIPTIONS {
            return Err(SurfaceError::TooManySelectors);
        }
        let registry = self.state.registry.read().unwrap();
        let parsed = surface::selectors(&registry, &selectors)?;
        let id = surfaces
            .next
            .checked_add(1)
            .ok_or(SurfaceError::TooManySelectors)?;
        if let Some(key) = key {
            self.state
                .subscriptions
                .lock()
                .unwrap()
                .replace(
                    &registry,
                    SubscriberId::Surface(id),
                    key,
                    surface::demand_selectors(&parsed, read_fields.as_deref()),
                    grant.clone(),
                )
                .map_err(surface::error)?;
        }
        surfaces.next = id;
        let state = key.and_then(|key| self.state.store.current(key));
        let initial = SurfaceUpdate {
            session: key.and_then(|key| {
                sessions
                    .public_id_of(crate::plugin::session::SessionHandle(key.0))
                    .map(str::to_owned)
            }),
            revision: state.as_ref().map_or(0, |s| s.revision),
            values: surface::selection_granted(
                &registry,
                state.as_deref(),
                &parsed,
                &grant,
                read_fields.as_deref(),
            ),
        };
        let (updates, receiver) = watch::channel(Arc::new(initial));
        surfaces.readers.push(SurfaceReader {
            id,
            reader: reader.to_owned(),
            target,
            key,
            selectors: parsed,
            grant,
            read_fields,
            updates,
        });
        drop(registry);
        drop(surfaces);
        drop(sessions);
        drop(focus);
        drop(instances);
        if let Some(key) = key {
            self.refresh_state_demand(key);
        }
        Ok(SurfaceSubscription::new(
            receiver,
            Unsubscribe {
                host: Arc::downgrade(self),
                id,
            },
        )
        .with_replacement({
            let host = Arc::downgrade(self);
            move |selectors| {
                host.upgrade()
                    .ok_or(SurfaceError::UnknownReader)?
                    .surface_replace(id, selectors)
            }
        }))
    }

    fn surface_replace(&self, id: u64, selectors: Vec<Selector>) -> Result<(), SurfaceError> {
        let mut surfaces = self.surfaces.lock().unwrap();
        let reader = surfaces
            .readers
            .iter_mut()
            .find(|reader| reader.id == id)
            .ok_or(SurfaceError::UnknownReader)?;
        let registry = self.state.registry.read().unwrap();
        let parsed = surface::selectors(&registry, &selectors)?;
        if let Some(key) = reader.key {
            self.state
                .subscriptions
                .lock()
                .unwrap()
                .replace(
                    &registry,
                    SubscriberId::Surface(id),
                    key,
                    surface::demand_selectors(&parsed, reader.read_fields.as_deref()),
                    reader.grant.clone(),
                )
                .map_err(surface::error)?;
        }
        reader.selectors = parsed;
        let state = reader.key.and_then(|key| self.state.store.current(key));
        let session = reader.updates.borrow().session.clone();
        reader.updates.send_replace(Arc::new(SurfaceUpdate {
            session,
            revision: state.as_ref().map_or(0, |state| state.revision),
            values: surface::selection_granted(
                &registry,
                state.as_deref(),
                &reader.selectors,
                &reader.grant,
                reader.read_fields.as_deref(),
            ),
        }));
        let key = reader.key;
        drop(registry);
        drop(surfaces);
        if let Some(key) = key {
            self.refresh_state_demand(key);
        }
        Ok(())
    }

    pub(crate) fn surface_unsubscribe(&self, id: u64) {
        let mut surfaces = self.surfaces.lock().unwrap();
        surfaces.readers.retain(|r| r.id != id);
        let removed = self
            .state
            .subscriptions
            .lock()
            .unwrap()
            .remove_subscriber(SubscriberId::Surface(id));
        drop(surfaces);
        for key in removed {
            self.refresh_state_demand(key);
        }
    }

    pub(super) fn surface_committed(&self, outcome: &CommitOutcome) {
        let surfaces = self.surfaces.lock().unwrap();
        let registry = self.state.registry.read().unwrap();
        // Read the latest state while holding the surface lock. Concurrent
        // notifications may arrive out of order, but can never regress a watch.
        let Some(state) = self.state.store.current(outcome.current.key) else {
            return;
        };
        for r in surfaces.readers.iter().filter(|r| r.key == Some(state.key)) {
            let values = surface::selection_granted(
                &registry,
                Some(&state),
                &r.selectors,
                &r.grant,
                r.read_fields.as_deref(),
            );
            let current = r.updates.borrow();
            if current.values == values {
                continue;
            }
            let session = current.session.clone();
            drop(current);
            r.updates.send_replace(Arc::new(SurfaceUpdate {
                session,
                revision: state.revision,
                values,
            }));
        }
    }

    pub(super) fn surface_focus_changed(&self) {
        {
            let mut surfaces = self.surfaces.lock().unwrap();
            surfaces.focus_revision = surfaces
                .focus_revision
                .checked_add(1)
                .expect("focus revision exhausted");
        }
        self.surface_rebind(None);
    }

    pub(super) fn surface_session_detaching(&self, key: SessionKey) {
        self.surface_roster_changed();
        self.surface_rebind(Some(key));
    }

    fn surface_rebind(&self, detached: Option<SessionKey>) {
        let focus = self.focus.lock().unwrap();
        let sessions = self.sessions.lock().unwrap();
        let mut surfaces = self.surfaces.lock().unwrap();
        let registry = self.state.registry.read().unwrap();
        let mut subscriptions = self.state.subscriptions.lock().unwrap();
        let mut changed = Vec::new();
        for r in &mut surfaces.readers {
            let key = match &r.target {
                SessionTarget::Focused => focus
                    .focused()
                    .map(|key| SessionKey(key.0))
                    .filter(|key| Some(*key) != detached),
                SessionTarget::Id(_) => r.key.filter(|key| Some(*key) != detached),
            };
            if key == r.key {
                continue;
            }
            changed.extend(subscriptions.remove_subscriber(SubscriberId::Surface(r.id)));
            r.key = key;
            let mut failure = None;
            if let Some(key) = key {
                // Rebind uses the same bounded registry path as initial subscribe.
                // A capacity failure is explicit, never stale data or demand.
                if let Err(error) = subscriptions.replace(
                    &registry,
                    SubscriberId::Surface(r.id),
                    key,
                    surface::demand_selectors(&r.selectors, r.read_fields.as_deref()),
                    r.grant.clone(),
                ) {
                    r.key = None;
                    failure = Some(error.to_string());
                } else {
                    changed.push(key);
                }
            }
            let state = r.key.and_then(|key| self.state.store.current(key));
            let mut values = surface::selection_granted(
                &registry,
                state.as_deref(),
                &r.selectors,
                &r.grant,
                r.read_fields.as_deref(),
            );
            if let Some(message) = failure {
                for entry in &mut values {
                    entry.error = Some(vosu_plugin_api::ErrorInfo {
                        code: "subscription_limit".into(),
                        message: message.clone(),
                    });
                }
            }
            r.updates.send_replace(Arc::new(SurfaceUpdate {
                session: r.key.and_then(|key| {
                    sessions
                        .public_id_of(crate::plugin::session::SessionHandle(key.0))
                        .map(str::to_owned)
                }),
                revision: state.as_ref().map_or(0, |s| s.revision),
                values,
            }));
        }
        drop(subscriptions);
        drop(registry);
        drop(surfaces);
        drop(sessions);
        drop(focus);
        changed.sort_unstable_by_key(|key| key.0);
        changed.dedup();
        for key in changed {
            self.refresh_state_demand(key);
        }
    }

    pub(super) fn surface_reader_removed(&self, reader: &str) {
        let removed = {
            let mut surfaces = self.surfaces.lock().unwrap();
            let mut removed = Vec::new();
            let mut index = 0;
            while index < surfaces.routes.len() {
                if surfaces.routes[index].reader == reader {
                    removed.push(surfaces.routes.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            removed
        };
        drop(removed);
        let ids: Vec<_> = self
            .surfaces
            .lock()
            .unwrap()
            .readers
            .iter()
            .filter(|r| r.reader == reader)
            .map(|r| r.id)
            .collect();
        for id in ids {
            self.surface_unsubscribe(id);
        }
    }
}

impl PluginHost {
    pub(super) fn surface_roster_changed(&self) {
        let mut surfaces = self.surfaces.lock().unwrap();
        surfaces.roster_revision = surfaces
            .roster_revision
            .checked_add(1)
            .expect("roster revision exhausted");
    }
}
