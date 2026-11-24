use super::*;
use crate::identity::PluginInstanceId;
use vosu_surface::routes::{RegistrationError, RegistrationLease, RouteDescriptor, RouteProvider};
use vosu_surface::state::{
    BundledRequester, CapturedSelection, IdentifiedRoster, IdentifiedSession,
};

pub(super) struct Requester {
    pub host: Weak<PluginHost>,
    pub instance: PluginInstanceId,
}

impl Requester {
    pub(super) fn live<'a>(
        &self,
        instances: &'a std::collections::HashMap<PluginInstanceId, InstanceSlot>,
    ) -> Result<&'a super::super::InstanceEntry, SurfaceError> {
        match instances.get(&self.instance) {
            Some(InstanceSlot::Live(entry)) if entry.publisher.bundled => Ok(entry),
            _ => Err(SurfaceError::UnknownReader),
        }
    }
}

impl BundledRequester for Requester {
    fn capture(
        &self,
        target: SessionTarget,
        selectors: &[Selector],
    ) -> Result<CapturedSelection, SurfaceError> {
        let host = self.host.upgrade().ok_or(SurfaceError::UnknownReader)?;
        // A single immutable bus image is loaded while focus and roster are pinned.
        // No callbacks, dispatch, samples or subscription writes occur here.
        let focus = host.focus.lock().unwrap();
        let instances = host.instances.lock().unwrap();
        let reader = self.live(&instances)?;
        let sessions = host.sessions.lock().unwrap();
        let key = resolve(&sessions, focus.focused(), &target)?;
        let mut surfaces = host.surfaces.lock().unwrap();
        let registry = host.state.registry.read().unwrap();
        let parsed = surface::selectors(&registry, selectors)?;
        let state = key.and_then(|key| host.state.store.current(key));
        let foreground = host
            .foreground
            .available()
            .then(|| host.foreground.foreground_pid());
        let mut roster = Vec::new();
        for (handle, owner, pid) in sessions.identified_sessions() {
            let Some(identify) = sessions.identify_of(handle) else {
                continue;
            };
            let Some((owner_instance, _)) = instances.iter().find(
                |(_, slot)| matches!(slot, InstanceSlot::Live(entry) if entry.plugin_id == *owner),
            ) else {
                continue;
            };
            roster.push(IdentifiedSession {
                session: sessions.public_id_of(handle).unwrap().to_owned(),
                generation: sessions.generation_of(handle).unwrap().0,
                owner: owner.clone(),
                owner_instance: owner_instance.0,
                game: identify.game.clone(),
                variant: identify.variant.clone(),
                foreground: foreground.map(|fg| fg == Some(pid)),
            });
        }
        roster.sort_unstable_by(|a, b| a.session.cmp(&b.session));
        if surfaces.last_roster != roster {
            surfaces.roster_revision = surfaces
                .roster_revision
                .checked_add(1)
                .expect("roster revision exhausted");
            surfaces.last_roster.clone_from(&roster);
        }
        let selected =
            key.and_then(|key| sessions.public_id_of(crate::plugin::session::SessionHandle(key.0)));
        if surfaces.last_focus.as_deref()
            != focus
                .focused()
                .and_then(|key| sessions.public_id_of(crate::plugin::session::SessionHandle(key.0)))
        {
            surfaces.focus_revision = surfaces
                .focus_revision
                .checked_add(1)
                .expect("focus revision exhausted");
            surfaces.last_focus = focus
                .focused()
                .and_then(|key| sessions.public_id_of(crate::plugin::session::SessionHandle(key.0)))
                .map(str::to_owned);
        }
        let session = roster
            .iter()
            .find(|entry| Some(entry.session.as_str()) == selected)
            .cloned();
        let mut revisions = Vec::new();
        for (ns, _) in &parsed {
            if !revisions.iter().any(|(name, _)| name == ns.as_ref()) {
                revisions.push((
                    ns.to_string(),
                    state
                        .as_ref()
                        .and_then(|state| state.namespace_revision(ns))
                        .unwrap_or(0),
                ));
            }
        }
        Ok(CapturedSelection {
            session,
            focus_revision: surfaces.focus_revision,
            roster: IdentifiedRoster {
                revision: surfaces.roster_revision,
                sessions: roster.into(),
            },
            revision: state.as_ref().map_or(0, |state| state.revision),
            namespace_revisions: revisions.into(),
            values: surface::selection_granted(
                &registry,
                state.as_deref(),
                &parsed,
                &reader.publisher.read_grant,
                Some(&reader.publisher.read_fields),
            )
            .into(),
        })
    }

    fn capture_roster(&self, game: &str) -> Result<IdentifiedRoster, SurfaceError> {
        let mut roster = self.capture(SessionTarget::Focused, &[])?.roster;
        roster.sessions = roster
            .sessions
            .iter()
            .filter(|entry| entry.game == game)
            .cloned()
            .collect();
        Ok(roster)
    }

    fn subscribe(
        &self,
        target: SessionTarget,
        selectors: Vec<Selector>,
    ) -> Result<SurfaceSubscription, SurfaceError> {
        let host = self.host.upgrade().ok_or(SurfaceError::UnknownReader)?;
        let reader = self
            .live(&host.instances.lock().unwrap())?
            .plugin_id
            .clone();
        host.surface_subscribe(&reader, target, selectors, Some(self.instance))
    }

    fn register_routes(
        &self,
        routes: Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>,
    ) -> Result<RegistrationLease, RegistrationError> {
        super::routes::register(self, routes)
    }
}

impl PluginHost {
    /// Called by trusted composition only, after explicit bundled initialization.
    /// The returned object cannot be rebound by the adapter to a foreign identity.
    pub fn surface_requester(
        self: &Arc<Self>,
        instance: PluginInstanceId,
    ) -> Result<Arc<dyn BundledRequester>, SurfaceError> {
        let requester = Requester {
            host: Arc::downgrade(self),
            instance,
        };
        requester.live(&self.instances.lock().unwrap())?;
        Ok(Arc::new(requester))
    }
}
