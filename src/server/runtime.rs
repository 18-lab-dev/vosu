//! Reloadable listener orchestration. Binding is separated from serving so the
//! application can wire axum, authorization, and WebSocket ownership without
//! coupling those implementations to rebind policy.

use std::{
    future::Future,
    io,
    net::{Shutdown, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{Notify, watch},
    task::{AbortHandle, JoinHandle, JoinSet},
    time,
};
use tokio_util::sync::CancellationToken;

use super::{
    routes::{self, PeerAddress, ResponseFlushBarrier, ResponseFlushRegistry, RouteProviders},
    websocket::UpgradeCallbackTracker,
};
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
pub const RETRY_DELAYS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];

pub type RuntimeFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListenTarget {
    pub host: String,
    pub port: u16,
}
impl ListenTarget {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveListener {
    pub target: ListenTarget,
    pub address: SocketAddr,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ListenerState {
    pub desired: Option<ListenTarget>,
    pub effective: Option<EffectiveListener>,
    pub last_error: Option<String>,
}

pub trait HostResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        target: &'a ListenTarget,
    ) -> RuntimeFuture<'a, io::Result<Vec<SocketAddr>>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TokioResolver;
impl HostResolver for TokioResolver {
    fn resolve<'a>(
        &'a self,
        target: &'a ListenTarget,
    ) -> RuntimeFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move {
            tokio::net::lookup_host((target.host.as_str(), target.port))
                .await
                .map(|it| it.collect())
        })
    }
}

/// Resolve and return the first address in resolver order. The original hostname
/// remains in `ListenTarget`; it is never replaced with a normalized IP string.
pub async fn resolve_first(
    resolver: &dyn HostResolver,
    target: &ListenTarget,
) -> io::Result<SocketAddr> {
    resolver
        .resolve(target)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "hostname resolved to no addresses",
            )
        })
}

pub trait BoundListener: Send + Sync {
    fn address(&self) -> SocketAddr;
    /// Stops accepting new HTTP/WebSocket connections.
    fn begin_drain(&self);
    /// Completes when all HTTP and WebSocket work has naturally finished.
    fn drained(&self) -> RuntimeFuture<'_, ()>;
    /// Cancels connections/tasks still alive after the drain deadline.
    fn cancel_survivors(&self);
}

pub trait ListenerBinder: Send + Sync {
    fn bind<'a>(
        &'a self,
        target: &'a ListenTarget,
        address: SocketAddr,
    ) -> RuntimeFuture<'a, io::Result<Arc<dyn BoundListener>>>;
}

/// Adapter for application wiring: supplies a bound `TcpListener` to the server
/// slice, which returns a handle owning its accept task and connection registry.
pub trait TcpListenerFactory: Send + Sync {
    fn start(
        &self,
        target: ListenTarget,
        listener: TcpListener,
    ) -> io::Result<Arc<dyn BoundListener>>;
}

pub struct TokioBinder<F>(pub F);
impl<F: TcpListenerFactory> ListenerBinder for TokioBinder<F> {
    fn bind<'a>(
        &'a self,
        target: &'a ListenTarget,
        address: SocketAddr,
    ) -> RuntimeFuture<'a, io::Result<Arc<dyn BoundListener>>> {
        Box::pin(async move {
            let listener = TcpListener::bind(address).await?;
            self.0.start(target.clone(), listener)
        })
    }
}

/// Production Axum listener factory. Axum owns the active HTTP connection set:
/// cancelling `accept_stop` closes the accept loop and starts graceful shutdown;
/// aborting the server task is reserved for `cancel_survivors` after the runtime
/// drain deadline.
#[derive(Clone)]
pub struct AxumListenerFactory {
    providers: RouteProviders,
}

impl AxumListenerFactory {
    pub fn new(providers: RouteProviders) -> Self {
        Self { providers }
    }
}
#[derive(Default)]
struct ConnectionRegistry {
    next_id: AtomicU64,
    sockets: Mutex<Vec<(u64, std::net::TcpStream)>>,
    changed: Notify,
}

impl ConnectionRegistry {
    fn sockets(&self) -> MutexGuard<'_, Vec<(u64, std::net::TcpStream)>> {
        match self.sockets.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn insert(&self, socket: std::net::TcpStream) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.sockets().push((id, socket));
        self.changed.notify_waiters();
        id
    }

    fn remove(&self, id: u64) {
        self.sockets().retain(|(candidate, _)| *candidate != id);
        self.changed.notify_waiters();
    }

    fn shutdown_all(&self) {
        for (_, socket) in self.sockets().iter() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    async fn wait_empty(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.sockets().is_empty() {
                return;
            }
            changed.await;
        }
    }
}

struct TrackedIo {
    stream: TcpStream,
    registry: Arc<ConnectionRegistry>,
    id: u64,
    peer: SocketAddr,
    flushes: Arc<ResponseFlushRegistry>,
    flush: Arc<ResponseFlushBarrier>,
}

impl Drop for TrackedIo {
    fn drop(&mut self) {
        self.flush.disconnect();
        self.flushes.remove(self.peer);
        self.registry.remove(self.id);
    }
}

