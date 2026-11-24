//! Revalidated bounded tree indexes and plugin-owned alias resolution.
//! Cache locks never span host calls, filesystem work or awaits. No background
//! worker is retained; the host reference is weak and requests own deadlines.
use crate::resources::{ReferenceClass, ReferenceIdentity, ResourceId, TreeIndex, TreeIndexEntry};
use crate::{
    plugin::{PluginHost, ResourceOwner, ResourceReply, ResourceRequest, ResourceRequestError},
    resource_io::{ResolvedResource, ResourceResolver},
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use vosu_plugin_api::{ResourceResolved, TreeList, TreeLookup};

impl From<crate::resources::TreeIndexError> for ResourceRequestError {
    fn from(_: crate::resources::TreeIndexError) -> Self {
        Self::InvalidReply
    }
}

pub const MAX_CACHED_TREE_INDEXES: usize = 16;
struct Cached {
    id: ResourceId,
    owner: ResourceOwner,
    index: Arc<TreeIndex>,
}
pub struct TreeResolver {
    host: Weak<PluginHost>,
    deadline: Duration,
    bytes: ResourceResolver,
    indexes: Mutex<VecDeque<Cached>>,
}
pub struct TreeMember {
    pub entry: TreeIndexEntry,
    pub index: Arc<TreeIndex>,
    pub source: ResolvedResource,
}
impl TreeResolver {
    pub fn new(host: &Arc<PluginHost>, deadline: Duration) -> Self {
        Self {
            host: Arc::downgrade(host),
            deadline,
            bytes: ResourceResolver::new(host, deadline),
            indexes: Mutex::new(VecDeque::new()),
        }
    }
    fn host(
        &self,
        owner: ResourceOwner,
        identity: &ReferenceIdentity,
    ) -> Result<Arc<PluginHost>, ResourceRequestError> {
        let host = self
            .host
            .upgrade()
            .ok_or(ResourceRequestError::TargetGone)?;
        if identity.class != ReferenceClass::Tree
            || !host.resource_provider_matches(owner.instance, &identity.plugin)
        {
            return Err(ResourceRequestError::InvalidRequest);
        }
        if host.resource_owner(owner.instance, owner.handle)? != owner {
            return Err(ResourceRequestError::Cancelled);
        }
        Ok(host)
    }
    /// Caller verifies live registry membership and supplies the kind's manifest
    /// families. Re-list on every request: tree indexes are not immutable, and
    /// even an unchanged plugin revision cannot hide a changed representation.
    pub async fn index(
        &self,
        owner: ResourceOwner,
        identity: &ReferenceIdentity,
        families: &[String],
    ) -> Result<Arc<TreeIndex>, ResourceRequestError> {
        let host = self.host(owner, identity)?;
        let reply = host
            .resource_requests(self.deadline)
            .request(
                owner,
                ResourceRequest::List(TreeList {
                    handle: owner.handle,
                    kind: identity.reference.kind.clone(),
                    key: identity.reference.key.clone(),
                }),
            )
            .await?;
        let ResourceReply::Resolved(ResourceResolved::Tree {
            entries,
            truncated,
            revision,
        }) = reply
        else {
            return Err(ResourceRequestError::InvalidReply);
        };
        let validated = Arc::new(TreeIndex::validate(entries, truncated, revision, families)?);
        self.host(owner, identity)?;
        let mut indexes = self.indexes.lock().unwrap_or_else(|p| p.into_inner());
        let previous = indexes
            .iter()
            .position(|e| e.id == identity.id && e.owner == owner)
            .and_then(|i| indexes.remove(i));
        let index = previous
            .filter(|e| e.index.etag == validated.etag)
            .map_or(validated, |e| e.index);
        if indexes.len() == MAX_CACHED_TREE_INDEXES {
            indexes.pop_front();
        }
        indexes.push_back(Cached {
            id: identity.id.clone(),
            owner,
            index: Arc::clone(&index),
        });
        Ok(index)
    }
    pub async fn lookup(
        &self,
        owner: ResourceOwner,
        identity: &ReferenceIdentity,
        families: &[String],
        path: &str,
    ) -> Result<TreeMember, ResourceRequestError> {
        crate::resources::path::validate(path).map_err(|_| ResourceRequestError::InvalidRequest)?;
        let index = self.index(owner, identity, families).await?;
        let host = self.host(owner, identity)?;
        let reply = host
            .resource_requests(self.deadline)
            .request(
                owner,
                ResourceRequest::Lookup(TreeLookup {
                    handle: owner.handle,
                    kind: identity.reference.kind.clone(),
                    key: identity.reference.key.clone(),
                    path: path.to_owned(),
                }),
            )
            .await?;
        let (entry, source) = match reply {
            ResourceReply::Resolved(ResourceResolved::TreePath {
                root,
                path,
                entry_path,
            }) => {
                let entry = index.member(&entry_path)?.clone();
                let file = self.bytes.open_file(owner, &root, &path).await?;
                if file.size() != entry.size {
                    return Err(ResourceRequestError::InvalidReply);
                }
                (entry, ResolvedResource::File(file))
            }
            ResourceReply::Stream {
                lease,
                entry_path: Some(entry_path),
            } => {
                let entry = index.member(&entry_path)?.clone();
                if lease.size() != entry.size {
                    return Err(ResourceRequestError::InvalidReply);
                }
                (entry, ResolvedResource::Stream(lease))
            }
            _ => return Err(ResourceRequestError::InvalidReply),
        };
        self.host(owner, identity)?;
        Ok(TreeMember {
            entry,
            index,
            source,
        })
    }
    pub fn retire(&self, id: &ResourceId) {
        self.indexes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|entry| entry.id != *id);
        self.bytes.retire(id);
    }
}
