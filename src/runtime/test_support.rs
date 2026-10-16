use super::*;

/// Feature-gated constructor inputs for exercising the exact production
/// router/delegate/listener stack with reserved in-process network endpoints.
#[cfg(feature = "test-harness")]
pub struct TestProductionCompositionOptions {
    pub target: ListenTarget,
    pub config: Arc<ConfigStore>,
    pub static_root: PathBuf,
    pub settings_root: PathBuf,
    pub cache_root: PathBuf,
    pub logs_root: PathBuf,
    pub updater: Arc<dyn UpdaterHandoff>,
    pub network: crate::net::streaming::TestNetworkConfig,
}

/// Owns the real production listener, WebSocket hubs, command workers, and
/// counter service used by the in-process harness. Shutdown follows the same
/// stop-accepting / 1001-close / listener-drain order as `ProductionRuntime`.
#[cfg(feature = "test-harness")]
pub struct TestProductionComposition {
    address: std::net::SocketAddr,
    listener: Option<ListenerOwner>,
    hubs: Option<WebSocketHubs>,
    processor: Option<Arc<CommandProcessor>>,
    relays: Option<Arc<WebSocketRelaySink>>,
    counters: Option<Arc<ReloadableCounterService>>,
}

#[cfg(feature = "test-harness")]
impl TestProductionComposition {
    pub fn address(&self) -> std::net::SocketAddr {
        self.address
    }

    pub async fn shutdown(mut self) {
        let listener = self.listener.take();
        if let Some(listener) = listener.as_ref() {
            listener.stop_accepting();
        }
        if let Some(hubs) = self.hubs.take() {
            hubs.shutdown().await;
        }
        if let Some(listener) = listener {
            listener.join().await;
        }
        if let Some(processor) = self.processor.take() {
            processor.shutdown().await;
        }
        if let Some(relays) = self.relays.take() {
            relays.shutdown().await;
        }
        if let Some(counters) = self.counters.take() {
            counters.shutdown().await;
        }
    }
}

/// Builds the ordinary production `ProductionServices`,
/// `ProductionRouteDelegate`, five hubs, `RouteProviders`, and Axum listener.
/// Only the updater and hardened network policy are injected.
#[cfg(feature = "test-harness")]
pub async fn start_test_production_composition(
    options: TestProductionCompositionOptions,
) -> io::Result<TestProductionComposition> {
    let config = options.config.current();
    fs::create_dir_all(&options.static_root)?;
    let counters = Arc::new(ReloadableCounterService::production(
        options.static_root,
        options.settings_root,
        options.cache_root.clone(),
    ));
    let snapshots = Arc::new(SnapshotProviders::new(
        Arc::new(ManagerSnapshot::not_ready(
            0,
            ApiRuntimeSettings {
                enable_key_overlay: config.enable_key_overlay,
                enable_mania_key_hit_errors: config.enable_mania_key_hit_errors,
            },
        )),
        Arc::new(ProductionSnapshotEncoder),
    ));
    let beatmap = Arc::new(RuntimeBeatmap {
        snapshots: snapshots.snapshot_manager(),
        counters: Arc::clone(&counters),
    });
    let counter_facade = Arc::new(RuntimeCounterFacade {
        counters: Arc::clone(&counters),
    });
    let relays = Arc::new(WebSocketRelaySink::new());
    let processor = Arc::new(CommandProcessor::new(counter_facade, relays.clone()));
    let hubs = WebSocketHubs::start(snapshots.clone(), snapshots.clone(), Duration::ZERO);
    let endpoints = options
        .network
        .endpoints()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "test network endpoints missing",
            )
        })?
        .clone();
    let test_client = crate::net::streaming::StreamingClient::for_test_network(
        options.network,
        crate::net::streaming::Deadlines::default(),
        crate::net::streaming::DownloadLimits::default(),
    );
    let services = ProductionServices::with_native_report_probe(
        PageService,
        Arc::new(RuntimePages {
            pages: PageService,
            counters: Arc::clone(&counters),
            catalog: CatalogService::new(
                test_client,
                endpoints.catalog,
                &current_version().to_string(),
            ),
            config: Arc::clone(&options.config),
            shutdown: CancellationToken::new(),
            update_version: None,
        }),
        Arc::clone(&counters),
        beatmap.clone(),
        Arc::new(RuntimeConfig::new(Arc::clone(&options.config))),
        Arc::new(RuntimeReportInput {
            config: Arc::clone(&options.config),
            snapshots: snapshots.snapshot_manager(),
            counters: Arc::clone(&counters),
            lifecycle: Arc::default(),
            logs: options.logs_root,
        }),
        options.updater,
        WebSocketEndpoints {
            v1: hubs.v1.clone(),
            sc: hubs.stream_companion.clone(),
            v2: hubs.v2.clone(),
            precise: hubs.v2_precise.clone(),
            commands: hubs.v1.clone(),
            subscriptions: hubs.v2_subscription.clone(),
            processor: processor.clone(),
            relays: relays.clone(),
        },
    );
    let providers = RouteProviders {
        authorization: Arc::new(AuthorizationPolicy::new(
            &config.allowed_ips,
            &config.server_ip,
        )),
        state: Arc::new(SnapshotStateProvider::new(snapshots.snapshot_manager())),
        calculation: Arc::new(LegacyCalculateProvider::new(beatmap)),
        delegate: Arc::new(ProductionRouteDelegate::new(services)),
    };
    let listener = match ListenerOwner::start(providers, options.target).await {
        Ok(listener) => listener,
        Err(error) => {
            hubs.shutdown().await;
            processor.shutdown().await;
            relays.shutdown().await;
            counters.shutdown().await;
            return Err(error);
        }
    };
    let address = listener
        .state()
        .effective
        .as_ref()
        .expect("listener start returned without effective address")
        .address;
    Ok(TestProductionComposition {
        address,
        listener: Some(listener),
        hubs: Some(hubs),
        processor: Some(processor),
        relays: Some(relays),
        counters: Some(counters),
    })
}
