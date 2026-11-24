//! Bundled-only projection onto M3's roots, membership and stream leases.
use super::super::surface::capture::Requester;
use super::surface_files::{Authority, Root, request_error};
use super::*;
use crate::resources::ReferenceClass;
use crate::state::{Pointer, SessionKey};
use vosu_surface::files::*;
use vosu_surface::state::IdentifiedSession;

impl Requester {
    fn selected_owner(
        &self,
        host: &PluginHost,
        selected: &IdentifiedSession,
    ) -> Result<ResourceOwner, FileError> {
        // Never trust editable capture metadata as authority. Recheck the actual
        // instance, owning provider and session occurrence under their locks.
        let instances = host.instances.lock().unwrap();
        self.live(&instances)
            .map_err(|_| FileError::PermissionDenied)?;
        let owner_instance = PluginInstanceId::new(selected.owner_instance);
        let Some(InstanceSlot::Live(provider)) = instances.get(&owner_instance) else {
            return Err(FileError::TargetGone);
        };
        if provider.plugin_id != selected.owner {
            return Err(FileError::PermissionDenied);
        }
        let sessions = host.sessions.lock().unwrap();
        let (handle, _, _) = sessions
            .identified_sessions()
            .find(|(handle, _, _)| {
                sessions.public_id_of(*handle) == Some(selected.session.as_str())
            })
            .ok_or(FileError::TargetGone)?;
        if sessions.generation_of(handle).map(|g| g.0) != Some(selected.generation)
            || sessions
                .owner_and_pid(handle)
                .map(|(owner, _)| owner.as_str())
                != Some(selected.owner.as_str())
            || sessions
                .identify_of(handle)
                .is_none_or(|id| id.game != selected.game || id.variant != selected.variant)
        {
            return Err(FileError::PermissionDenied);
        }
        let resources = host.resource_state.lock().unwrap();
        let generation = resources
            .owners
            .get(&owner_instance)
            .ok_or(FileError::TargetGone)?
            .generation;
        Ok(ResourceOwner {
            instance: owner_instance,
            handle: handle.0,
            generation,
        })
    }
    fn authority(&self) -> Authority {
        Authority {
            host: self.host.clone(),
            requester: self.instance,
        }
    }
}
impl FilesRequester for Requester {
    fn declared_root(
        &self,
        selected: &IdentifiedSession,
        slot: &str,
    ) -> Result<RootLease, FileError> {
        let host = self.host.upgrade().ok_or(FileError::TargetGone)?;
        let owner = self.selected_owner(&host, selected)?;
        {
            let state = host.resource_state.lock().unwrap();
            if !state
                .owners
                .get(&owner.instance)
                .is_some_and(|owner| owner.slots.contains(slot))
            {
                return Err(FileError::PermissionDenied);
            }
        }
        let root = host
            .resource_root(owner.instance, owner.handle, slot)
            .map_err(|error| {
                if error == ResourceRequestError::InvalidReply {
                    FileError::NotReady
                } else {
                    request_error(error)
                }
            })?;
        if host
            .resource_owner(owner.instance, owner.handle)
            .map_err(request_error)?
            != owner
        {
            return Err(FileError::TargetGone);
        }
        Ok(RootLease::new(Root {
            authority: self.authority(),
            root,
        }))
    }
    fn open_calculation_path(
        &self,
        selected: &IdentifiedSession,
        spelling: &str,
    ) -> Operation<'_, OpenedFile> {
        let selected = selected.clone();
        let spelling = spelling.to_owned();
        Box::pin(async move {
            let host = self.host.upgrade().ok_or(FileError::TargetGone)?;
            let owner = self.selected_owner(&host, &selected)?;
            if !Path::new(&spelling).is_absolute() {
                let path = RelativeChild::parse(&spelling)?;
                let root = self.declared_root(&selected, "songs")?;
                return root.open(&path).await;
            }
            let mut slots = {
                let state = host.resource_state.lock().unwrap();
                state
                    .owners
                    .get(&owner.instance)
                    .ok_or(FileError::TargetGone)?
                    .slots
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
            };
            slots.sort_unstable();
            let roots = slots
                .into_iter()
                .filter_map(|slot| host.resource_root(owner.instance, owner.handle, &slot).ok())
                .collect::<Vec<_>>();
            if host
                .resource_owner(owner.instance, owner.handle)
                .map_err(request_error)?
                != owner
            {
                return Err(FileError::TargetGone);
            }
            let authority = self.authority();
            tokio::task::spawn_blocking(move || {
                authority.check()?;
                let mut best = None;
                for root in roots {
                    if !root.is_current() {
                        return Err(FileError::TargetGone);
                    }
                    if let Ok((child, depth)) = root.root.relative_absolute(Path::new(&spelling))
                        && best
                            .as_ref()
                            .is_none_or(|(_, _, previous)| depth > *previous)
                    {
                        best = Some((root, child, depth));
                    }
                }
                let (root, path, _) = best.ok_or(FileError::InvalidPath)?;
                super::surface_files::opened(super::surface_files::open_root(&root, &path)?)
            })
            .await
            .map_err(|_| FileError::TargetGone)?
        })
    }
}

impl Requester {
    pub(crate) fn selected_resource(
        &self,
        selected: &IdentifiedSession,
        token: &str,
    ) -> Result<
        (
            Arc<PluginHost>,
            ResourceOwner,
            Arc<crate::resources::ReferenceIdentity>,
        ),
        FileError,
    > {
        let host = self.host.upgrade().ok_or(FileError::TargetGone)?;
        let owner = self.selected_owner(&host, selected)?;
        let identity = {
            let instances = host.instances.lock().unwrap();
            let reader = self
                .live(&instances)
                .map_err(|_| FileError::PermissionDenied)?;
            let provider = instances
                .get(&owner.instance)
                .ok_or(FileError::TargetGone)?;
            let schemas = host.state.registry.read().unwrap();
            let store = &host.state.store.resources;
            // Reference-changing commits publish while holding this registry,
            // so membership and the immutable selected image share one cut.
            let registry = store.registry.lock().unwrap();
            let (identity, bindings) = registry
                .lookup_bindings(&store.secret, token, &[provider.plugin_id()])
                .map_err(|_| FileError::NotFound)?;
            if identity.class != ReferenceClass::Resource
                || !reader.publisher.read_resources.iter().any(|cap| {
                    match identity.reference.kind.as_str() {
                        "file" => cap == "chart.file",
                        "audio" => cap == "chart.audio",
                        "background" => cap == "chart.background",
                        "set" => cap == "chart.set",
                        "skin" => cap == "skin",
                        kind => {
                            cap.strip_prefix(identity.plugin.as_ref())
                                .and_then(|suffix| suffix.strip_prefix('.'))
                                == Some(kind)
                        }
                    }
                })
            {
                return Err(FileError::PermissionDenied);
            }
            let state = host.state.store.current(SessionKey(owner.handle));
            let permitted = bindings
                .iter()
                .filter(|binding| {
                    binding.instance == owner.instance.0 && binding.session == owner.handle
                })
                .any(|binding| {
                    let Ok(pointer) = Pointer::parse(&binding.pointer) else {
                        return false;
                    };
                    crate::state::surface::selection_granted(
                        &schemas,
                        state.as_deref(),
                        &[(Arc::clone(&binding.namespace), pointer)],
                        &reader.publisher.read_grant,
                        Some(&reader.publisher.read_fields),
                    )
                    .iter()
                    .any(|entry| entry.value.is_some())
                });
            if !permitted {
                return Err(FileError::PermissionDenied);
            }
            identity
        };
        Ok((host, owner, identity))
    }
}