impl AsyncRead for TrackedIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_read(cx, buffer)
    }
}

impl AsyncWrite for TrackedIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(cx, buffer)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.stream).poll_flush(cx);
        if matches!(result, Poll::Ready(Ok(()))) {
            self.flush.flushed();
        }
        result
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write_vectored(cx, buffers)
    }
}

struct TrackedListener {
    listener: TcpListener,
    registry: Arc<ConnectionRegistry>,
    flushes: Arc<ResponseFlushRegistry>,
}

impl axum::serve::Listener for TrackedListener {
    type Io = TrackedIo;
    type Addr = PeerAddress;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            if let Ok((stream, address)) = self.listener.accept().await
                && let Ok(stream) = stream.into_std()
                && let Ok(control) = stream.try_clone()
                && let Ok(stream) = TcpStream::from_std(stream)
            {
                let id = self.registry.insert(control);
                let flush = Arc::new(ResponseFlushBarrier::default());
                self.flushes.register(address, &flush);
                return (
                    TrackedIo {
                        stream,
                        registry: self.registry.clone(),
                        id,
                        peer: address,
                        flushes: self.flushes.clone(),
                        flush,
                    },
                    PeerAddress(address),
                );
            }
            time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.listener.local_addr().map(PeerAddress)
    }
}

impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, TrackedListener>>
    for PeerAddress
{
    fn connect_info(stream: axum::serve::IncomingStream<'_, TrackedListener>) -> Self {
        *stream.remote_addr()
    }
}

struct AxumBoundListener {
    address: SocketAddr,
    accept_stop: CancellationToken,
    completed: CancellationToken,
    abort: AbortHandle,
    connections: Arc<ConnectionRegistry>,
    upgrades: Arc<UpgradeCallbackTracker>,
}

impl BoundListener for AxumBoundListener {
    fn address(&self) -> SocketAddr {
        self.address
    }
    fn begin_drain(&self) {
        self.upgrades.seal();
        self.accept_stop.cancel();
    }
    fn drained(&self) -> RuntimeFuture<'_, ()> {
        Box::pin(async move {
            self.completed.cancelled().await;
            self.connections.wait_empty().await;
            self.upgrades.wait_drained().await;
        })
    }
    fn cancel_survivors(&self) {
        self.upgrades.cancel();
        self.connections.shutdown_all();
        self.abort.abort();
    }
}

struct CompleteOnDrop(CancellationToken);
impl Drop for CompleteOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl TcpListenerFactory for AxumListenerFactory {
    fn start(
        &self,
        _target: ListenTarget,
        listener: TcpListener,
    ) -> io::Result<Arc<dyn BoundListener>> {
        let address = listener.local_addr()?;
        let accept_stop = CancellationToken::new();
        let completed = CancellationToken::new();
        let completion = completed.clone();
        let shutdown = accept_stop.clone();
        let upgrades = Arc::new(UpgradeCallbackTracker::new());
        let flushes = Arc::new(ResponseFlushRegistry::default());
        let service = routes::service_with_upgrades(
            self.providers.clone(),
            upgrades.clone(),
            flushes.clone(),
        );
        let connections = Arc::new(ConnectionRegistry::default());
        let tracked = TrackedListener {
            listener,
            registry: connections.clone(),
            flushes,
        };
        let task = tokio::spawn(async move {
            let _complete = CompleteOnDrop(completion);
            let _ = axum::serve(tracked, service)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await;
        });
        Ok(Arc::new(AxumBoundListener {
            address,
            accept_stop,
            completed,
            connections,
            upgrades,
            abort: task.abort_handle(),
        }))
    }
}

pub type AxumListenerBinder = TokioBinder<AxumListenerFactory>;

pub struct ListenerRuntime {
    resolver: Arc<dyn HostResolver>,
    binder: Arc<dyn ListenerBinder>,
    state_tx: watch::Sender<ListenerState>,
    retry_delays: Arc<[Duration]>,
    drain_timeout: Duration,
}

impl ListenerRuntime {
    pub fn new(
        resolver: Arc<dyn HostResolver>,
        binder: Arc<dyn ListenerBinder>,
    ) -> (Self, watch::Receiver<ListenerState>) {
        Self::with_policy(resolver, binder, RETRY_DELAYS.into(), DRAIN_TIMEOUT)
    }

    pub fn with_policy(
        resolver: Arc<dyn HostResolver>,
        binder: Arc<dyn ListenerBinder>,
        retry_delays: Arc<[Duration]>,
        drain_timeout: Duration,
    ) -> (Self, watch::Receiver<ListenerState>) {
        let (state_tx, state_rx) = watch::channel(ListenerState::default());
        (
            Self {
                resolver,
                binder,
                state_tx,
                retry_delays,
                drain_timeout,
            },
            state_rx,
        )
    }

