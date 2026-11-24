//! Bounded, instance-owned resource correlation. Never dispatch under this lock.
use std::collections::HashMap;
use tokio::sync::oneshot;
use vosu_plugin_api::{Message, ResourceResolved, StreamChunk, VOSU_STREAM_CHUNK_MAX_BYTES};

use super::super::host_services::PluginInstanceId;
use super::PluginHost;
use std::sync::Weak;
use std::time::Duration;

/// Captured live authority. Root updates revoke previously captured generations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceOwner {
    pub instance: PluginInstanceId,
    pub handle: u64,
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub enum ResourceRequest {
    Resolve(vosu_plugin_api::ResourceResolve),
    List(vosu_plugin_api::TreeList),
    Lookup(vosu_plugin_api::TreeLookup),
    Read(vosu_plugin_api::StreamRead),
}

impl ResourceRequest {
    pub(super) fn message(self, owner: ResourceOwner) -> Result<Message, ResourceRequestError> {
        Ok(match self {
            Self::Resolve(r) if r.handle == owner.handle => Message::ResourceResolve(r),
            Self::List(r) if r.handle == owner.handle => Message::TreeList(r),
            Self::Lookup(r) if r.handle == owner.handle => Message::TreeLookup(r),
            Self::Read(r) => Message::StreamRead(r),
            _ => return Err(ResourceRequestError::InvalidRequest),
        })
    }
}

/// Weak host seam for R2/R3; no worker, queue or task exists until requested.
#[derive(Clone)]
pub struct ResourceRequests {
    pub(super) host: Weak<PluginHost>,
    pub(super) deadline: Duration,
}

impl ResourceRequests {
    pub async fn request(
        &self,
        owner: ResourceOwner,
        request: ResourceRequest,
    ) -> Result<ResourceReply, ResourceRequestError> {
        let host = self
            .host
            .upgrade()
            .ok_or(ResourceRequestError::TargetGone)?;
        match host.resource_request(owner, request, self.deadline).await? {
            ResourceReply::Resolved(ResourceResolved::Error { error })
            | ResourceReply::Chunk(StreamChunk::Error { error }) => {
                Err(match error.code.as_str() {
                    "gone" => ResourceRequestError::Gone,
                    "not_found" => ResourceRequestError::NotFound,
                    _ => ResourceRequestError::ProviderFailed,
                })
            }
            reply => Ok(reply),
        }
    }
}

