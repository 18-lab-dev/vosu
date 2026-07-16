use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use axum::{
    extract::{
        FromRequestParts, Request,
        ws::{WebSocket, WebSocketUpgrade},
    },
    response::{IntoResponse, Response},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::Endpoint;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Result of the HTTP runtime's pre-upgrade gate. The runtime must derive
/// `authorized` from the shared authorization predicate using the actual peer
/// socket address and normalized Host/Origin/Referer headers. It must apply
/// CORS headers to either rejection and call `on_upgrade` only for `Upgrade`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpgradeDecision {
    Upgrade(Endpoint),
    Reject { status: u16, body: &'static str },
}

pub fn decide_upgrade(path: &str, authorized: bool) -> UpgradeDecision {
    if !authorized {
        return UpgradeDecision::Reject {
            status: 403,
            body: "Not Found",
        };
    }
    match Endpoint::classify(path) {
        Some(endpoint) => UpgradeDecision::Upgrade(endpoint),
        None => UpgradeDecision::Reject {
            status: 404,
            body: "Not Found",
        },
    }
}

pub struct UpgradeCallbackTracker {
    accepting: AtomicBool,
    active: AtomicUsize,
    cancellation: CancellationToken,
    drained: Notify,
}

pub(super) struct UpgradeRegistration {
    tracker: Arc<UpgradeCallbackTracker>,
    completed: AtomicBool,
}

impl UpgradeCallbackTracker {
    pub fn new() -> Self {
        Self {
            accepting: AtomicBool::new(true),
            active: AtomicUsize::new(0),
            cancellation: CancellationToken::new(),
            drained: Notify::new(),
        }
    }

    pub fn seal(&self) {
        self.accepting.store(false, Ordering::Release);
    }

    pub fn cancel(&self) {
        self.seal();
        self.cancellation.cancel();
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    pub async fn wait_drained(&self) {
        loop {
            let notified = self.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.active() == 0 {
                return;
            }
            notified.await;
        }
    }

    pub(super) fn register(self: &Arc<Self>) -> Option<Arc<UpgradeRegistration>> {
        if !self.accepting.load(Ordering::Acquire) {
            return None;
        }
        self.active.fetch_add(1, Ordering::AcqRel);
        if !self.accepting.load(Ordering::Acquire) {
            self.complete_one();
            return None;
        }
        Some(Arc::new(UpgradeRegistration {
            tracker: Arc::clone(self),
            completed: AtomicBool::new(false),
        }))
    }

    fn complete_one(&self) {
        if self.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.drained.notify_waiters();
        }
    }
}

impl Default for UpgradeCallbackTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl UpgradeRegistration {
    fn complete(&self) {
        if !self.completed.swap(true, Ordering::AcqRel) {
            self.tracker.complete_one();
        }
    }

    pub(super) async fn run<F>(self: Arc<Self>, future: F)
    where
        F: Future<Output = ()>,
    {
        let cancellation = self.tracker.cancellation.clone();
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => {},
            _ = future => {},
        }
        self.complete();
    }
}

impl Drop for UpgradeRegistration {
    fn drop(&mut self) {
        self.complete();
    }
}

/// Extracts and configures Axum's actual RFC 6455 handshake after routing and
/// authorization have selected an exact endpoint.
pub async fn upgrade_response<F, Fut>(
    endpoint: Endpoint,
    request: Request,
    tracker: Arc<UpgradeCallbackTracker>,
    on_socket: F,
) -> Response
where
    F: FnOnce(Endpoint, WebSocket) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (mut parts, _) = request.into_parts();
    match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => {
            // Register before `on_upgrade`: Axum spawns a task which may wait
            // on Hyper's upgrade future before invoking our callback. Listener
            // draining must own that pending task too, not briefly observe
            // `active == 0` after the 101 response has been constructed.
            let Some(registration) = tracker.register() else {
                return Response::builder()
                    .status(503)
                    .body(axum::body::Body::from("Not Found"))
                    .expect("valid rejection response");
            };
            upgrade
                .max_frame_size(MAX_FRAME_BYTES)
                .max_message_size(MAX_MESSAGE_BYTES)
                .on_upgrade(move |socket| async move {
                    let handler = on_socket(endpoint, socket);
                    registration.run(handler).await;
                })
                .into_response()
        }
        Err(rejection) => rejection.into_response(),
    }
}
