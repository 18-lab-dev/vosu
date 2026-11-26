use super::super::{
    auth::AuthorizationPolicy,
    routes::{
        CachedJson, CalculatePpProvider, CalculateResult, Endpoint, ProviderFuture, RouteDelegate,
        StateJsonProvider, StateRoute,
    },
};
use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, Notify},
};
use vosu_proptest::bounded;

struct ProductionState;
impl StateJsonProvider for ProductionState {
    fn cached_json(&self, _: StateRoute) -> ProviderFuture<'_, CachedJson> {
        Box::pin(async { CachedJson::Ready(bytes::Bytes::from_static(br#"{"ok":true}"#)) })
    }
}
struct HangingState;
impl StateJsonProvider for HangingState {
    fn cached_json(&self, _: StateRoute) -> ProviderFuture<'_, CachedJson> {
        Box::pin(std::future::pending())
    }
}
struct ProductionCalculation;
impl CalculatePpProvider for ProductionCalculation {
    fn calculate_pp(&self, _: Option<&str>) -> ProviderFuture<'_, CalculateResult> {
        Box::pin(async { CalculateResult::NotReady })
    }
}
struct ProductionDelegate;
impl RouteDelegate for ProductionDelegate {
    fn handle(
        &self,
        _: Endpoint,
        _: axum::extract::Request,
        _: bytes::Bytes,
    ) -> ProviderFuture<'_, axum::response::Response> {
        Box::pin(async { axum::response::Response::new(axum::body::Body::empty()) })
    }
}
fn production_providers(state: Arc<dyn StateJsonProvider>) -> RouteProviders {
    RouteProviders {
        authorization: Arc::new(AuthorizationPolicy::new("127.0.0.1,localhost", "localhost")),
        state,
        calculation: Arc::new(ProductionCalculation),
        delegate: Arc::new(ProductionDelegate),
    }
}
async fn http_get(address: SocketAddr, path: &str) -> io::Result<Vec<u8>> {
    let mut stream = bounded!(TcpStream::connect(address))?;
    bounded!(stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes(),
    ))?;
    let mut response = Vec::new();
    bounded!(stream.read_to_end(&mut response))?;
    Ok(response)
}

struct Resolver(Vec<SocketAddr>);
impl HostResolver for Resolver {
    fn resolve<'a>(
        &'a self,
        _: &'a ListenTarget,
    ) -> RuntimeFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}
struct CancelGuard<'a>(&'a AtomicBool);
impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
struct RacingResolver {
    address: SocketAddr,
    started: Notify,
    cancelled: AtomicBool,
}
impl HostResolver for RacingResolver {
    fn resolve<'a>(
        &'a self,
        target: &'a ListenTarget,
    ) -> RuntimeFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move {
            if target.host == "slow.test" {
                let guard = CancelGuard(&self.cancelled);
                self.started.notify_one();
                std::future::pending::<()>().await;
                drop(guard);
            }
            Ok(vec![self.address])
        })
    }
}

