use super::*;

/// Exactly five state publication loops. Connections are owned by their hubs;
/// shutdown cancels every loop and joins it before the provider can be dropped.
pub struct WebSocketHubs {
    pub v1: WebSocketHub,
    pub stream_companion: WebSocketHub,
    pub v2: WebSocketHub,
    pub v2_precise: WebSocketHub,
    pub v2_subscription: V2SubscriptionHub,
    tasks: Vec<JoinHandle<()>>,
}

impl WebSocketHubs {
    pub fn start(
        provider: Arc<dyn WebSocketStateProvider>,
        subscription_provider: Arc<dyn V2SubscriptionProvider>,
        debounce: Duration,
    ) -> Self {
        let v1 = WebSocketHub::new(WebSocketSchema::V1, Arc::clone(&provider));
        let stream_companion =
            WebSocketHub::new(WebSocketSchema::StreamCompanion, Arc::clone(&provider));
        let v2 = WebSocketHub::new(WebSocketSchema::V2, Arc::clone(&provider));
        let v2_precise = WebSocketHub::new(WebSocketSchema::V2Precise, provider);
        let v2_subscription = V2SubscriptionHub::new(subscription_provider, debounce);
        let mut tasks = [&v1, &stream_companion, &v2, &v2_precise]
            .into_iter()
            .map(|hub| {
                let hub = hub.clone();
                tokio::spawn(async move { hub.run().await })
            })
            .collect::<Vec<_>>();
        tasks.push(tokio::spawn({
            let hub = v2_subscription.clone();
            async move { hub.run().await }
        }));
        Self {
            v1,
            stream_companion,
            v2,
            v2_precise,
            v2_subscription,
            tasks,
        }
    }

    pub async fn shutdown(self) {
        // Seal registration and enqueue a protocol close before cancelling the
        // publication loops. `close_all` also waits for each writer/reader
        // callback to unregister, so no upgraded callback can outlive the hub.
        self.v1.close_all(1001).await;
        self.stream_companion.close_all(1001).await;
        self.v2.close_all(1001).await;
        self.v2_precise.close_all(1001).await;
        self.v2_subscription.close_all(1001);
        self.v1.shutdown();
        self.stream_companion.shutdown();
        self.v2.shutdown();
        self.v2_precise.shutdown();
        self.v2_subscription.shutdown();
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

/// Owns the reloadable production Axum listener and its complete join path.
pub struct ListenerOwner {
    pub(super) desired: watch::Sender<ListenTarget>,
    pub(super) state: watch::Receiver<ListenerState>,
    pub(super) cancel: CancellationToken,
    pub(super) task: JoinHandle<()>,
    pub(super) broker: Option<Arc<crate::server::broker::BrokerToken>>,
    pub(super) broker_logger: Option<vosu_core::logging::Logger>,
}

impl ListenerOwner {
    /// Starts the listener runtime and waits until the initial address is
    /// actually bound. Readiness must never be reported before this returns.
    pub async fn start(providers: RouteProviders, target: ListenTarget) -> io::Result<Self> {
        Self::start_with_broker(providers, target, Ok(None), None).await
    }

    pub(crate) async fn start_with_broker(
        providers: RouteProviders,
        target: ListenTarget,
        path: io::Result<Option<PathBuf>>,
        logger: Option<vosu_core::logging::Logger>,
    ) -> io::Result<Self> {
        let broker = match path.and_then(|path| {
            path.map(crate::server::broker::BrokerToken::new)
                .transpose()
        }) {
            Ok(token) => token.map(Arc::new),
            Err(error) => {
                if let Some(logger) = &logger {
                    logger.warn(format!("Native broker disabled: {error}"));
                }
                None
            }
        };
        let factory = AxumListenerFactory::new(providers).with_broker(broker.clone());
        let (runtime, state) =
            ListenerRuntime::new(Arc::new(TokioResolver), Arc::new(TokioBinder(factory)));
        let (desired, desired_rx) = watch::channel(target.clone());
        let cancel = CancellationToken::new();
        let task = tokio::spawn(runtime.run(desired_rx, cancel.clone()));
        let mut owner = Self {
            desired,
            state,
            cancel,
            task,
            broker,
            broker_logger: logger,
        };
        if let Err(error) = owner.wait_for_stage(&target).await {
            owner.shutdown().await;
            return Err(error);
        }
        Ok(owner)
    }

    /// Stages a replacement. The old listener stays effective when DNS or bind
    /// fails; callers commit their effective config only after success.
    pub async fn stage(&mut self, target: ListenTarget) -> io::Result<()> {
        if self.desired.borrow().clone() == target
            && self
                .state
                .borrow()
                .effective
                .as_ref()
                .is_some_and(|effective| effective.target == target)
        {
            return Ok(());
        }
        self.desired
            .send(target.clone())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "listener runtime stopped"))?;
        self.wait_for_stage(&target).await
    }

    pub fn state(&self) -> ListenerState {
        self.state.borrow().clone()
    }

    async fn wait_for_stage(&mut self, target: &ListenTarget) -> io::Result<()> {
        loop {
            let state = self.state.borrow().clone();
            if state
                .effective
                .as_ref()
                .is_some_and(|effective| &effective.target == target)
            {
                if let Some(broker) = &self.broker
                    && let Err(error) = broker.publish(
                        state
                            .effective
                            .as_ref()
                            .expect("effective listener")
                            .address,
                    )
                {
                    broker.disable();
                    if let Some(logger) = &self.broker_logger {
                        logger.warn(format!(
                            "Native broker disabled: credential publication failed: {error}"
                        ));
                    }
                }
                return Ok(());
            }
            if state.desired.as_ref() == Some(target)
                && let Some(error) = state.last_error
            {
                return Err(io::Error::new(io::ErrorKind::AddrNotAvailable, error));
            }
            self.state.changed().await.map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "listener runtime stopped before bind",
                )
            })?;
        }
    }

    /// Stops accepting new HTTP connections and begins the listener drain.
    /// Joining is deliberately separate so WebSocket hubs can first emit 1001
    /// and allow their upgrade callbacks to unregister.
    pub fn stop_accepting(&self) {
        self.cancel.cancel();
    }

    pub async fn join(self) {
        let _ = self.task.await;
    }

    pub async fn shutdown(self) {
        self.stop_accepting();
        self.join().await;
    }
}