pub const MAX_PENDING_RESOURCE_REQUESTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceRequestError {
    InvalidRequest,
    InvalidReply,
    /// The provider confirmed that the published representation is no longer valid.
    Gone,
    NotFound,
    ProviderFailed,
    TargetGone,
    Cancelled,
    Busy,
    TimedOut,
    DispatchFailed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResourceReply {
    Resolved(ResourceResolved),
    Chunk(StreamChunk),
    Stream {
        lease: crate::plugin::StreamLease,
        entry_path: Option<String>,
    },
}

#[derive(Clone, Copy)]
enum Expected {
    Resource,
    List,
    Lookup,
    Read(u32),
}

impl Expected {
    fn request(message: &Message) -> Option<(Self, Option<u64>)> {
        Some(match message {
            Message::ResourceResolve(r) => (Self::Resource, Some(r.handle)),
            Message::TreeList(r) => (Self::List, Some(r.handle)),
            Message::TreeLookup(r) => (Self::Lookup, Some(r.handle)),
            Message::StreamRead(r)
                if r.len <= VOSU_STREAM_CHUNK_MAX_BYTES
                    && r.offset.checked_add(u64::from(r.len)).is_some() =>
            {
                (Self::Read(r.len), None)
            }
            _ => return None,
        })
    }

    fn accepts(self, reply: &ResourceReply) -> bool {
        match (self, reply) {
            (_, ResourceReply::Resolved(ResourceResolved::Error { .. })) => {
                !matches!(self, Self::Read(_))
            }
            (
                Self::Resource,
                ResourceReply::Resolved(
                    ResourceResolved::Path { .. } | ResourceResolved::Stream { .. },
                ),
            )
            | (Self::List, ResourceReply::Resolved(ResourceResolved::Tree { .. }))
            | (
                Self::Lookup,
                ResourceReply::Resolved(
                    ResourceResolved::TreePath { .. } | ResourceResolved::TreeStream { .. },
                ),
            )
            | (Self::Read(_), ResourceReply::Chunk(StreamChunk::Error { .. })) => true,
            (Self::Read(len), ResourceReply::Chunk(StreamChunk::Data { data })) => {
                data.0.len() <= len as usize
            }
            _ => false,
        }
    }
}

struct Pending {
    instance: PluginInstanceId,
    handle: Option<u64>,
    generation: u64,
    expected: Expected,
    sender: oneshot::Sender<Result<ResourceReply, ResourceRequestError>>,
    stream: Option<(
        ResourceOwner,
        ResourceRequests,
        crate::plugin::outbox::OutboxSlot,
    )>,
}

#[derive(Default)]
pub(super) struct Requests {
    // Host-wide monotonic ids also prevent a replacement instance answering an old id.
    next: u64,
    entries: HashMap<u64, Pending>,
    streams: HashMap<(PluginInstanceId, u64), Weak<crate::plugin::resource_lease::LeaseInner>>,
}

impl Requests {
    pub fn contains(&self, id: u64) -> bool {
        self.entries.contains_key(&id)
    }

    pub fn dispatch_failed(&mut self, id: u64) {
        if let Some(pending) = self.entries.remove(&id) {
            let _ = pending
                .sender
                .send(Err(ResourceRequestError::DispatchFailed));
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn reserve(
        &mut self,
        instance: PluginInstanceId,
        generation: u64,
        message: &Message,
    ) -> Result<
        (
            u64,
            oneshot::Receiver<Result<ResourceReply, ResourceRequestError>>,
        ),
        ResourceRequestError,
    > {
        let (expected, handle) =
            Expected::request(message).ok_or(ResourceRequestError::InvalidRequest)?;
        if self
            .entries
            .values()
            .filter(|p| p.instance == instance)
            .count()
            >= MAX_PENDING_RESOURCE_REQUESTS
        {
            return Err(ResourceRequestError::Busy);
        }
        self.next = self.next.checked_add(1).ok_or(ResourceRequestError::Busy)?;
        let (sender, receiver) = oneshot::channel();
        self.entries.insert(
            self.next,
            Pending {
                instance,
                generation,
                handle,
                expected,
                sender,
                stream: None,
            },
        );
        Ok((self.next, receiver))
    }

    pub fn scope(&mut self, id: u64, handle: u64) {
        if let Some(pending) = self.entries.get_mut(&id) {
            pending.handle = Some(handle);
        }
    }

    pub fn arm_stream(
        &mut self,
        id: u64,
        owner: ResourceOwner,
        requests: ResourceRequests,
        slot: crate::plugin::outbox::OutboxSlot,
    ) {
        if let Some(pending) = self.entries.get_mut(&id) {
            pending.stream = Some((owner, requests, slot));
        }
    }

    pub fn read_allowed(&self, owner: ResourceOwner, stream: u64) -> bool {
        self.streams
            .get(&(owner.instance, stream))
            .and_then(Weak::upgrade)
            .is_some_and(|lease| lease.owner == owner && lease.is_current())
    }

    pub fn stream_is_live(&self, instance: PluginInstanceId, stream: u64) -> bool {
        self.streams
            .get(&(instance, stream))
            .and_then(Weak::upgrade)
            .is_some_and(|lease| lease.is_current())
    }

    pub fn complete(
        &mut self,
        instance: PluginInstanceId,
        generation: u64,
        id: u64,
        reply: ResourceReply,
    ) -> Result<(), ResourceRequestError> {
        let pending = self
            .entries
            .get(&id)
            .ok_or(ResourceRequestError::Cancelled)?;
        if pending.instance != instance || pending.generation != generation {
            return Err(ResourceRequestError::Cancelled);
        }
        // A malformed reply terminates its own request, never an unrelated one.
        let valid = pending.expected.accepts(&reply);
        let mut pending = self.entries.remove(&id).expect("entry checked above");
        let result = if valid {
            let stream = match &reply {
                ResourceReply::Resolved(ResourceResolved::Stream { stream, size }) => {
                    Some((*stream, *size, None))
                }
                ResourceReply::Resolved(ResourceResolved::TreeStream {
                    stream,
                    size,
                    entry_path,
                }) => Some((*stream, *size, Some(entry_path.clone()))),
                _ => None,
            };
            if let Some((stream, size, entry_path)) = stream {
                self.streams
                    .retain(|_, lease| lease.upgrade().is_some_and(|lease| lease.is_current()));
                if self.streams.contains_key(&(instance, stream)) {
                    Err(ResourceRequestError::InvalidReply)
                } else if let Some((owner, requests, slot)) = pending.stream.take() {
                    let lease =
                        crate::plugin::StreamLease::new(owner, stream, size, requests, slot);
                    if self
                        .streams
                        .keys()
                        .filter(|(owner, _)| *owner == instance)
                        .count()
                        >= MAX_PENDING_RESOURCE_REQUESTS
                    {
                        lease.close();
                        Err(ResourceRequestError::Busy)
                    } else {
                        self.streams
                            .insert((instance, stream), std::sync::Arc::downgrade(&lease.0));
                        Ok(ResourceReply::Stream { lease, entry_path })
                    }
                } else {
                    Err(ResourceRequestError::InvalidReply)
                }
            } else {
                Ok(reply)
            }
        } else {
            Err(ResourceRequestError::InvalidReply)
        };
        let status = result.as_ref().map(|_| ()).map_err(|error| *error);
        let _ = pending.sender.send(result);
        status
    }

    pub fn remove(&mut self, id: u64) {
        self.entries.remove(&id);
    }

    pub fn cancel(&mut self, instance: PluginInstanceId, handle: Option<u64>) {
        self.entries.retain(|_, pending| {
            !(pending.instance == instance && handle.is_none_or(|h| pending.handle == Some(h)))
        });
        self.streams.retain(|(owner, _), lease| {
            if *owner == instance
                && let Some(lease) = lease.upgrade()
                && handle.is_none_or(|h| lease.owner.handle == h)
            {
                lease.close();
                return false;
            }
            lease.strong_count() != 0
        });
    }
}

#[cfg(test)]
mod tests;
