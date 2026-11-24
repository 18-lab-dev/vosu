//! Declared-root capabilities and the lazy-operation host seam.
//! Lock order: instances -> sessions -> resources. Filesystem opens, dispatch,
//! waits and callbacks happen after unlock. A root epoch invalidates retained
//! resolutions on replacement or teardown, including capabilities held by caches.
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vosu_plugin_api::{Message, RootDeclare, vosu_status};
use vosu_sys::fs::{SafeChild, SafeRelativePath, SafeRoot};

use super::super::session::SessionHandle;
use super::resource_requests::{
    Requests, ResourceOwner, ResourceReply, ResourceRequest, ResourceRequestError, ResourceRequests,
};
use super::{InstanceSlot, PluginHost};
use crate::identity::PluginInstanceId;

/// A current publication and the live provider authority selected for one read.
pub struct PublishedResource {
    pub identity: Arc<crate::resources::ReferenceIdentity>,
    pub owner: ResourceOwner,
    pub families: Vec<String>,
}
/// Retained authority, not a filesystem path. Cached users must check validity.
#[derive(Clone, Debug)]
pub struct RootCapability {
    root: Arc<SafeRoot>,
    current: Arc<AtomicBool>,
    session_current: Option<Arc<AtomicBool>>,
}

impl RootCapability {
    pub fn is_current(&self) -> bool {
        self.current.load(Ordering::Acquire)
            && self
                .session_current
                .as_ref()
                .is_none_or(|epoch| epoch.load(Ordering::Acquire))
    }

    pub fn open_child(&self, path: &SafeRelativePath) -> Result<SafeChild, ResourceRequestError> {
        if !self.is_current() {
            return Err(ResourceRequestError::Cancelled);
        }
        let child = self.root.open_child(path).map_err(|error| match error {
            vosu_sys::fs::SafePathError::Io(error)
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                ResourceRequestError::NotFound
            }
            _ => ResourceRequestError::InvalidReply,
        })?;
        if !self.is_current() {
            return Err(ResourceRequestError::Cancelled);
        }
        Ok(child)
    }
}

mod surface;
mod surface_files;

struct Owner {
    slots: HashSet<String>,
    roots: HashMap<(Option<u64>, String), RootCapability>,
    sessions: HashMap<u64, Arc<AtomicBool>>,
    generation: u64,
}

impl Owner {
    fn invalidate(&mut self) {
        // Any root update conservatively invalidates all pending resolutions.
        self.generation += 1;
        for root in self.roots.values() {
            root.current.store(false, Ordering::Release);
        }
        // Unchanged retained directories acquire a fresh epoch too.
        for root in self.roots.values_mut() {
            root.current = Arc::new(AtomicBool::new(true));
        }
    }
}

#[derive(Default)]
pub(super) struct ResourceState {
    owners: HashMap<PluginInstanceId, Owner>,
    requests: Requests,
}

impl ResourceState {
    pub fn register(&mut self, instance: PluginInstanceId, slots: &[String]) {
        self.remove(instance);
        self.owners.insert(
            instance,
            Owner {
                slots: slots.iter().cloned().collect(),
                roots: HashMap::new(),
                sessions: HashMap::new(),
                generation: 0,
            },
        );
    }

    pub fn remove(&mut self, instance: PluginInstanceId) {
        if let Some(owner) = self.owners.remove(&instance) {
            for epoch in owner.sessions.values() {
                epoch.store(false, Ordering::Release);
            }
            for root in owner.roots.values() {
                root.current.store(false, Ordering::Release);
            }
        }
        self.requests.cancel(instance, None);
    }
}

struct RequestGuard<'a> {
    state: &'a Mutex<ResourceState>,
    id: u64,
}
impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .requests
            .remove(self.id);
    }
}

