//! Bundled route descriptors and bounded provider messages; transport policy stays in core.
use crate::listener::ListenerRole;
use bytes::Bytes;
use std::{future::Future, pin::Pin, sync::Arc};
pub type ResponseFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub const MAX_HEADERS: usize = 32;
pub const MAX_METADATA_BYTES: usize = 16 * 1024;
pub const MAX_QUERY_PAIRS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoutePath {
    Exact(String),
    /// Each `{name}` matches exactly one nonempty path segment.
    Template(String),
    /// A segment-boundary prefix, including the prefix itself.
    Prefix(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteDescriptor {
    pub listener: ListenerRole,
    pub method: String,
    pub path: RoutePath,
    pub websocket: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationError {
    UnknownRequester,
    InvalidDescriptor,
    Collision,
    Capacity,
}

/// Already admitted request. Query pairs are decoded once; credentials never cross this seam.
pub struct SurfaceRequest {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Arc<[u8]>,
    pub context: Arc<crate::state::CapturedSelection>,
    pub management: Option<crate::management::ManagementProvenance>,
}
impl SurfaceRequest {
    pub fn valid_metadata(&self) -> bool {
        self.query.len() <= MAX_QUERY_PAIRS
            && self.headers.len() <= MAX_HEADERS
            && self
                .query
                .iter()
                .chain(&self.headers)
                .map(|(k, v)| k.len().saturating_add(v.len()))
                .sum::<usize>()
                <= MAX_METADATA_BYTES
            && self.headers.iter().all(|(name, value)| {
                matches!(
                    name.as_str(),
                    "range" | "if-none-match" | "if-modified-since" | "if-range"
                ) && !value.contains(['\r', '\n'])
            })
    }
}
pub enum ResponseBody {
    Empty,
    Bytes(Bytes),
    /// A same-open-object R0 lease, streamed in bounded chunks by the transport.
    File {
        file: crate::files::OpenedFile,
        offset: u64,
        length: u64,
    },
}
pub struct SurfaceResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: ResponseBody,
}
impl SurfaceResponse {
    pub fn valid_metadata(&self) -> bool {
        (100..=599).contains(&self.status)
            && self.headers.len() <= MAX_HEADERS
            && self
                .headers
                .iter()
                .map(|(k, v)| k.len().saturating_add(v.len()))
                .sum::<usize>()
                <= MAX_METADATA_BYTES
            && self.headers.iter().all(|(name, value)| {
                matches!(
                    name.as_str(),
                    "content-type"
                        | "content-length"
                        | "content-range"
                        | "accept-ranges"
                        | "etag"
                        | "last-modified"
                        | "cache-control"
                        | "location"
                        | "content-security-policy"
                ) && !value.contains(['\r', '\n'])
            })
            && match &self.body {
                ResponseBody::File {
                    file,
                    offset,
                    length,
                } => offset
                    .checked_add(*length)
                    .is_some_and(|end| end <= file.metadata.size),
                _ => true,
            }
    }
}
pub enum ProviderResponse {
    Http(SurfaceResponse),
    Feed(SurfaceFeed),
}
pub trait RouteProvider: Send + Sync + 'static {
    fn respond(&self, request: SurfaceRequest) -> ResponseFuture<'_, ProviderResponse>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outbound {
    /// One in-flight state frame; acknowledge only after transport write completion.
    Text(Bytes),
    Reply(Bytes),
    Binary(Bytes),
    Pong(Bytes),
    Close {
        code: u16,
        reason: &'static str,
    },
}
#[derive(Clone, Debug, PartialEq)]
pub enum Inbound {
    Text { data: Bytes, finished: bool },
    Binary { data: Bytes, finished: bool },
    Continuation { data: Bytes, finished: bool },
    Ping(Bytes),
    Pong(Bytes),
    Close,
    Error,
}
/// Connection-owned demand and H0's latest-only/write-ack contract.
pub trait FeedControl: Send + Sync {
    fn id(&self) -> u64;
    fn receive(&self, frame: Inbound) -> ResponseFuture<'_, Option<Bytes>>;
    /// Bounded legacy filter JSON; rejection must retain the prior plan/demand.
    fn replace_filters(&self, filters: Bytes) -> Result<(), String>;
    fn write_completed(&self);
    fn send_text(&self, bytes: Bytes) -> ResponseFuture<'_, bool>;
    fn try_send_text(&self, bytes: Bytes) -> bool;
    fn close(&self);
    fn is_closed(&self) -> bool;
}
pub trait FeedReceiver: Send {
    fn recv(&mut self) -> ResponseFuture<'_, Option<Outbound>>;
}
pub struct SurfaceFeed {
    pub state: Arc<dyn FeedControl>,
    pub receiver: Box<dyn FeedReceiver>,
}
#[must_use = "dropping this unregisters every route and feed"]
pub struct RegistrationLease {
    _release: Box<dyn Send + Sync>,
}
impl RegistrationLease {
    pub fn new(release: impl Send + Sync + 'static) -> Self {
        Self {
            _release: Box::new(release),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn response_metadata_rejects_credentials_injection_and_excess() {
        let mut response = SurfaceResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: ResponseBody::Bytes(Bytes::from_static(b"42")),
        };
        assert!(response.valid_metadata());
        response.headers = vec![("set-cookie".into(), "secret".into())];
        assert!(!response.valid_metadata());
        response.headers = vec![(
            "content-type".into(),
            "text/plain\r\nset-cookie: secret".into(),
        )];
        assert!(!response.valid_metadata());
        response.headers = vec![("content-type".into(), "x".repeat(MAX_METADATA_BYTES + 1))];
        assert!(!response.valid_metadata());
        response.headers = vec![("etag".into(), "x".into()); MAX_HEADERS + 1];
        assert!(!response.valid_metadata());
    }
}
