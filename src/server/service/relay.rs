struct RelayClient {
    channel: u64,
    client: CommandClient,
    connection: Weak<tokio::sync::Mutex<Connection>>,
}

/// Production relay transport. WebSocket-originated events return to the
/// originating endpoint; route-originated refreshes target `/websocket/commands`.
#[derive(Clone)]
pub struct WebSocketRelaySink {
    clients: Arc<Mutex<HashMap<u64, RelayClient>>>,
    events: tokio::sync::mpsc::UnboundedSender<Relay>,
    processor: Arc<Mutex<Weak<CommandProcessor>>>,
    lifecycle: Arc<RelayLifecycle>,
}

struct RelayLifecycle {
    cancellation: CancellationToken,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for RelayLifecycle {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            worker.abort();
        }
    }
}

struct RelayRegistration {
    sink: WebSocketRelaySink,
    client_id: u64,
}

impl Drop for RelayRegistration {
    fn drop(&mut self) {
        self.sink.unregister(self.client_id);
    }
}

impl Default for WebSocketRelaySink {
    fn default() -> Self {
        Self::new()
    }
}

impl WebSocketRelaySink {
    pub fn new() -> Self {
        let clients = Arc::new(Mutex::new(HashMap::new()));
        let processor = Arc::new(Mutex::new(Weak::new()));
        let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
        let cancellation = CancellationToken::new();
        let worker = tokio::spawn(run_relay_worker(
            Arc::clone(&clients),
            Arc::clone(&processor),
            receiver,
            cancellation.clone(),
        ));
        Self {
            clients,
            events,
            processor,
            lifecycle: Arc::new(RelayLifecycle {
                cancellation,
                worker: Mutex::new(Some(worker)),
            }),
        }
    }

    fn bind_processor(&self, processor: &Arc<CommandProcessor>) {
        *self
            .processor
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(processor);
    }

    pub async fn shutdown(&self) {
        let processor = self
            .processor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upgrade();
        if let Some(processor) = processor {
            processor.shutdown().await;
        }
        self.lifecycle.cancellation.cancel();
        let worker = self
            .lifecycle
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = worker.await;
        }
    }

    pub fn is_shutdown(&self) -> bool {
        self.lifecycle.cancellation.is_cancelled()
    }

    fn register(
        &self,
        channel: u64,
        client: CommandClient,
        connection: &Arc<tokio::sync::Mutex<Connection>>,
    ) -> RelayRegistration {
        let client_id = client.id;
        self.clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                client_id,
                RelayClient {
                    channel,
                    client,
                    connection: Arc::downgrade(connection),
                },
            );
        RelayRegistration {
            sink: self.clone(),
            client_id,
        }
    }

    fn unregister(&self, client_id: u64) {
        self.clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&client_id);
    }
}

impl RelaySink for WebSocketRelaySink {
    fn relay(&self, event: Relay) {
        let _ = self.events.send(event);
    }
}

async fn run_relay_worker(
    clients: Arc<Mutex<HashMap<u64, RelayClient>>>,
    processor: Arc<Mutex<Weak<CommandProcessor>>>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<Relay>,
    cancellation: CancellationToken,
) {
    loop {
        let event = tokio::select! {
            _ = cancellation.cancelled() => return,
            event = events.recv() => {
                let Some(event) = event else { return };
                event
            }
        };
        let target_channel = event
            .except
            .and_then(client_channel)
            .unwrap_or(COMMANDS_CHANNEL);
        let recipients = {
            let clients = clients.lock().unwrap_or_else(PoisonError::into_inner);
            clients
                .values()
                .filter(|entry| entry.channel == target_channel)
                .filter_map(|entry| {
                    let frame = CommandProcessor::relay_frame(&entry.client, &event)?;
                    Some((entry.client.clone(), entry.connection.upgrade()?, frame))
                })
                .collect::<Vec<_>>()
        };
        let processor = processor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .upgrade();
        let Some(processor) = processor else {
            continue;
        };
        for (client, connection, frame) in recipients {
            match processor.process(&client, &frame) {
                CommandAction::Reply(frame) => {
                    tokio::select! {
                        _ = cancellation.cancelled() => return,
                        _ = async {
                            let _ = connection.lock().await.send_text(frame).await;
                        } => {}
                    }
                }
                CommandAction::ApplyFilters(filters) => {
                    connection.lock().await.set_filters(filters);
                }
                CommandAction::NoAck => {}
            }
        }
    }
}

const COMMANDS_CHANNEL: u64 = 5;

fn client_channel(client_id: u64) -> Option<u64> {
    let channel = client_id % 8;
    (1..=COMMANDS_CHANNEL).contains(&channel).then_some(channel)
}

#[derive(Clone)]
pub struct WebSocketEndpoints {
    pub v1: WebSocketHub,
    pub sc: WebSocketHub,
    pub v2: WebSocketHub,
    pub precise: WebSocketHub,
    pub commands: WebSocketHub,
    pub processor: Arc<CommandProcessor>,
    pub relays: Arc<WebSocketRelaySink>,
}

impl WebSocketEndpoints {
    fn hub(&self, endpoint: Endpoint) -> (&WebSocketHub, bool, u64) {
        match endpoint {
            Endpoint::WsV1 => (&self.v1, false, 1),
            Endpoint::WsTokens => (&self.sc, false, 2),
            Endpoint::WsV2 => (&self.v2, false, 3),
            Endpoint::WsV2Precise => (&self.precise, false, 4),
            Endpoint::WsCommands => (&self.commands, true, 5),
            _ => unreachable!("hub requested for non-WebSocket endpoint"),
        }
    }
}

#[derive(Clone)]
pub struct ProductionServices {
    pub pages: PageService,
    pub page_data: Arc<dyn PageDataProvider>,
    pub counters: Arc<ReloadableCounterService>,
    pub beatmap: Arc<dyn CurrentBeatmapProvider>,
    pub config: Arc<dyn ConfigUpdateService>,
    pub report_input: Arc<dyn ReportInputProvider>,
    pub report_probe: Arc<dyn ReportProbe>,
    pub updater: Arc<dyn UpdaterHandoff>,
    pub websocket: WebSocketEndpoints,
}

impl ProductionServices {
    // This public constructor intentionally exposes each required service dependency.
    #[allow(clippy::too_many_arguments)]
    pub fn with_native_report_probe(
        pages: PageService,
        page_data: Arc<dyn PageDataProvider>,
        counters: Arc<ReloadableCounterService>,
        beatmap: Arc<dyn CurrentBeatmapProvider>,
        config: Arc<dyn ConfigUpdateService>,
        report_input: Arc<dyn ReportInputProvider>,
        updater: Arc<dyn UpdaterHandoff>,
        websocket: WebSocketEndpoints,
    ) -> Self {
        Self {
            pages,
            page_data,
            counters,
            beatmap,
            config,
            report_input,
            report_probe: Arc::new(NativeReportProbe),
            updater,
            websocket,
        }
    }
}
