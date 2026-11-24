//! Bundled route descriptors and bounded, immutable provider messages.
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenerRole {
    Overlay,
    Dashboard,
}

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
    /// WebSocket admission is explicit; it still collides with HTTP GET.
    pub websocket: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistrationError {
    UnknownRequester,
    InvalidDescriptor,
    Collision,
    Capacity,
}

/// Request material is admitted and bounded by the transport before dispatch.
/// No credentials, configuration access or caller-supplied management flag.
pub struct SurfaceRequest {
    pub method: String,
    pub path: String,
    pub body: Arc<[u8]>,
    pub context: Arc<crate::state::CapturedSelection>,
    pub management: Option<crate::management::ManagementProvenance>,
}

pub struct SurfaceResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Arc<[u8]>,
}

pub enum ProviderResponse {
    Http(SurfaceResponse),
    Feed(SurfaceFeed),
}

pub trait RouteProvider: Send + Sync + 'static {
    fn respond(&self, request: SurfaceRequest) -> ProviderResponse;
}

pub trait FeedEncoder: Send + Sync + 'static {
    fn encode(&self, update: &crate::state::SurfaceUpdate) -> SurfaceResponse;
}

/// Core retains weak cancellation handles; a closed feed drops its exact demand.
pub struct SurfaceFeed {
    pub state: Arc<FeedState>,
    pub encoder: Arc<dyn FeedEncoder>,
}

pub struct FeedState {
    subscription: std::sync::Mutex<Option<crate::state::SurfaceSubscription>>,
}

impl SurfaceFeed {
    pub fn new(
        subscription: crate::state::SurfaceSubscription,
        encoder: Arc<dyn FeedEncoder>,
    ) -> Self {
        Self {
            state: Arc::new(FeedState {
                subscription: std::sync::Mutex::new(Some(subscription)),
            }),
            encoder,
        }
    }
}

impl FeedState {
    pub fn updates(
        &self,
    ) -> Option<tokio::sync::watch::Receiver<Arc<crate::state::SurfaceUpdate>>> {
        self.subscription
            .lock()
            .unwrap()
            .as_ref()
            .map(|subscription| subscription.updates.clone())
    }

    pub fn close(&self) {
        let subscription = self.subscription.lock().unwrap().take();
        drop(subscription);
    }
}

#[must_use = "dropping this unregisters every route and feed"]
pub struct RegistrationLease {
    _release: Box<dyn Send + Sync>,
}
impl RegistrationLease {
    /// Host implementation seam; adapters obtain leases from their requester.
    pub fn new(release: impl Send + Sync + 'static) -> Self {
        Self {
            _release: Box::new(release),
        }
    }
}
