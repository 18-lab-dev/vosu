// Listener rebind contract, included by `tests/server_compat.rs` so it shares
// that suite's imports.

struct ResolverFixture;
impl HostResolver for ResolverFixture {
    fn resolve<'a>(
        &'a self,
        target: &'a ListenTarget,
    ) -> RuntimeFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move { Ok(vec![SocketAddr::from(([127, 0, 0, 1], target.port))]) })
    }
}
struct BoundFixture {
    address: SocketAddr,
}
impl BoundListener for BoundFixture {
    fn address(&self) -> SocketAddr {
        self.address
    }
    fn begin_drain(&self) {}
    fn drained(&self) -> RuntimeFuture<'_, ()> {
        Box::pin(async {})
    }
    fn cancel_survivors(&self) {}
}
struct BinderFixture;
impl ListenerBinder for BinderFixture {
    fn bind<'a>(
        &'a self,
        target: &'a ListenTarget,
        address: SocketAddr,
    ) -> RuntimeFuture<'a, io::Result<Arc<dyn BoundListener>>> {
        Box::pin(async move {
            if target.host == "unresolvable.invalid" {
                Err(io::Error::new(
                    io::ErrorKind::AddrNotAvailable,
                    "fixture bind failure",
                ))
            } else {
                Ok(Arc::new(BoundFixture { address }) as Arc<dyn BoundListener>)
            }
        })
    }
}

#[tokio::test]
async fn failed_listener_rebind_keeps_effective_listener_and_shutdown_terminates_runtime() {
    let (runtime, mut state) = ListenerRuntime::with_policy(
        Arc::new(ResolverFixture),
        Arc::new(BinderFixture),
        vec![Duration::from_millis(20)].into(),
        Duration::from_millis(20),
    );
    let (desired, desired_rx) = watch::channel(ListenTarget::new("localhost", 24050));
    let shutdown = CancellationToken::new();
    let task = {
        let shutdown = shutdown.clone();
        tokio::spawn(runtime.run(desired_rx, shutdown))
    };
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            state.changed().await.unwrap();
            if state.borrow().effective.is_some() {
                break;
            }
        }
    })
    .await
    .unwrap();
    let original = state.borrow().effective.clone().unwrap();

    desired.send_replace(ListenTarget::new("unresolvable.invalid", 24051));
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            state.changed().await.unwrap();
            let snapshot = state.borrow();
            if snapshot
                .desired
                .as_ref()
                .is_some_and(|target| target.host == "unresolvable.invalid")
                && snapshot.last_error.is_some()
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(state.borrow().effective.as_ref().unwrap(), &original);
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
}