struct Handle {
    address: SocketAddr,
    began: AtomicBool,
    cancelled: AtomicBool,
    hold: bool,
    released: CancellationToken,
}
impl BoundListener for Handle {
    fn address(&self) -> SocketAddr {
        self.address
    }
    fn begin_drain(&self) {
        self.began.store(true, Ordering::SeqCst);
    }
    fn drained(&self) -> RuntimeFuture<'_, ()> {
        Box::pin(async move {
            if self.hold {
                self.released.cancelled().await;
            }
        })
    }
    fn cancel_survivors(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.released.cancel();
    }
}
struct Binder {
    calls: AtomicUsize,
    fail_call: AtomicUsize,
    handles: Mutex<Vec<Arc<Handle>>>,
}
impl ListenerBinder for Binder {
    fn bind<'a>(
        &'a self,
        _: &'a ListenTarget,
        address: SocketAddr,
    ) -> RuntimeFuture<'a, io::Result<Arc<dyn BoundListener>>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if self.fail_call.load(Ordering::SeqCst) == call {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, "busy"));
            }
            let handle = Arc::new(Handle {
                address,
                began: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                hold: true,
                released: CancellationToken::new(),
            });
            self.handles.lock().await.push(handle.clone());
            Ok(handle as Arc<dyn BoundListener>)
        })
    }
}
struct RebindBinder {
    calls: AtomicUsize,
    handles: Mutex<Vec<Arc<Handle>>>,
}
impl ListenerBinder for RebindBinder {
    fn bind<'a>(
        &'a self,
        _: &'a ListenTarget,
        address: SocketAddr,
    ) -> RuntimeFuture<'a, io::Result<Arc<dyn BoundListener>>> {
        Box::pin(async move {
            let hold = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
            let handle = Arc::new(Handle {
                address,
                began: AtomicBool::new(false),
                cancelled: AtomicBool::new(false),
                hold,
                released: CancellationToken::new(),
            });
            self.handles.lock().await.push(handle.clone());
            Ok(handle as Arc<dyn BoundListener>)
        })
    }
}

struct PortResolver;
impl HostResolver for PortResolver {
    fn resolve<'a>(
        &'a self,
        target: &'a ListenTarget,
    ) -> RuntimeFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move { Ok(vec![SocketAddr::from(([127, 0, 0, 1], target.port))]) })
    }
}
async fn unused_port() -> u16 {
    let listener = bounded!(TcpListener::bind("127.0.0.1:0")).unwrap();
    listener.local_addr().unwrap().port()
}
async fn wait_effective(state: &mut watch::Receiver<ListenerState>, host: &str) -> SocketAddr {
    loop {
        if let Some(value) = state
            .borrow()
            .effective
            .as_ref()
            .filter(|value| value.target.host == host)
        {
            return value.address;
        }
        bounded!(state.changed()).unwrap();
    }
}

#[tokio::test]
async fn connection_registry_observes_the_last_connection_removal() {
    let listener = bounded!(TcpListener::bind("127.0.0.1:0")).unwrap();
    let address = listener.local_addr().unwrap();
    let connect = tokio::spawn(async move { bounded!(TcpStream::connect(address)).unwrap() });
    let (server, _) = bounded!(listener.accept()).unwrap();
    let client = bounded!(connect).unwrap();
    let registry = Arc::new(ConnectionRegistry::default());
    let id = registry.insert(server.into_std().unwrap());
    let waiting = tokio::spawn({
        let registry = registry.clone();
        async move { bounded!(registry.wait_empty()) }
    });

    tokio::task::yield_now().await;
    registry.remove(id);
    bounded!(time::timeout(Duration::from_millis(50), waiting))
        .unwrap()
        .unwrap();
    drop(client);
}

#[tokio::test]
async fn accepted_connections_disable_nagle_for_small_state_frames() {
    use axum::serve::Listener as _;

    let listener = bounded!(TcpListener::bind("127.0.0.1:0")).unwrap();
    let address = listener.local_addr().unwrap();
    let mut tracked = TrackedListener {
        listener,
        registry: Arc::new(ConnectionRegistry::default()),
        flushes: Arc::new(ResponseFlushRegistry::default()),
    };
    let connect = tokio::spawn(async move { bounded!(TcpStream::connect(address)).unwrap() });
    let (io, _) = bounded!(tracked.accept());
    let _client = bounded!(connect).unwrap();
    assert!(io.stream.nodelay().unwrap());
}

