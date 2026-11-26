//! Copied into the frozen 33f957de checkout by scripts/prepare-m7-bench-baseline.py.
//! Like the current TestDaemon, this changes only test construction/discovery;
//! the old listeners, tool authority, adapters, and host composition run as is.
use super::*;
use std::path::Path;
use vosu_core::{
    logging::ConsoleSink,
    plugin::{DiscoveryService, ProcessSource},
};

#[path = "m7_bench_paths.rs"]
mod paths;

struct Console;
impl ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
}

pub struct TestDaemon(ProductionRuntime);
impl TestDaemon {
    pub async fn new(root: &Path) -> Self {
        let config = Arc::new(
            ConfigStore::initialize(root.join("vosu.toml"))
                .await
                .unwrap(),
        );
        let current = config.current();
        let logger = Logger::new(root, LoggerOptions::default(), Arc::new(Console)).unwrap();
        let settings = Arc::new(ConfigSettingsBridgeB2::new(config.clone()));
        let plugins = PluginHost::new(
            logger.clone(),
            Arc::new(PluginSettingsService::new(settings.clone())),
            Handle::current(),
        );
        let watch = plugins.watch_bridge_b2(&settings, &Handle::current());
        Self(ProductionRuntime {
            changes: config.subscribe(),
            config,
            logger,
            paths: paths::runtime_test_paths(root),
            shutdown_token: CancellationToken::new(),
            signal_task: AsyncMutex::new(None),
            startup_readiness: None,
            authorizer: Arc::new(ReloadableAuthorizer::new(
                &current.allowed_ips,
                &current.server_ip,
            )),
            update_handed_off: AtomicBool::new(false),
            update_notice: None,
            counters: AsyncMutex::new(None),
            tools: AsyncMutex::new(None),
            catalog: AsyncMutex::new(None),
            dev_tools: vec![],
            dev_plugins: vec![],
            api_v1: api_v1::ApiV1Service::new(&plugins, &current),
            plugins,
            state_host: AsyncMutex::new(None),
            plugin_settings_watch: AsyncMutex::new(Some(watch)),
            listener: AsyncMutex::new(None),
            dashboard_listener: AsyncMutex::new(None),
            origins: Arc::new(
                crate::server::origins::Origins::new(
                    "http://127.0.0.1:24050".into(),
                    "http://127.0.0.1:24051".into(),
                )
                .unwrap(),
            ),
            hubs: AsyncMutex::new(None),
            adapters: AsyncMutex::new(None),
            processor: AsyncMutex::new(None),
            relays: AsyncMutex::new(None),
            ordinary_role: false,
            tray: AsyncMutex::new(None),
            discovery: AsyncMutex::new(None),
            #[cfg(windows)]
            foreground: AsyncMutex::new(None),
            #[cfg(target_os = "linux")]
            login_state: Arc::new(AsyncMutex::new(false)),
        })
    }

    pub fn config(&self) -> &Arc<ConfigStore> {
        &self.0.config
    }

    pub fn plugins(&self) -> &Arc<PluginHost> {
        &self.0.plugins
    }

    pub fn add_development_tool(&mut self, root: PathBuf) {
        self.0.dev_tools.push(root);
    }

    pub async fn compose(&mut self, source: Arc<dyn ProcessSource>) -> io::Result<()> {
        let broker = self.0.paths.config().join("broker.json");
        self.0
            .compose_with_discovery(Ok(Some(broker)), move |host, _logger| {
                Some(DiscoveryService::start(host, source, Handle::current()))
            })
            .await
    }

    pub async fn listener_addresses(&self) -> (std::net::SocketAddr, std::net::SocketAddr) {
        async fn address(listener: &AsyncMutex<Option<ListenerOwner>>) -> std::net::SocketAddr {
            listener
                .lock()
                .await
                .as_ref()
                .unwrap()
                .state()
                .effective
                .unwrap()
                .address
        }
        (
            address(&self.0.listener).await,
            address(&self.0.dashboard_listener).await,
        )
    }

    pub async fn shutdown(&self) -> io::Result<()> {
        self.0.shutdown().await
    }
}
