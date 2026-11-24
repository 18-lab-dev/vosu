use super::lifecycle::api_v1;
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
    pub network: vosu_net::streaming::TestNetworkConfig,
    /// `None` loads the production game provider. `Some` replaces it.
    pub game: Option<TestGame>,
}

/// A fixture game that replaces the production game provider (`BUNDLED[0]`).
/// The fixture game publishes through the real ABI under its own manifest
/// grants. The adapters, routes and listener stay the production ones.
///
/// The host built for it differs from production in two ways. It uses this
/// synthetic foreground source, so recorded foreground facts can be
/// reproduced. Its resource secret is ephemeral. Resource ids are opaque
/// and are never echoed in adapter route bytes.
#[cfg(feature = "test-harness")]
pub struct TestGame {
    pub plugin: vosu_core::plugin::BundledPlugin<'static>,
    pub foreground: Arc<vosu_core::plugin::SyntheticForegroundSource>,
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
    adapters: Arc<adapters::Adapters>,
    plugins: Arc<PluginHost>,
    logger: Logger,
}

#[cfg(feature = "test-harness")]
impl TestProductionComposition {
    pub fn address(&self) -> std::net::SocketAddr {
        self.address
    }

    /// The composition's plugin host. Fixture games use it to report
    /// synthetic process records and foreground changes.
    pub fn plugins(&self) -> &Arc<PluginHost> {
        &self.plugins
    }

    /// The `test-harness` adapter capture of the mounted schema provider:
    /// one provider snapshot, never a route response.
    pub fn adapter_capture(
        &self,
        schema: WebSocketSchema,
    ) -> Result<crate::server::adapter_capture::AdapterCaptureSample, String> {
        self.adapters.provider(schema).capture_for_harness()
    }

    pub async fn shutdown(mut self) {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
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
            self.adapters.shutdown().await.expect("chart shutdown");
            let plugins = self.plugins.clone();
            tokio::task::spawn_blocking(move || {
                plugins.destroy_all(dev_plugins::PLUGIN_DESTROY_GRACE)
            })
            .await
            .expect("plugin shutdown");
            self.logger.shutdown().await.expect("logger shutdown");
        })
        .await
        .expect("test production composition did not shut down within 30s");
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
    let plugin_data = options.settings_root.join("plugins");
    let counters = Arc::new(ReloadableCounterService::production(
        options.static_root,
        options.settings_root,
        options.cache_root.clone(),
    ));
    let beatmap = Arc::new(RuntimeBeatmap {
        counters: Arc::clone(&counters),
    });
    let counter_facade = Arc::new(RuntimeCounterFacade {
        counters: Arc::clone(&counters),
    });
    let relays = Arc::new(WebSocketRelaySink::new());
    let processor = Arc::new(CommandProcessor::new(counter_facade, relays.clone()));
    let logger = Logger::new(
        &options.logs_root,
        LoggerOptions::default(),
        Arc::new(StdoutConsole),
    )?;
    let settings = Arc::new(PluginSettingsService::new(Arc::new(
        ConfigSettingsBridgeB2::new(options.config.clone()),
    )));
    let plugins = match &options.game {
        Some(game) => {
            fs::create_dir_all(&plugin_data)?;
            PluginHost::new_with_foreground(
                logger.clone(),
                settings,
                Handle::current(),
                game.foreground.clone(),
            )
        }
        None => api_v1::resource_host(logger.clone(), settings, &plugin_data).await?,
    };
    let mut bundled = crate::plugins::BUNDLED.to_vec();
    if let Some(game) = &options.game {
        bundled[0] = game.plugin;
    }
    let loaded = vosu_core::plugin::load_bundled(&bundled).map_err(io::Error::other)?;
    let pool = vosu_core::plugin::WorkerPool::new(4, vosu_core::plugin::DEFAULT_QUEUE_CAPACITY);
    for (index, plugin) in loaded.iter().enumerate() {
        plugins
            .create_instance(
                vosu_core::plugin::PluginInstanceId::new(index as u64),
                plugin,
                plugin_data.join(plugin.manifest().id.as_str()),
                Some(pool.clone()),
            )
            .map_err(io::Error::other)?;
    }
    let (adapters, hubs) = adapters::Adapters::start(&plugins, options.config.clone()).await?;
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
    let test_client = vosu_net::streaming::StreamingClient::for_test_network(
        options.network,
        vosu_net::streaming::Deadlines::default(),
        vosu_net::streaming::DownloadLimits::default(),
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
            update: None,
        }),
        Arc::clone(&counters),
        beatmap.clone(),
        Arc::new(RuntimeConfig::new(Arc::clone(&options.config))),
        Arc::new(RuntimeReportInput {
            config: Arc::clone(&options.config),
            plugins: Arc::clone(&plugins),
            files: adapters.files.clone(),
            counters: Arc::clone(&counters),
            logs: options.logs_root,
        }),
        options.updater,
        WebSocketEndpoints {
            v1: hubs.v1.clone(),
            sc: hubs.stream_companion.clone(),
            v2: hubs.v2.clone(),
            precise: hubs.v2_precise.clone(),
            commands: hubs.v1.clone(),
            processor: processor.clone(),
            relays: relays.clone(),
        },
    );
    let providers = RouteProviders {
        authorization: Arc::new(AuthorizationPolicy::new(
            &config.allowed_ips,
            &config.server_ip,
        )),
        state: adapters.clone(),
        calculation: Arc::new(DeclaredCalculateProvider(adapters.files.clone())),
        delegate: Arc::new(
            ProductionRouteDelegate::new(services).with_surface(adapters.router(plugins.clone())),
        ),
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
        adapters,
        plugins,
        logger,
    })
}
