//! Independent API v1 connections. No task or producer timer exists without a client.
//! Client locks protect only protocol state; source calls always happen after unlocking.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{Notify, oneshot, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use vosu_core::api::{ApiError, ApiPlan, ApiStateSource, ApiUpdate, SessionTarget};
use vosu_core::state::LaneBits;

use super::selection::{MAX_FRAME, frame};
use crate::server::subscription_wire::{self as wire, Mailbox, Outbound, Receiver};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

struct Hub {
    source: ApiStateSource,
    debounce: watch::Sender<Duration>,
    shutdown: CancellationToken,
}
impl Drop for Hub {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

#[derive(Clone)]
pub struct ApiV1SubscriptionHub(Arc<Hub>);

struct Client {
    request: Mutex<Option<oneshot::Sender<ApiPlan>>>,
    established: AtomicBool,
    writing: AtomicBool,
    mailbox: Arc<Mailbox>,
    wake: Notify,
    closed: CancellationToken,
}
impl Client {
    fn finish(&self, code: u16, error: Option<(u16, &'static str)>) {
        let mut request = lock(&self.request);
        if self.closed.is_cancelled() {
            return;
        }
        if let Some((code, message)) = error {
            self.mailbox
                .try_send_terminal(Outbound::Error(wire::error_frame(code, message)));
        }
        self.mailbox.try_send_terminal(Outbound::Close { code });
        self.closed.cancel();
        request.take();
        self.mailbox.close_senders();
    }
    fn close(&self, code: u16) {
        self.finish(code, None);
    }
    fn error(&self, code: u16, message: &'static str) {
        self.finish(1008, Some((code, message)));
    }
}

pub struct ApiV1SubscriptionConnection {
    client: Arc<Client>,
}

impl ApiV1SubscriptionHub {
    pub fn new(source: ApiStateSource, debounce: Duration) -> Self {
        Self(Arc::new(Hub {
            source,
            debounce: watch::Sender::new(debounce),
            shutdown: CancellationToken::new(),
        }))
    }

    /// Validate upgrade before creating protocol state. This read creates no demand.
    pub fn connect(
        &self,
        target: SessionTarget,
    ) -> Result<(ApiV1SubscriptionConnection, Receiver), ApiError> {
        self.0.source.session(&target)?;
        Ok(self.open(Some(target)))
    }

    pub fn set_debounce(&self, debounce: Duration) {
        self.0.debounce.send_replace(debounce);
    }
    pub fn shutdown(&self) {
        self.0.shutdown.cancel();
    }

    pub(super) fn open(
        &self,
        target: Option<SessionTarget>,
    ) -> (ApiV1SubscriptionConnection, Receiver) {
        let (send, request) = oneshot::channel();
        let mailbox = Arc::new(Mailbox::new());
        let client = Arc::new(Client {
            request: Mutex::new(Some(send)),
            established: AtomicBool::new(false),
            writing: AtomicBool::new(false),
            mailbox: Arc::clone(&mailbox),
            wake: Notify::new(),
            closed: CancellationToken::new(),
        });
        tokio::spawn(establish(
            Arc::downgrade(&self.0),
            Arc::downgrade(&client),
            target,
            request,
            Instant::now() + REQUEST_TIMEOUT,
        ));
        (
            ApiV1SubscriptionConnection { client },
            Receiver::new(mailbox),
        )
    }
}

impl ApiV1SubscriptionConnection {
    pub fn text(&self, data: &[u8]) {
        if self.client.closed.is_cancelled() {
            return;
        }
        if self.client.established.swap(true, Ordering::AcqRel) {
            self.client
                .error(wire::ALREADY_SUBSCRIBED, wire::ALREADY_SUBSCRIBED_MESSAGE);
            return;
        }
        if data.len() > MAX_FRAME {
            self.close(1009);
            return;
        }
        let pointers = match wire::parse_subscription_request(data) {
            Ok(pointers) => pointers,
            Err(error) => {
                self.client.error(error.code(), error.message());
                return;
            }
        };
        // Preserve shared wire error classification while core validates demand.
        let plan = ApiPlan::new(&pointers);
        if let Err(error) = wire::Selection::compile(pointers) {
            self.client.error(error.code(), error.message());
            return;
        }
        let plan = match plan {
            Ok(plan) => plan,
            Err(_) => {
                self.client
                    .error(wire::INVALID_POINTER, wire::INVALID_POINTER_MESSAGE);
                return;
            }
        };
        if let Some(send) = lock(&self.client.request).take() {
            let _ = send.send(plan);
        }
    }

