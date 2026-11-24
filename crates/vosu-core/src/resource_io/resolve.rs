//! Lazy resolution. Only successful retained file opens are cached; streams are
//! per-request leases. Cache -> file locks never span host/plugin calls or I/O.
use crate::plugin::PluginHost;
use crate::plugin::RootCapability;
use crate::plugin::StreamLease;
use crate::plugin::{ResourceOwner, ResourceReply, ResourceRequest, ResourceRequestError};
use crate::resources::{ReferenceIdentity, ResourceId};
use std::collections::VecDeque;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use vosu_plugin_api::{ResourceResolve, ResourceResolved, VOSU_STREAM_CHUNK_MAX_BYTES};
use vosu_sys::fs::{SafeChild, SafeRelativePath};

pub const MAX_CACHED_FILE_RESOLUTIONS: usize = 128;
pub enum ResolvedResource {
    File(Arc<ResolvedFile>),
    Stream(StreamLease),
}
pub struct ResolvedFile {
    root: RootCapability,
    child: Mutex<SafeChild>,
    size: u64,
    current: AtomicBool,
}
impl ResolvedFile {
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn is_current(&self) -> bool {
        self.current.load(Ordering::Acquire) && self.root.is_current()
    }
    pub fn invalidate(&self) {
        self.current.store(false, Ordering::Release);
    }
    pub async fn read(
        self: &Arc<Self>,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, ResourceRequestError> {
        if len > VOSU_STREAM_CHUNK_MAX_BYTES
            || offset.checked_add(u64::from(len)).is_none()
            || offset > self.size
        {
            return Err(ResourceRequestError::InvalidRequest);
        }
        let file = Arc::clone(self);
        tokio::task::spawn_blocking(move || file.read_blocking(offset, len))
            .await
            .map_err(|_| ResourceRequestError::Cancelled)?
    }
    fn read_blocking(&self, offset: u64, len: u32) -> Result<Vec<u8>, ResourceRequestError> {
        if !self.is_current() {
            return Err(ResourceRequestError::Cancelled);
        }
        let result = (|| {
            let mut child = self.child.lock().unwrap_or_else(|p| p.into_inner());
            let before = child
                .file
                .metadata()
                .map_err(|_| ResourceRequestError::InvalidReply)?;
            if before.len() != self.size || before.modified().ok() != child.metadata.modified().ok()
            {
                return Err(ResourceRequestError::Cancelled);
            }
            child
                .file
                .seek(SeekFrom::Start(offset))
                .map_err(|_| ResourceRequestError::InvalidReply)?;
            let mut bytes = vec![0; u64::from(len).min(self.size - offset) as usize];
            child
                .file
                .read_exact(&mut bytes)
                .map_err(|_| ResourceRequestError::InvalidReply)?;
            let after = child
                .file
                .metadata()
                .map_err(|_| ResourceRequestError::InvalidReply)?;
            if before.len() != after.len()
                || before.modified().ok() != after.modified().ok()
                || !self.is_current()
            {
                return Err(ResourceRequestError::Cancelled);
            }
            Ok(bytes)
        })();
        if result.is_err() {
            self.invalidate();
        }
        result
    }
    fn snapshot(&self) -> Result<(), ResourceRequestError> {
        let mut child = self.child.lock().unwrap_or_else(|p| p.into_inner());
        let before = child
            .file
            .metadata()
            .map_err(|_| ResourceRequestError::InvalidReply)?;
        let mut snapshot = tempfile::tempfile().map_err(|_| ResourceRequestError::InvalidReply)?;
        child
            .file
            .seek(SeekFrom::Start(0))
            .map_err(|_| ResourceRequestError::InvalidReply)?;
        let copied = std::io::copy(&mut child.file, &mut snapshot)
            .map_err(|_| ResourceRequestError::InvalidReply)?;
        let after = child
            .file
            .metadata()
            .map_err(|_| ResourceRequestError::InvalidReply)?;
        if copied != self.size
            || before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
            || !self.is_current()
        {
            self.invalidate();
            return Err(ResourceRequestError::Cancelled);
        }
        let metadata = snapshot
            .metadata()
            .map_err(|_| ResourceRequestError::InvalidReply)?;
        *child = SafeChild {
            file: snapshot,
            metadata,
        };
        Ok(())
    }
}
struct Cached {
    id: ResourceId,
    owner: ResourceOwner,
    file: Arc<ResolvedFile>,
}
pub struct ResourceResolver {
    host: Weak<PluginHost>,
    deadline: Duration,
    files: Mutex<VecDeque<Cached>>,
    streams: Mutex<VecDeque<(ResourceId, Weak<crate::plugin::resource_lease::LeaseInner>)>>,
}
impl ResourceResolver {
    pub fn new(host: &Arc<PluginHost>, deadline: Duration) -> Self {
        Self {
            host: Arc::downgrade(host),
            deadline,
            files: Mutex::new(VecDeque::new()),
            streams: Mutex::new(VecDeque::new()),
        }
    }
    /// The caller must verify current registry membership before every request.
    /// Identity remains publish-time authority; no resolved path becomes an ID.
    pub async fn resolve(
        &self,
        owner: ResourceOwner,
        identity: &ReferenceIdentity,
    ) -> Result<ResolvedResource, ResourceRequestError> {
        let host = self
            .host
            .upgrade()
            .ok_or(ResourceRequestError::TargetGone)?;
        if !host.resource_provider_matches(owner.instance, &identity.plugin) {
            return Err(ResourceRequestError::InvalidRequest);
        }
        if host.resource_owner(owner.instance, owner.handle)? != owner {
            return Err(ResourceRequestError::Cancelled);
        }
        {
            let mut files = self.files.lock().unwrap_or_else(|p| p.into_inner());
            files.retain(|entry| entry.file.is_current());
            if let Some(index) = files
                .iter()
                .position(|entry| entry.id == identity.id && entry.owner == owner)
            {
                let entry = files.remove(index).expect("cache position exists");
                let file = Arc::clone(&entry.file);
                files.push_back(entry);
                return Ok(ResolvedResource::File(file));
            }
        }
        let reply = host
            .resource_requests(self.deadline)
            .request(
                owner,
                ResourceRequest::Resolve(ResourceResolve {
                    handle: owner.handle,
                    kind: identity.reference.kind.clone(),
                    key: identity.reference.key.clone(),
                }),
            )
            .await?;
        match reply {
            ResourceReply::Resolved(ResourceResolved::Path { root, path }) => {
                let file = self.open_file(owner, &root, &path).await?;
                // Freeze mutable bytes on a cold worker using bounded memory.
                let snapshot = Arc::clone(&file);
                tokio::task::spawn_blocking(move || snapshot.snapshot())
                    .await
                    .map_err(|_| ResourceRequestError::Cancelled)??;
                let mut files = self.files.lock().unwrap_or_else(|p| p.into_inner());
                if !file.is_current() {
                    return Err(ResourceRequestError::Cancelled);
                }
                if files.len() == MAX_CACHED_FILE_RESOLUTIONS
                    && let Some(old) = files.pop_front()
                {
                    old.file.invalidate();
                }
                files.push_back(Cached {
                    id: identity.id.clone(),
                    owner,
                    file: Arc::clone(&file),
                });
                Ok(ResolvedResource::File(file))
            }
            ResourceReply::Stream {
                lease,
                entry_path: None,
            } => {
                let mut streams = self.streams.lock().unwrap_or_else(|p| p.into_inner());
                streams
                    .retain(|(_, lease)| lease.upgrade().is_some_and(|lease| lease.is_current()));
                if streams.len() == MAX_CACHED_FILE_RESOLUTIONS
                    && let Some((_, old)) = streams.pop_front()
                    && let Some(old) = old.upgrade()
                {
                    old.close();
                }
                streams.push_back((identity.id.clone(), Arc::downgrade(&lease.0)));
                Ok(ResolvedResource::Stream(lease))
            }
            _ => Err(ResourceRequestError::InvalidReply),
        }
    }
    /// Tree lookup shares this retained-root opening path after index validation.
    pub async fn open_file(
        &self,
        owner: ResourceOwner,
        root: &str,
        path: &str,
    ) -> Result<Arc<ResolvedFile>, ResourceRequestError> {
        let host = self
            .host
            .upgrade()
            .ok_or(ResourceRequestError::TargetGone)?;
        if host.resource_owner(owner.instance, owner.handle)? != owner {
            return Err(ResourceRequestError::Cancelled);
        }
        let root = host.resource_root(owner.instance, owner.handle, root)?;
        let path = SafeRelativePath::parse(path).map_err(|_| ResourceRequestError::InvalidReply)?;
        tokio::task::spawn_blocking(move || {
            let child = root.open_child(&path)?;
            if !child.metadata.is_file() {
                return Err(ResourceRequestError::InvalidReply);
            }
            Ok(Arc::new(ResolvedFile {
                size: child.metadata.len(),
                root,
                child: Mutex::new(child),
                current: AtomicBool::new(true),
            }))
        })
        .await
        .map_err(|_| ResourceRequestError::Cancelled)?
    }
    pub fn retire(&self, id: &ResourceId) {
        self.files
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|entry| {
                if entry.id == *id {
                    entry.file.invalidate();
                    false
                } else {
                    true
                }
            });
        self.streams
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|(resource, lease)| {
                if resource == id {
                    if let Some(lease) = lease.upgrade() {
                        lease.close();
                    }
                    false
                } else {
                    lease.strong_count() != 0
                }
            });
    }
}