impl PluginHost {
    fn resource_authority(
        &self,
        instance: PluginInstanceId,
        handle: Option<u64>,
    ) -> Result<std::sync::MutexGuard<'_, ResourceState>, vosu_status> {
        let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        let slot = instances
            .get(&instance)
            .ok_or(vosu_status::VOSU_ERR_INVALID_ARGUMENT)?;
        if matches!(slot, InstanceSlot::Destroying { .. }) {
            return Err(vosu_status::VOSU_ERR_TARGET_GONE);
        }
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(handle) = handle {
            match sessions.owner_and_pid(SessionHandle(handle)) {
                Some((owner, _)) if owner == slot.plugin_id() => {}
                Some(_) => return Err(vosu_status::VOSU_ERR_PERMISSION_DENIED),
                None if sessions.was_issued(SessionHandle(handle)) => {
                    return Err(vosu_status::VOSU_ERR_TARGET_GONE);
                }
                None => return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT),
            }
        }
        Ok(self
            .resource_state
            .lock()
            .unwrap_or_else(|p| p.into_inner()))
    }

    pub(super) fn handle_root_declare(
        &self,
        instance: PluginInstanceId,
        request: RootDeclare,
    ) -> vosu_status {
        if let Err(status) = self.resource_authority(instance, request.handle) {
            return status;
        }
        {
            let state = self
                .resource_state
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            if !state
                .owners
                .get(&instance)
                .is_some_and(|owner| owner.slots.contains(&request.name))
            {
                return vosu_status::VOSU_ERR_PERMISSION_DENIED;
            }
        }
        // SafeRoot enforces host-native absolute paths. Never open under a host lock.
        let root = match SafeRoot::open(Path::new(&request.path)) {
            Ok(root) => Arc::new(root),
            Err(_) => return vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        };
        // Hold authority through installation to close detach/shutdown races.
        let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        let Some(slot) = instances.get(&instance) else {
            return vosu_status::VOSU_ERR_TARGET_GONE;
        };
        if matches!(slot, InstanceSlot::Destroying { .. }) {
            return vosu_status::VOSU_ERR_TARGET_GONE;
        }
        let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(handle) = request.handle {
            match sessions.owner_and_pid(SessionHandle(handle)) {
                Some((owner, _)) if owner == slot.plugin_id() => {}
                _ => return vosu_status::VOSU_ERR_TARGET_GONE,
            }
        }
        let mut state = self
            .resource_state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(owner) = state.owners.get_mut(&instance) else {
            return vosu_status::VOSU_ERR_TARGET_GONE;
        };
        owner.invalidate();
        owner.roots.insert(
            (request.handle, request.name),
            RootCapability {
                root,
                current: Arc::new(AtomicBool::new(true)),
                session_current: None,
            },
        );
        state.requests.cancel(instance, None);
        vosu_status::VOSU_OK
    }

    /// Session-scoped roots override plugin-wide declarations of the same slot.
    pub fn resource_root(
        &self,
        instance: PluginInstanceId,
        handle: u64,
        name: &str,
    ) -> Result<RootCapability, ResourceRequestError> {
        let mut state = self
            .resource_authority(instance, Some(handle))
            .map_err(|_| ResourceRequestError::TargetGone)?;
        let owner = state
            .owners
            .get_mut(&instance)
            .ok_or(ResourceRequestError::TargetGone)?;
        let mut root = owner
            .roots
            .get(&(Some(handle), name.to_owned()))
            .or_else(|| owner.roots.get(&(None, name.to_owned())))
            .cloned()
            .ok_or(ResourceRequestError::InvalidReply)?;
        root.session_current = Some(Arc::clone(
            owner
                .sessions
                .entry(handle)
                .or_insert_with(|| Arc::new(AtomicBool::new(true))),
        ));
        Ok(root)
    }

    /// Lock order: instances -> reference registry. Release both before checking
    /// session/root authority; no plugin callback or filesystem work runs locked.
    pub fn published_resource(
        &self,
        token: &str,
        class: crate::resources::ReferenceClass,
    ) -> Result<PublishedResource, crate::resources::RegistryError> {
        use crate::resources::RegistryError;
        let (identity, bindings, providers) = {
            let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
            let known: Vec<_> = instances.values().map(InstanceSlot::plugin_id).collect();
            let store = &self.state.store.resources;
            let registry = store.registry.lock().unwrap_or_else(|p| p.into_inner());
            let (identity, bindings) = registry.lookup_bindings(&store.secret, token, &known)?;
            if identity.class != class {
                return Err(RegistryError::NotFound);
            }
            let providers: Vec<_> = instances
                .iter()
                .filter_map(|(&id, slot)| {
                    let InstanceSlot::Live(entry) = slot else {
                        return None;
                    };
                    if entry.plugin_id != identity.plugin.as_ref() {
                        return None;
                    }
                    let kind = entry
                        .publisher
                        .grant
                        .resource_kind(&identity.reference.kind)?;
                    Some((id, kind.families.clone()))
                })
                .collect();
            (identity, bindings, providers)
        };
        for binding in bindings {
            if let Some((_, families)) = providers
                .iter()
                .find(|(id, _)| *id == PluginInstanceId::new(binding.instance))
                && let Ok(owner) =
                    self.resource_owner(PluginInstanceId::new(binding.instance), binding.session)
            {
                return Ok(PublishedResource {
                    identity,
                    owner,
                    families: families.clone(),
                });
            }
        }
        Err(RegistryError::Gone)
    }

    /// Identity/provider check without copying a plugin identifier.
    pub fn resource_provider_matches(&self, instance: PluginInstanceId, plugin: &str) -> bool {
        let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        matches!(instances.get(&instance), Some(InstanceSlot::Live(entry)) if entry.plugin_id == plugin)
    }

    pub fn resource_owner(
        &self,
        instance: PluginInstanceId,
        handle: u64,
    ) -> Result<ResourceOwner, ResourceRequestError> {
        let state = self
            .resource_authority(instance, Some(handle))
            .map_err(|_| ResourceRequestError::TargetGone)?;
        let generation = state
            .owners
            .get(&instance)
            .ok_or(ResourceRequestError::TargetGone)?
            .generation;
        Ok(ResourceOwner {
            instance,
            handle,
            generation,
        })
    }

    pub fn resource_requests(&self, deadline: Duration) -> ResourceRequests {
        ResourceRequests {
            host: self.self_ref.clone(),
            deadline,
        }
    }

    /// Bounded one-shot operation; dropping this future cancels its slot. The
    /// caller owns stream leases; R3 closes them on every HTTP terminal path.
    pub(super) async fn resource_request(
        &self,
        owner: ResourceOwner,
        request: ResourceRequest,
        deadline: Duration,
    ) -> Result<ResourceReply, ResourceRequestError> {
        let ResourceOwner {
            instance,
            handle,
            generation,
        } = owner;
        let message = request.message(owner)?;
        let close_slot = if matches!(
            message,
            Message::ResourceResolve(_) | Message::TreeLookup(_)
        ) {
            Some(
                self.reserve_outbox(instance)
                    .map_err(|_| ResourceRequestError::Busy)?,
            )
        } else {
            None
        };
        let (id, receiver) = {
            let mut state = self
                .resource_authority(instance, Some(handle))
                .map_err(|_| ResourceRequestError::TargetGone)?;
            let current = state
                .owners
                .get(&instance)
                .ok_or(ResourceRequestError::TargetGone)?
                .generation;
            if current != generation {
                return Err(ResourceRequestError::Cancelled);
            }
            if let Message::StreamRead(read) = &message
                && !state.requests.read_allowed(owner, read.stream)
            {
                return Err(ResourceRequestError::Cancelled);
            }
            let reserved = state.requests.reserve(instance, generation, &message)?;
            state.requests.scope(reserved.0, handle);
            if let Some(slot) = close_slot {
                state.requests.arm_stream(
                    reserved.0,
                    owner,
                    self.resource_requests(deadline),
                    slot,
                );
            }
            reserved
        };
        let _guard = RequestGuard {
            state: &self.resource_state,
            id,
        };
        // Reservation precedes dispatch, including synchronous re-entrant replies.
        self.dispatch_resource_request(instance, &message, id)?;
        tokio::time::timeout(deadline, receiver)
            .await
            .map_err(|_| ResourceRequestError::TimedOut)?
            .map_err(|_| ResourceRequestError::Cancelled)?
    }

    fn dispatch_resource_request(
        &self,
        instance: PluginInstanceId,
        message: &Message,
        id: u64,
    ) -> Result<(), ResourceRequestError> {
        let message = super::OwnedMessage::from_message(message, id)
            .map_err(|_| ResourceRequestError::InvalidRequest)?;
        let weak = self.self_ref.clone();
        let task = Box::new(move |vtable: &vosu_plugin_api::vosu_plugin_vtable, ctx| {
            let Some(host) = weak.upgrade() else {
                return;
            };
            let pending = host
                .resource_state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .requests
                .contains(id);
            if !pending {
                return;
            }
            drop(host);
            // No host lock is held across this re-entrant plugin callback.
            if message.call(vtable, ctx) != vosu_status::VOSU_OK
                && let Some(host) = weak.upgrade()
            {
                host.resource_state
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .requests
                    .dispatch_failed(id);
            }
        });
        let instances = self.instances.lock().unwrap_or_else(|p| p.into_inner());
        match instances.get(&instance) {
            Some(InstanceSlot::Live(entry)) => entry
                .instance
                .dispatch_task(task)
                .map_err(|_| ResourceRequestError::DispatchFailed),
            _ => Err(ResourceRequestError::TargetGone),
        }
    }

    pub(super) fn handle_resource_reply(
        &self,
        instance: PluginInstanceId,
        id: u64,
        reply: ResourceReply,
    ) -> vosu_status {
        let stream = match &reply {
            ResourceReply::Resolved(
                vosu_plugin_api::ResourceResolved::Stream { stream, .. }
                | vosu_plugin_api::ResourceResolved::TreeStream { stream, .. },
            ) => Some(*stream),
            _ => None,
        };
        // Reserve outside resource_state (lock order instances -> resources).
        let late_close = match stream {
            Some(stream) => self
                .reserve_outbox(instance)
                .map(|slot| (stream, slot))
                .ok(),
            None => None,
        };
        let mut state = self
            .resource_state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let Some(owner) = state.owners.get(&instance) else {
            return vosu_status::VOSU_ERR_CANCELLED;
        };
        let generation = owner.generation;
        let result = state.requests.complete(instance, generation, id, reply);
        let close = late_close.filter(|(stream, _)| {
            result.is_err()
                && result != Err(ResourceRequestError::Busy)
                && !state.requests.stream_is_live(instance, *stream)
        });
        drop(state);
        if let Some((stream, slot)) = close {
            let message = super::OwnedMessage::from_message(
                &Message::StreamClose(vosu_plugin_api::StreamClose { stream }),
                0,
            )
            .expect("encoding stream close");
            drop(slot.send(message));
        }
        match result {
            Ok(()) => vosu_status::VOSU_OK,
            Err(ResourceRequestError::InvalidReply) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            Err(_) => vosu_status::VOSU_ERR_CANCELLED,
        }
    }

    pub(super) fn resource_session_detached(&self, handle: SessionHandle) {
        let mut state = self
            .resource_state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let ResourceState { owners, requests } = &mut *state;
        for (instance, owner) in owners {
            if let Some(epoch) = owner.sessions.remove(&handle.0) {
                epoch.store(false, Ordering::Release);
            }
            owner.roots.retain(|(scope, _), root| {
                if *scope == Some(handle.0) {
                    root.current.store(false, Ordering::Release);
                    false
                } else {
                    true
                }
            });
            requests.cancel(*instance, Some(handle.0));
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod byte_tests;
#[cfg(test)]
mod resolve;
#[cfg(test)]
mod stream;
#[cfg(test)]
mod tree_tests;
