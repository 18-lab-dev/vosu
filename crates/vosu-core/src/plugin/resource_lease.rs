//! Request-owned stream authority. Close is idempotent and capacity-reserved.
use crate::plugin::{OwnedMessage, outbox::OutboxSlot};
use crate::plugin::{ResourceOwner, ResourceRequests};
use std::sync::{Arc, Mutex};
use vosu_plugin_api::{Message, StreamClose};

pub(crate) struct LeaseInner {
    pub owner: ResourceOwner,
    pub stream: u64,
    pub size: u64,
    pub requests: ResourceRequests,
    close: Mutex<Option<OutboxSlot>>,
}
impl LeaseInner {
    pub fn is_current(&self) -> bool {
        self.close
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }
    pub fn close(&self) {
        let slot = self.close.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(slot) = slot {
            // Fixed-size codec shape cannot exceed any frame limit.
            let message = OwnedMessage::from_message(
                &Message::StreamClose(StreamClose {
                    stream: self.stream,
                }),
                0,
            )
            .expect("encoding a stream close");
            drop(slot.send(message));
        }
    }
}
impl Drop for LeaseInner {
    fn drop(&mut self) {
        self.close();
    }
}

/// A distinct resolve owns each lease; clones retain that same request's lifetime.
#[derive(Clone)]
pub struct StreamLease(pub(crate) Arc<LeaseInner>);
impl std::fmt::Debug for StreamLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamLease")
            .field("stream", &self.0.stream)
            .field("size", &self.0.size)
            .finish_non_exhaustive()
    }
}
impl PartialEq for StreamLease {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl StreamLease {
    pub(crate) fn new(
        owner: ResourceOwner,
        stream: u64,
        size: u64,
        requests: ResourceRequests,
        close: OutboxSlot,
    ) -> Self {
        Self(Arc::new(LeaseInner {
            owner,
            stream,
            size,
            requests,
            close: Mutex::new(Some(close)),
        }))
    }
    pub fn size(&self) -> u64 {
        self.0.size
    }
    pub fn is_current(&self) -> bool {
        self.0.is_current()
    }
    pub fn close(&self) {
        self.0.close();
    }
}