    pub fn ping(&self, data: Bytes) {
        if !self.client.closed.is_cancelled()
            && !self.client.mailbox.try_send_control(Outbound::Pong(data))
        {
            self.close(1011);
        }
    }
    /// Call after each Text frame has actually been written, not when dequeued.
    pub fn write_completed(&self) {
        self.client.writing.store(false, Ordering::Release);
        self.client.wake.notify_one();
    }
    pub fn close(&self, code: u16) {
        self.client.close(code);
    }
    pub fn unsupported_data(&self) {
        self.close(1003);
    }
}
impl Drop for ApiV1SubscriptionConnection {
    fn drop(&mut self) {
        self.client.close(1000);
    }
}

async fn establish(
    hub: Weak<Hub>,
    client: Weak<Client>,
    target: Option<SessionTarget>,
    request: oneshot::Receiver<ApiPlan>,
    deadline: Instant,
) {
    let Some(owner) = hub.upgrade() else {
        if let Some(c) = client.upgrade() {
            c.close(1001);
        }
        return;
    };
    let shutdown = owner.shutdown.clone();
    drop(owner);
    let Some(plan) = await_plan(&client, &shutdown, request, deadline).await else {
        return;
    };
    let Some(c) = client.upgrade() else {
        return;
    };
    let Some(owner) = hub.upgrade() else {
        c.close(1001);
        return;
    };
    let subscription = match target {
        Some(target) => owner.source.subscribe(target, plan),
        None => owner.source.subscribe_sessions(plan),
    };
    let debounce = owner.debounce.subscribe();
    drop(owner);
    let mut subscription = match subscription {
        Ok(subscription) => subscription,
        Err(ApiError::UnknownSession) => {
            c.error(6, "unknown session");
            return;
        }
        Err(ApiError::Closed) => {
            c.close(1001);
            return;
        }
        Err(_) => {
            c.close(1011);
            return;
        }
    };
    drop(c);
    stream(client, &mut subscription.updates, debounce, shutdown).await;
    // The RAII registration drops on every exit, including cancel and shutdown.
}

async fn await_plan(
    client: &Weak<Client>,
    shutdown: &CancellationToken,
    request: oneshot::Receiver<ApiPlan>,
    deadline: Instant,
) -> Option<ApiPlan> {
    let closed = client.upgrade()?.closed.clone();
    let request = tokio::select! {
        biased;
        () = closed.cancelled() => return None,
        () = shutdown.cancelled() => { if let Some(c) = client.upgrade() { c.close(1001); } return None; }
        result = tokio::time::timeout_at(deadline, request) => result,
    };
    match request {
        Ok(Ok(plan)) => Some(plan),
        Ok(Err(_)) => None,
        Err(_) => {
            if let Some(c) = client.upgrade() {
                c.error(
                    wire::SUBSCRIPTION_TIMEOUT,
                    wire::SUBSCRIPTION_TIMEOUT_MESSAGE,
                );
            }
            None
        }
    }
}

fn lanes(bits: LaneBits) -> [bool; 4] {
    [
        bits.regular_always,
        bits.precise_always,
        bits.regular_gameplay,
        bits.precise_gameplay,
    ]
}

async fn stream(
    client: Weak<Client>,
    updates: &mut watch::Receiver<Arc<ApiUpdate>>,
    mut debounce: watch::Receiver<Duration>,
    shutdown: CancellationToken,
) {
    let mut baseline: Option<Arc<ApiUpdate>> = None;
    let mut pending = Some(Arc::clone(&updates.borrow_and_update()));
    let mut last = [None; 4];
    let mut revision = 0u64;
    loop {
        let Some(c) = client.upgrade() else {
            return;
        };
        if c.closed.is_cancelled() {
            return;
        }
        if !c.writing.load(Ordering::Acquire) && pending.is_some() {
            // A completion can race a publication after the select woke. Capture the
            // latest immutable value at dispatch, rather than send an older pending one.
            let latest = updates.borrow_and_update();
            if pending
                .as_ref()
                .is_some_and(|old| !Arc::ptr_eq(old, &latest))
            {
                pending = Some(Arc::clone(&latest));
            }
        }
        let mut deadline = None;
        if !c.writing.load(Ordering::Acquire)
            && let Some(current) = pending.as_ref()
        {
            if let Some(error) = &current.error {
                c.close(if matches!(error, ApiError::Closed) {
                    1001
                } else {
                    1011
                });
                return;
            }
            let initial = baseline.as_ref().is_none_or(|old| {
                old.binding != current.binding && current.session.is_some() && !current.ended
            });
            let active = lanes(current.lanes);
            if !initial && !current.ended {
                let duration = *debounce.borrow();
                deadline = active
                    .iter()
                    .zip(last)
                    .filter_map(|(active, stamp)| {
                        if *active {
                            stamp.and_then(|stamp: Instant| stamp.checked_add(duration))
                        } else {
                            None
                        }
                    })
                    .max()
                    .filter(|stamp| *stamp > Instant::now());
            }
            if deadline.is_none() {
                let old = if initial {
                    None
                } else {
                    baseline.as_ref().map(|old| old.values.as_slice())
                };
                match frame(revision + 1, old, &current.values) {
                    Ok(Some(bytes)) => {
                        c.writing.store(true, Ordering::Release);
                        if c.mailbox.try_send_data(Outbound::Text(bytes)).is_err() {
                            c.close(1011);
                            return;
                        }
                        revision += 1;
                        let now = Instant::now();
                        for (active, stamp) in active.into_iter().zip(&mut last) {
                            if active {
                                *stamp = Some(now);
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {
                        c.error(
                            wire::SERIALIZATION_FAILED,
                            wire::SERIALIZATION_FAILED_MESSAGE,
                        );
                        return;
                    }
                }
                baseline = pending.take();
            }
        }
        if baseline.as_ref().is_some_and(|old| old.ended) && !c.writing.load(Ordering::Acquire) {
            c.close(1000);
            return;
        }
        tokio::select! {
            biased;
            () = c.closed.cancelled() => return,
            () = shutdown.cancelled() => { c.close(1001); return; }
            changed = updates.changed(), if !baseline.as_ref().is_some_and(|old| old.ended)
                && !pending.as_ref().is_some_and(|latest| latest.ended) => {
                if changed.is_err() { c.close(1001); return; }
                pending = Some(Arc::clone(&updates.borrow_and_update()));
            }
            () = c.wake.notified() => {}
            changed = debounce.changed() => { if changed.is_err() { c.close(1001); return; } }
            () = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; } else { std::future::pending::<()>().await; } } => {}
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
