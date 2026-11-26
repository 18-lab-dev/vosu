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
    pub plugin: crate::plugin::BundledPlugin<'static>,
    pub foreground: Arc<crate::plugin::SyntheticForegroundSource>,
}

/// Owns the real listener, hubs, adapters, plugin instances and overlay
/// workers. Startup errors use the same ordered cleanup as a normal stop.
#[cfg(feature = "test-harness")]
pub struct TestProductionComposition {
    listener: Option<ListenerOwner>,
    hubs: Option<WebSocketHubs>,
    overlay: Option<Arc<dyn OverlayComposition>>,
    adapters: Option<Arc<adapters::Adapters>>,
    plugins: Arc<PluginHost>,
    logger: Logger,
}

#[cfg(feature = "test-harness")]
impl TestProductionComposition {
    pub fn address(&self) -> std::net::SocketAddr {
        self.listener
            .as_ref()
            .and_then(|listener| listener.state().effective.clone())
            .expect("listener has an effective address before composition returns")
            .address
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
        self.adapters
            .as_ref()
            .expect("adapters started")
            .provider(schema)
            .capture_for_harness()
    }

    pub async fn shutdown(mut self) {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            self.stop().await;
        })
        .await
        .expect("test production composition did not shut down within 30s");
    }

    async fn stop(&mut self) {
        if let Some(listener) = self.listener.as_ref() {
            listener.stop_accepting();
        }
        if let Some(hubs) = self.hubs.take() {
            hubs.shutdown().await;
        }
        if let Some(listener) = self.listener.take() {
            listener.join().await;
        }
        let adapters = match self.adapters.take() {
            Some(adapters) => adapters.shutdown().await,
            None => Ok(()),
        };
        let plugins = Arc::clone(&self.plugins);
        let destroyed = tokio::task::spawn_blocking(move || {
            plugins.destroy_all(dev_plugins::PLUGIN_DESTROY_GRACE)
        })
        .await;
        if let Some(overlay) = self.overlay.take() {
            overlay.shutdown().await;
        }
        let logged = self.logger.shutdown().await;
        adapters.expect("chart shutdown");
        destroyed.expect("plugin shutdown");
        logged.expect("logger shutdown");
    }
}