    pub async fn run(
        self,
        mut desired_rx: watch::Receiver<ListenTarget>,
        shutdown: CancellationToken,
    ) {
        let mut active: Option<(ListenTarget, Arc<dyn BoundListener>)> = None;
        let mut desired = desired_rx.borrow_and_update().clone();
        let mut retry_index = 0usize;
        let mut retry_now = true;
        let mut drains = JoinSet::new();

        'runtime: loop {
            if retry_now {
                self.publish(&desired, active.as_ref(), None);
                let attempted = desired.clone();
                let result = tokio::select! {
                    _ = shutdown.cancelled() => break 'runtime,
                    changed = desired_rx.changed() => {
                        if changed.is_err() { break 'runtime; }
                        desired = desired_rx.borrow_and_update().clone();
                        retry_index = 0;
                        retry_now = true;
                        continue 'runtime;
                    }
                    result = self.try_bind(&attempted, active.as_ref()) => result,
                };

                // A watch update can become ready in the same scheduler turn as
                // the bind. Never publish an obsolete listener in that race.
                match desired_rx.has_changed() {
                    Ok(true) => {
                        if let Ok(Some(obsolete)) = result {
                            spawn_drain(&mut drains, obsolete, self.drain_timeout);
                        }
                        desired = desired_rx.borrow_and_update().clone();
                        retry_index = 0;
                        retry_now = true;
                        continue 'runtime;
                    }
                    Err(_) => {
                        if let Ok(Some(obsolete)) = result {
                            spawn_drain(&mut drains, obsolete, self.drain_timeout);
                        }
                        break 'runtime;
                    }
                    Ok(false) => {}
                }

                match result {
                    Ok(Some(next)) => {
                        retry_index = 0;
                        let old = active.replace((attempted, next));
                        self.publish(&desired, active.as_ref(), None);
                        if let Some((_, old)) = old {
                            spawn_drain(&mut drains, old, self.drain_timeout);
                        }
                        retry_now = false;
                    }
                    Ok(None) => {
                        // The new hostname resolved to the socket already being
                        // served. Keep that listener and commit only its public
                        // target metadata so reporting/dashboard authority
                        // follows the requested hostname without an impossible
                        // second bind to the same address.
                        retry_index = 0;
                        if let Some((target, _)) = active.as_mut() {
                            *target = attempted;
                        }
                        self.publish(&desired, active.as_ref(), None);
                        retry_now = false;
                    }
                    Err(error) => {
                        self.publish(&desired, active.as_ref(), Some(error.to_string()));
                        retry_now = false;
                    }
                }
            }

            let delay = self
                .retry_delays
                .get(retry_index)
                .copied()
                .or_else(|| self.retry_delays.last().copied())
                .unwrap_or(Duration::from_secs(30));
            tokio::select! {
                _ = drains.join_next(), if !drains.is_empty() => {}
                _ = shutdown.cancelled() => break,
                changed = desired_rx.changed() => {
                    if changed.is_err() { break; }
                    let replacement = desired_rx.borrow_and_update().clone();
                    if replacement != desired { desired = replacement; retry_index = 0; retry_now = true; }
                }
                _ = time::sleep(delay), if !retry_now && active.as_ref().is_none_or(|(target, _)| target != &desired) => {
                    retry_index = retry_index.saturating_add(1).min(self.retry_delays.len().saturating_sub(1));
                    retry_now = true;
                }
            }
        }

        if let Some((_, listener)) = active {
            spawn_drain(&mut drains, listener, self.drain_timeout);
        }
        while drains.join_next().await.is_some() {}
        let _ = self.state_tx.send(ListenerState {
            desired: Some(desired),
            effective: None,
            last_error: None,
        });
    }

    async fn try_bind(
        &self,
        target: &ListenTarget,
        active: Option<&(ListenTarget, Arc<dyn BoundListener>)>,
    ) -> io::Result<Option<Arc<dyn BoundListener>>> {
        let address = resolve_first(self.resolver.as_ref(), target).await?;
        if active.is_some_and(|(_, listener)| listener.address() == address) {
            return Ok(None);
        }
        self.binder.bind(target, address).await.map(Some)
    }

    fn publish(
        &self,
        desired: &ListenTarget,
        active: Option<&(ListenTarget, Arc<dyn BoundListener>)>,
        error: Option<String>,
    ) {
        let effective = active.map(|(target, listener)| EffectiveListener {
            target: target.clone(),
            address: listener.address(),
        });
        let _ = self.state_tx.send(ListenerState {
            desired: Some(desired.clone()),
            effective,
            last_error: error,
        });
    }
}

fn spawn_drain(drains: &mut JoinSet<()>, listener: Arc<dyn BoundListener>, timeout: Duration) {
    drains.spawn(async move { drain(listener, timeout).await });
}
async fn drain(listener: Arc<dyn BoundListener>, timeout: Duration) {
    listener.begin_drain();
    if time::timeout(timeout, listener.drained()).await.is_err() {
        listener.cancel_survivors();
    }
}

/// Installs SIGINT and SIGTERM cancellation without owning application shutdown.
pub fn install_signal_cancellation(shutdown: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
            tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        shutdown.cancel();
    })
}

#[cfg(test)]
mod tests;