#[tokio::test]
async fn production_factory_binds_effective_address_and_serves_requests() {
    let binder = TokioBinder(AxumListenerFactory::new(production_providers(Arc::new(
        ProductionState,
    ))));
    let listener = bounded!(binder.bind(
        &ListenTarget::new("localhost", 0),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .unwrap();
    assert_ne!(listener.address().port(), 0);
    let response = bounded!(http_get(listener.address(), "/json")).unwrap();
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"));
    assert!(response.contains(r#"{"ok":true}"#));
    assert!(
        response
            .to_ascii_lowercase()
            .contains("access-control-allow-origin: *")
    );
    listener.begin_drain();
    bounded!(time::timeout(
        Duration::from_millis(100),
        listener.drained()
    ))
    .unwrap();
}

#[tokio::test]
async fn production_runtime_rebinds_before_old_drain_and_shutdown_stops_new_accepts() {
    let first_port = bounded!(unused_port());
    let second_port = bounded!(unused_port());
    let binder = Arc::new(TokioBinder(AxumListenerFactory::new(production_providers(
        Arc::new(ProductionState),
    ))));
    let (runtime, mut state) = ListenerRuntime::with_policy(
        Arc::new(PortResolver),
        binder,
        vec![Duration::from_millis(1)].into(),
        Duration::from_millis(20),
    );
    let (tx, rx) = watch::channel(ListenTarget::new("first.test", first_port));
    let stop = CancellationToken::new();
    let task = tokio::spawn(runtime.run(rx, stop.clone()));
    let first = bounded!(wait_effective(&mut state, "first.test"));
    assert!(
        String::from_utf8(bounded!(http_get(first, "/json")).unwrap())
            .unwrap()
            .contains(r#"{"ok":true}"#)
    );

    tx.send(ListenTarget::new("second.test", second_port))
        .unwrap();
    let second = bounded!(wait_effective(&mut state, "second.test"));
    assert!(
        String::from_utf8(bounded!(http_get(second, "/json")).unwrap())
            .unwrap()
            .contains(r#"{"ok":true}"#)
    );
    for _ in 0..20 {
        if bounded!(TcpStream::connect(first)).is_err() {
            break;
        }
        bounded!(time::sleep(Duration::from_millis(2)));
    }
    assert!(bounded!(TcpStream::connect(first)).is_err());

    stop.cancel();
    bounded!(task).unwrap();
    assert!(bounded!(TcpStream::connect(second)).is_err());
}

#[tokio::test]
async fn production_drain_deadline_force_closes_active_connection() {
    let binder = TokioBinder(AxumListenerFactory::new(production_providers(Arc::new(
        HangingState,
    ))));
    let listener = bounded!(binder.bind(
        &ListenTarget::new("localhost", 0),
        "127.0.0.1:0".parse().unwrap(),
    ))
    .unwrap();
    let mut stream = bounded!(TcpStream::connect(listener.address())).unwrap();
    bounded!(stream.write_all(b"GET /json HTTP/1.1\r\nHost: localhost\r\n\r\n")).unwrap();
    bounded!(time::sleep(Duration::from_millis(5)));
    bounded!(drain(listener, Duration::from_millis(5)));
    let mut byte = [0_u8; 1];
    let read = bounded!(time::timeout(
        Duration::from_millis(50),
        stream.read(&mut byte)
    ))
    .unwrap();
    assert!(read.is_err() || read.unwrap() == 0);
}

#[tokio::test]
async fn drain_deadline_does_not_wait_for_broken_completion_signal() {
    struct StuckListener {
        cancelled: AtomicBool,
    }

    impl BoundListener for StuckListener {
        fn address(&self) -> SocketAddr {
            "127.0.0.1:0".parse().unwrap()
        }

        fn begin_drain(&self) {}

        fn drained(&self) -> RuntimeFuture<'_, ()> {
            Box::pin(std::future::pending())
        }

        fn cancel_survivors(&self) {
            self.cancelled.store(true, Ordering::SeqCst);
        }
    }

    let listener = Arc::new(StuckListener {
        cancelled: AtomicBool::new(false),
    });
    bounded!(time::timeout(
        Duration::from_millis(50),
        drain(listener.clone(), Duration::from_millis(5)),
    ))
    .unwrap();
    assert!(listener.cancelled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn hostname_resolution_preserves_first_ordered_address() {
    let first: SocketAddr = "[::1]:24050".parse().unwrap();
    let second = "127.0.0.1:24050".parse().unwrap();
    let target = ListenTarget::new("localhost", 24050);
    assert_eq!(
        bounded!(resolve_first(&Resolver(vec![first, second]), &target)).unwrap(),
        first
    );
    assert_eq!(target.host, "localhost");
}

#[tokio::test]
async fn failed_rebind_retains_old_then_retry_binds_before_drain() {
    let binder = Arc::new(Binder {
        calls: AtomicUsize::new(0),
        fail_call: AtomicUsize::new(2),
        handles: Mutex::new(Vec::new()),
    });
    let (runtime, state) = ListenerRuntime::with_policy(
        Arc::new(PortResolver),
        binder.clone(),
        vec![Duration::from_millis(10)].into(),
        Duration::from_millis(1),
    );
    let (tx, rx) = watch::channel(ListenTarget::new("one.test", 24050));
    let stop = CancellationToken::new();
    let task = tokio::spawn(runtime.run(rx, stop.clone()));
    tokio::task::yield_now().await;
    assert_eq!(
        state.borrow().effective.as_ref().unwrap().target.host,
        "one.test"
    );
    tx.send(ListenTarget::new("two.test", 24051)).unwrap();
    tokio::task::yield_now().await;
    assert_eq!(
        state.borrow().effective.as_ref().unwrap().target.host,
        "one.test"
    );
    assert!(state.borrow().last_error.is_some());
    assert!(
        !bounded!(binder.handles.lock())[0]
            .began
            .load(Ordering::SeqCst)
    );
    bounded!(time::sleep(Duration::from_millis(15)));
    tokio::task::yield_now().await;
    assert_eq!(
        state.borrow().effective.as_ref().unwrap().target.host,
        "two.test"
    );
    assert!(
        bounded!(binder.handles.lock())[0]
            .began
            .load(Ordering::SeqCst)
    );
    stop.cancel();
    bounded!(task).unwrap();
}

#[tokio::test]
async fn hostname_change_resolving_to_active_address_reuses_listener() {
    let address = "127.0.0.1:24050".parse().unwrap();
    let binder = Arc::new(Binder {
        calls: AtomicUsize::new(0),
        fail_call: AtomicUsize::new(usize::MAX),
        handles: Mutex::new(Vec::new()),
    });
    let (runtime, mut state) = ListenerRuntime::with_policy(
        Arc::new(Resolver(vec![address])),
        binder.clone(),
        vec![Duration::from_millis(1)].into(),
        Duration::from_millis(1),
    );
    let (tx, rx) = watch::channel(ListenTarget::new("first.test", 24050));
    let stop = CancellationToken::new();
    let task = tokio::spawn(runtime.run(rx, stop.clone()));
    bounded!(wait_effective(&mut state, "first.test"));

    tx.send(ListenTarget::new("second.test", 24050)).unwrap();
    assert_eq!(bounded!(wait_effective(&mut state, "second.test")), address);
    bounded!(time::sleep(Duration::from_millis(5)));

    let current = state.borrow().clone();
    assert_eq!(
        current.desired,
        Some(ListenTarget::new("second.test", 24050))
    );
    assert_eq!(
        current.effective.as_ref().unwrap().target,
        ListenTarget::new("second.test", 24050),
    );
    assert_eq!(current.effective.as_ref().unwrap().address, address);
    assert!(current.last_error.is_none());
    assert_eq!(binder.calls.load(Ordering::SeqCst), 1);
    assert_eq!(bounded!(binder.handles.lock()).len(), 1);

    stop.cancel();
    bounded!(task).unwrap();
}

#[tokio::test]
async fn immediate_shutdown_awaits_the_obsolete_listener_drain() {
    let binder = Arc::new(RebindBinder {
        calls: AtomicUsize::new(0),
        handles: Mutex::new(Vec::new()),
    });
    let (runtime, mut state) = ListenerRuntime::with_policy(
        Arc::new(PortResolver),
        binder.clone(),
        vec![Duration::from_millis(1)].into(),
        Duration::from_millis(5),
    );
    let (tx, rx) = watch::channel(ListenTarget::new("old.test", 24050));
    let stop = CancellationToken::new();
    let task = tokio::spawn(runtime.run(rx, stop.clone()));
    bounded!(wait_effective(&mut state, "old.test"));
    tx.send(ListenTarget::new("new.test", 24051)).unwrap();
    bounded!(wait_effective(&mut state, "new.test"));
    stop.cancel();
    bounded!(time::timeout(Duration::from_millis(50), task))
        .unwrap()
        .unwrap();

    let handles = bounded!(binder.handles.lock());
    assert_eq!(handles.len(), 2);
    assert!(handles[0].began.load(Ordering::SeqCst));
    assert!(handles[0].cancelled.load(Ordering::SeqCst));
    assert!(handles[1].began.load(Ordering::SeqCst));
    assert!(!handles[1].cancelled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn config_change_cancels_and_discards_obsolete_bind_attempt() {
    let address = "127.0.0.1:24050".parse().unwrap();
    let resolver = Arc::new(RacingResolver {
        address,
        started: Notify::new(),
        cancelled: AtomicBool::new(false),
    });
    let binder = Arc::new(Binder {
        calls: AtomicUsize::new(0),
        fail_call: AtomicUsize::new(usize::MAX),
        handles: Mutex::new(Vec::new()),
    });
    let (runtime, mut state) = ListenerRuntime::with_policy(
        resolver.clone(),
        binder,
        vec![Duration::from_millis(1)].into(),
        Duration::from_millis(1),
    );
    let (tx, rx) = watch::channel(ListenTarget::new("slow.test", 24050));
    let stop = CancellationToken::new();
    let task = tokio::spawn(runtime.run(rx, stop.clone()));
    bounded!(resolver.started.notified());
    tx.send(ListenTarget::new("fast.test", 24050)).unwrap();
    loop {
        if state
            .borrow()
            .effective
            .as_ref()
            .is_some_and(|value| value.target.host == "fast.test")
        {
            break;
        }
        bounded!(state.changed()).unwrap();
    }
    assert!(resolver.cancelled.load(Ordering::SeqCst));
    stop.cancel();
    bounded!(task).unwrap();
}

#[tokio::test]
async fn shutdown_cancels_in_flight_resolution_without_waiting_for_it() {
    let resolver = Arc::new(RacingResolver {
        address: "127.0.0.1:24050".parse().unwrap(),
        started: Notify::new(),
        cancelled: AtomicBool::new(false),
    });
    let binder = Arc::new(Binder {
        calls: AtomicUsize::new(0),
        fail_call: AtomicUsize::new(usize::MAX),
        handles: Mutex::new(Vec::new()),
    });
    let (runtime, _) = ListenerRuntime::with_policy(
        resolver.clone(),
        binder,
        vec![Duration::from_millis(1)].into(),
        Duration::from_millis(1),
    );
    let (_tx, rx) = watch::channel(ListenTarget::new("slow.test", 24050));
    let stop = CancellationToken::new();
    let task = tokio::spawn(runtime.run(rx, stop.clone()));
    bounded!(resolver.started.notified());
    stop.cancel();
    bounded!(time::timeout(Duration::from_millis(50), task))
        .unwrap()
        .unwrap();
    assert!(resolver.cancelled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn drain_deadline_cancels_survivors() {
    let handle = Arc::new(Handle {
        address: "127.0.0.1:1".parse().unwrap(),
        began: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        hold: true,
        released: CancellationToken::new(),
    });
    let copy = handle.clone();
    let task = tokio::spawn(drain(handle, Duration::from_millis(5)));
    tokio::task::yield_now().await;
    assert!(copy.began.load(Ordering::SeqCst));
    assert!(!copy.cancelled.load(Ordering::SeqCst));
    bounded!(time::sleep(Duration::from_millis(10)));
    bounded!(task).unwrap();
    assert!(copy.cancelled.load(Ordering::SeqCst));
}