/// Builds the ordinary production `ProductionServices`,
/// `ProductionRouteDelegate`, five hubs, `RouteProviders`, and Axum listener.
/// Only the updater and hardened network policy are injected.
#[cfg(feature = "test-harness")]
pub async fn start_test_production_composition(
    bundle: &'static Bundle,
    options: TestProductionCompositionOptions,
) -> io::Result<TestProductionComposition> {
    let config = options.config.current();
    fs::create_dir_all(&options.static_root)?;
    let static_root = std::path::absolute(&options.static_root)?;
    let plugin_data = options.settings_root.join("plugins");
    let plugin_settings_root = options.settings_root.join("core");
    let overlay_instance = bundle
        .instance(bundle.overlay_root.plugin)
        .ok_or_else(|| io::Error::other("the overlay host is not a required bundled plugin"))?;
    let endpoint = options
        .network
        .endpoints()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "test network endpoints missing",
            )
        })?
        .catalog
        .clone();
    let catalog = OverlayCatalog::Test {
        client: vosu_net::streaming::StreamingClient::for_test_network(
            options.network,
            vosu_net::streaming::Deadlines::default(),
            vosu_net::streaming::DownloadLimits::default(),
        ),
        endpoint,
    };
    let origins = Arc::new(crate::server::origins::Origins::new(
        String::new(),
        String::new(),
    )?);
    fs::create_dir_all(&plugin_data)?;
    let logger = Logger::new(
        &options.logs_root,
        LoggerOptions::default(),
        Arc::new(StdoutConsole),
    )?;
    let storage = Arc::new(super::overlay_root::RuntimeSettingsStorage::new(
        Arc::new(ConfigSettingsBridgeB2::new(options.config.clone())),
        Arc::new(crate::plugin::settings::PluginSettingsStore::open(
            &plugin_settings_root,
        )),
        bundle.overlay_root,
    ));
    storage.seed_overlay_root(&static_root);
    let settings = Arc::new(PluginSettingsService::new(
        storage.clone() as Arc<dyn SettingsStorage>
    ));
    let plugins = match &options.game {
        Some(game) => {
            // The fixture uses a synthetic foreground and ephemeral resource ids.
            PluginHost::new_with_foreground(
                logger.clone(),
                settings.clone(),
                Handle::current(),
                game.foreground.clone(),
            )
        }
        None => match api_v1::resource_host(logger.clone(), settings.clone(), &plugin_data).await {
            Ok(host) => host,
            Err(error) => {
                logger.shutdown().await?;
                return Err(error);
            }
        },
    };
    let mut owner = TestProductionComposition {
        listener: None,
        hubs: None,
        overlay: None,
        adapters: None,
        plugins,
        logger,
    };
    let result = async {
        let mut bundled = bundle.plugins.to_vec();
        if let Some(game) = &options.game {
            bundled[0] = game.plugin;
        }
        let plugins = &owner.plugins;
        let loaded = crate::plugin::load_bundled(
            &bundle.version(),
            &dev_plugins::source_snapshot(plugins, &bundled, &[]),
            &bundled,
        )
        .map_err(io::Error::other)?;
        let pool = crate::plugin::WorkerPool::new(4, crate::plugin::DEFAULT_QUEUE_CAPACITY);
        for (index, plugin) in loaded.iter().enumerate() {
            plugins
                .create_instance(
                    crate::plugin::PluginInstanceId::new(index as u64),
                    plugin,
                    plugin_data.join(plugin.manifest().id.as_str()),
                    Some(pool.clone()),
                )
                .map_err(io::Error::other)?;
        }
        let overlay = bundle.hosts.overlay(OverlayInputs {
            host: Arc::clone(plugins),
            instance: overlay_instance,
            data_dir: plugin_data.join(bundle.overlay_root.plugin),
            storage,
            settings,
            static_root,
            settings_root: options.settings_root,
            cache_root: options.cache_root,
            version: bundle.version(),
            catalog,
            shutdown: CancellationToken::new(),
            origins: origins.clone(),
        });
        owner.overlay = Some(overlay.clone());
        let (adapters, hubs) =
            adapters::Adapters::start(bundle, plugins, options.config.clone(), true).await?;
        owner.adapters = Some(adapters.clone());
        let websocket = WebSocketEndpoints {
            v1: hubs.v1.clone(),
            sc: hubs.stream_companion.clone(),
            v2: hubs.v2.clone(),
            precise: hubs.v2_precise.clone(),
            commands: hubs.v1.clone(),
        };
        owner.hubs = Some(hubs);
        overlay.mount(websocket.commands.clone())?;
        let services = ProductionServices::with_native_report_probe(
            Arc::clone(&overlay),
            Arc::new(RuntimeConfig::new(Arc::clone(&options.config))),
            Arc::new(RuntimeReportInput {
                config: Arc::clone(&options.config),
                plugins: Arc::clone(plugins),
                adapters: Arc::clone(&adapters),
                overlay,
                logs: options.logs_root,
                update: None,
            }),
            options.updater,
            websocket,
        );
        let providers = RouteProviders {
            authorization: Arc::new(AuthorizationPolicy::new(
                &config.allowed_ips,
                &config.server_ip,
            )),
            delegate: Arc::new(
                ProductionRouteDelegate::new(services)
                    .with_surface(adapters.router(plugins.clone())),
            ),
        };
        let listener = ListenerOwner::start(providers, options.target).await?;
        origins.observe_listeners(listener.state.clone(), watch::channel(Default::default()).1);
        owner.listener = Some(listener);
        Ok(())
    }
    .await;
    if let Err(error) = result {
        owner.shutdown().await;
        return Err(error);
    }
    Ok(owner)
}
