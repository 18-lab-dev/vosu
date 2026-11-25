//! Host lock order: focus -> instances -> sessions -> matcher -> store/registry.
//! Publication snapshots authority and releases both global mutexes before
//! committing. No plugin callback runs under these locks.
use super::{InstanceSlot, PluginHost};
use crate::plugin::{PluginInstanceId, session::SessionHandle};
use crate::state::{ClientFacts, SessionKey};
use vosu_plugin_api::{
    PublishFieldRef, decode_publish_batch_ref, decode_publish_field_ref, vosu_opcode, vosu_status,
};

impl PluginHost {
    pub(super) fn handle_publish_raw(
        &self,
        instance: PluginInstanceId,
        opcode: vosu_opcode,
        payload: &[u8],
    ) -> vosu_status {
        match opcode {
            vosu_opcode::VOSU_OP_PUBLISH_FIELD => match decode_publish_field_ref(payload) {
                Ok(entry) => {
                    self.commit_publication(instance, entry.handle, std::iter::once(entry))
                }
                Err(_) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            },
            vosu_opcode::VOSU_OP_PUBLISH_BATCH => match decode_publish_batch_ref(payload) {
                Ok(batch) => self.commit_publication(instance, batch.handle, batch.entries()),
                Err(_) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            },
            _ => vosu_status::VOSU_ERR_UNSUPPORTED,
        }
    }

    pub(super) fn commit_publication<'a>(
        &self,
        instance: PluginInstanceId,
        raw: u64,
        entries: impl ExactSizeIterator<Item = PublishFieldRef<'a>>,
    ) -> vosu_status {
        let publisher = {
            let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            let Some(InstanceSlot::Live(entry)) = instances.get(&instance) else {
                return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
            };
            entry.publisher.clone()
        };
        let handle = SessionHandle(raw);
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let Some((owner, _)) = sessions.owner_and_pid(handle) else {
            // Session handles are monotonic and never reused. The registry
            // exposes its issued high-water mark rather than retaining tombstones.
            return if sessions.was_issued(handle) {
                vosu_status::VOSU_ERR_TARGET_GONE
            } else {
                vosu_status::VOSU_ERR_INVALID_ARGUMENT
            };
        };
        if owner != publisher.grant.plugin_id() {
            return vosu_status::VOSU_ERR_PERMISSION_DENIED;
        }
        if sessions.identify_of(handle).is_none() {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        drop(sessions);
        // StateStore serializes close against commit using the session slot's
        // closed marker. A detached slot cannot be resurrected by this call.
        match self.state.store.commit_from_instance(
            &publisher.grant,
            SessionKey(raw),
            instance.0,
            entries,
        ) {
            Ok(outcome) => {
                let _serial = self.state_serial.lock().unwrap_or_else(|p| p.into_inner());
                self.state_committed(&outcome);
                self.surface_committed(&outcome);
                self.api_committed(&outcome);
                vosu_status::VOSU_OK
            }
            Err(crate::state::CommitError::UnknownSession) => vosu_status::VOSU_ERR_TARGET_GONE,
            Err(error) => error.into(),
        }
    }

    pub(super) fn open_bus_session(&self, handle: SessionHandle, focused: bool) {
        let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        let Some(identify) = sessions.identify_of(handle) else {
            return;
        };
        let Some((owner, pid)) = sessions.owner_and_pid(handle) else {
            return;
        };
        let Some(entry) = instances.values().find_map(|slot| match slot {
            InstanceSlot::Live(entry) if &entry.plugin_id == owner => Some(entry),
            _ => None,
        }) else {
            return;
        };
        let facts = ClientFacts {
            game: identify.game.clone(),
            variant: identify.variant.clone(),
            name: identify.name.clone(),
            version: identify.version.clone(),
            pid,
            session: sessions
                .public_id_of(handle)
                .expect("identified session has public id")
                .to_owned(),
            plugin_id: owner.clone(),
            plugin_version: entry.publisher.version.clone(),
        };
        self.state
            .store
            .open_session(SessionKey(handle.0), &entry.publisher.grant, facts, focused);
    }

    pub(super) fn close_bus_session(&self, handle: SessionHandle) {
        self.state.store.close_session(SessionKey(handle.0));
    }

    pub(super) fn close_bus_sessions_for(&self, plugin_id: &str) {
        let detached = {
            let mut sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            sessions.detach_owner(plugin_id)
        };
        for event in detached {
            self.surface_session_detaching(SessionKey(event.handle));
            self.state_session_detaching(&event, None);
            self.close_bus_session(SessionHandle(event.handle));
        }
    }
}

/// Cold per-instance publisher data stays out of the hot instance-slot enum.
pub(super) struct Publisher {
    pub(super) grant: crate::state::PublisherGrant,
    pub(super) version: String,
    pub(super) read_grant: crate::state::InternalGrant,
    pub(super) read_fields: Vec<String>,
    pub(super) read_resources: Vec<String>,
    pub(super) bundled: bool,
    /// May own an official `vosu.*` identity ([`PackageOrigin::is_official`]):
    /// bundled, or the test harness's official fixture.
    ///
    /// [`PackageOrigin::is_official`]: crate::plugin::PackageOrigin::is_official
    pub(super) official: bool,
    pub(super) _registration: SchemaRegistration,
}

/// Owns namespace registrations across every create/error/shutdown path.
pub(super) struct SchemaRegistration {
    runtime: std::sync::Arc<crate::state::StateRuntime>,
    provider: String,
    registered: bool,
}
impl SchemaRegistration {
    pub(super) fn new(
        runtime: &std::sync::Arc<crate::state::StateRuntime>,
        plugin: &crate::plugin::registry::LoadedPlugin,
    ) -> Result<Self, crate::plugin::error::HostError> {
        use crate::plugin::error::HostError;
        let manifest = plugin.manifest();
        let provided: Vec<_> = manifest
            .provides
            .iter()
            .filter(|p| p.schema.is_some())
            .collect();
        if provided.len() != plugin.namespace_schemas().len() {
            return Err(HostError::State(
                "namespace schema count does not match provides".into(),
            ));
        }
        let mut registry = runtime.registry.write().unwrap_or_else(|p| p.into_inner());
        // Refuse duplicate live providers rather than letting a failed second
        // instance unregister the first instance's schemas.
        if provided.iter().any(|p| {
            registry
                .get(p.id.strip_prefix("field:").unwrap_or(&p.id))
                .is_some()
        }) {
            return Err(HostError::State(
                "namespace provider already registered".into(),
            ));
        }
        // Explicit package-origin provenance (M6a Q3: bundled, or an exact
        // official installed source), never inferred from the backend: a
        // dlopened library or a process package proves nothing either way.
        let provenance = plugin.schema_provenance();
        for (provided, text) in provided.iter().zip(plugin.namespace_schemas()) {
            let ns = provided.id.strip_prefix("field:").unwrap_or(&provided.id);
            let version = semver::Version::parse(&provided.version)
                .map_err(|error| HostError::State(error.to_string()))?;
            if let Err(error) =
                registry.register_versioned(&manifest.id, ns, &version, provenance, text)
            {
                registry.unregister_provider(&manifest.id);
                return Err(HostError::State(error.to_string()));
            }
        }
        Ok(Self {
            runtime: runtime.clone(),
            provider: manifest.id.clone(),
            registered: !provided.is_empty(),
        })
    }
}
impl Drop for SchemaRegistration {
    fn drop(&mut self) {
        if !self.registered {
            return;
        }
        self.runtime
            .registry
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .unregister_provider(&self.provider);
    }
}
