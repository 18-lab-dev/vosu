//! Isolated test-harness access to the real daemon composition and joins.
use super::*;
use crate::{
    logging::ConsoleSink,
    plugin::{BundledPlugin, DiscoveryService, ProcessSource, WorkerPool},
    server::service::{PluginManagementRoutes, SurfaceRouter},
};
use std::path::Path;

#[derive(Default)]
struct Console(Mutex<String>);
impl ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, text: &str) -> io::Result<()> {
        self.0.lock().unwrap().push_str(text);
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
}

async fn owner(root: &Path, bundle: &'static Bundle) -> (ProductionRuntime, Arc<Console>) {
    let config = Arc::new(
        ConfigStore::initialize(root.join("vosu.toml"))
            .await
            .unwrap(),
    );
    let current = config.current();
    let console = Arc::new(Console::default());
    let logger = Logger::new(root, LoggerOptions::default(), console.clone()).unwrap();
    let bridge = Arc::new(ConfigSettingsBridgeB2::new(config.clone()));
    let settings_storage = Arc::new(RuntimeSettingsStorage::new(
        bridge.clone(),
        Arc::new(crate::plugin::settings::PluginSettingsStore::open(root)),
        bundle.overlay_root,
    ));
    let paths = crate::runtime::test_paths::runtime_test_paths(root);
    settings_storage.seed_overlay_root(&resolved_static_root(&paths, &current).unwrap());
    let plugin_settings = Arc::new(PluginSettingsService::new(
        settings_storage.clone() as Arc<dyn crate::plugin::settings::SettingsStorage>
    ));
    let plugins = PluginHost::new(logger.clone(), plugin_settings.clone(), Handle::current());
    let plugin_settings_watch = plugins.watch_bridge_b2(&bridge, &Handle::current());
    (
        ProductionRuntime {
            bundle,
            changes: config.subscribe(),
            config,
            logger,
            paths,
            shutdown_token: CancellationToken::new(),
            signal_task: AsyncMutex::new(None),
            startup_readiness: None,
            authorizer: Arc::new(ReloadableAuthorizer::new(
                &current.allowed_ips,
                &current.server_ip,
            )),
            update_handed_off: AtomicBool::new(false),
            update_notice: None,
            removed_routes: Arc::default(),
            overlay: AsyncMutex::new(None),
            settings_storage,
            plugin_settings,
            tools: AsyncMutex::new(None),
            tool_host: AsyncMutex::new(None),
            catalog: AsyncMutex::new(None),
            dev_tools: vec![],
            dev_plugins: dev_plugins::StartupPlugins::default(),
            api_v1: api_v1::ApiV1Service::new(&plugins, &current),
            plugins,
            state_host: AsyncMutex::new(None),
            plugin_settings_watch: AsyncMutex::new(Some(plugin_settings_watch)),
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
            ordinary_role: false,
            dashboard: true,
            tray: AsyncMutex::new(None),
            discovery: AsyncMutex::new(None),
            #[cfg(windows)]
            foreground: AsyncMutex::new(None),
            #[cfg(target_os = "linux")]
            login_state: Arc::new(AsyncMutex::new(false)),
        },
        console,
    )
}

/// A production runtime with temporary paths, no tray/login side effects and
/// a discovery source controlled by the calling test.
pub struct TestDaemon {
    runtime: ProductionRuntime,
    console: Arc<Console>,
}

impl TestDaemon {
    pub async fn new(root: &Path, bundle: &'static Bundle) -> Self {
        let (runtime, console) = owner(root, bundle).await;
        Self { runtime, console }
    }

    pub fn add_development_tool(&mut self, root: PathBuf) {
        self.runtime.dev_tools.push(root);
    }

    pub async fn listener_addresses(&self) -> (std::net::SocketAddr, std::net::SocketAddr) {
        async fn address(owner: &AsyncMutex<Option<ListenerOwner>>) -> std::net::SocketAddr {
            owner
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
            address(&self.runtime.listener).await,
            address(&self.runtime.dashboard_listener).await,
        )
    }

    pub async fn adapter_cache_generations(&self) -> [u64; 4] {
        let adapters = self.runtime.adapters.lock().await;
        let adapters = adapters.as_ref().expect("composed adapters");
        [
            WebSocketSchema::V1,
            WebSocketSchema::StreamCompanion,
            WebSocketSchema::V2,
            WebSocketSchema::V2Precise,
        ]
        .map(|schema| adapters.provider(schema).cached_generation())
    }

    pub fn config(&self) -> &Arc<ConfigStore> {
        &self.runtime.config
    }
    pub fn logger(&self) -> &Logger {
        &self.runtime.logger
    }
    pub fn plugins(&self) -> &Arc<PluginHost> {
        &self.runtime.plugins
    }
    pub fn output(&self) -> String {
        self.console.0.lock().unwrap().clone()
    }
    pub fn shutdown_token(&self) -> &CancellationToken {
        &self.runtime.shutdown_token
    }
    pub async fn current_overlay_root(&self) -> Option<PathBuf> {
        let overlay = self.runtime.overlay.lock().await.clone()?;
        Some(overlay.current_root())
    }

    pub async fn compose(&mut self, source: Arc<dyn ProcessSource>) -> io::Result<()> {
        let broker = self.runtime.paths.config().join("broker.json");
        self.runtime
            .compose_with_discovery(Ok(Some(broker)), move |host, logger| {
                Some(Self::discovery(host, logger, source))
            })
            .await
    }

    fn discovery(
        host: &Arc<PluginHost>,
        logger: &Logger,
        source: Arc<dyn ProcessSource>,
    ) -> DiscoveryService {
        let source = Arc::new(crate::runtime::discovery::operational::LoggedSource::new(
            source,
            logger.clone(),
        ));
        DiscoveryService::start(host, source, Handle::current())
    }

    pub fn install_discovery(&mut self, source: Arc<dyn ProcessSource>) {
        let runtime = &mut self.runtime;
        *runtime.discovery.get_mut() =
            Some(Self::discovery(&runtime.plugins, &runtime.logger, source));
    }

    pub async fn discovery_running(&self) -> bool {
        self.runtime.discovery.lock().await.is_some()
    }

    pub fn load_all(&self, dashboard: bool) -> io::Result<Arc<dyn PluginManagementRoutes>> {
        let owner = &self.runtime;
        Ok(Arc::new(dev_plugins::load_all(
            owner.bundle,
            &owner.plugins,
            dev_plugins::StartupPlugins::default(),
            &owner.paths,
            &owner.logger,
            dashboard,
        )?))
    }

    pub fn load_bundled(&self, entries: &[BundledPlugin<'_>]) -> io::Result<u64> {
        let owner = &self.runtime;
        let pool = WorkerPool::new(2, 65536);
        dev_plugins::load_bundled(
            owner.bundle,
            &owner.plugins,
            &owner.paths,
            &owner.logger,
            &pool,
            entries,
            &[],
            &mut super::super::plugin_management::Roster::default(),
            true,
        )
    }

    pub async fn start_adapters(&self, dashboard: bool) -> io::Result<TestAdapters> {
        let owner = &self.runtime;
        let (adapters, hubs) = adapters::Adapters::start(
            owner.bundle,
            &owner.plugins,
            owner.config.clone(),
            dashboard,
        )
        .await?;
        Ok(TestAdapters {
            adapters,
            hubs: Some(hubs),
        })
    }

    pub fn set_hubs(&mut self, hubs: WebSocketHubs) {
        *self.runtime.hubs.get_mut() = Some(hubs);
    }
    pub fn set_signal_task(&mut self, task: JoinHandle<io::Result<()>>) {
        *self.runtime.signal_task.get_mut() = Some(task);
    }
    pub async fn commands_hub(&self) -> Option<WebSocketHub> {
        self.runtime
            .hubs
            .lock()
            .await
            .as_ref()
            .map(|hubs| hubs.v1.clone())
    }
    pub fn subscriptions(&self) -> crate::server::api_v1::subscriptions::ApiV1SubscriptionHub {
        self.runtime.api_v1.subscription_hub()
    }

    /// Observe completion without moving or aborting any runtime-owned task.
    #[cfg(feature = "test-harness")]
    pub async fn join_probes(&self) -> Vec<(&'static str, tokio::task::AbortHandle)> {
        let mut probes = Vec::new();
        for (name, owner) in [
            ("overlay listener", &self.runtime.listener),
            ("dashboard listener", &self.runtime.dashboard_listener),
        ] {
            if let Some(listener) = owner.lock().await.as_ref() {
                probes.push((name, listener.task.abort_handle()));
                if let Some(watcher) = &listener.cli_task {
                    probes.push(("broker watcher", watcher.abort_handle()));
                }
            }
        }
        if let Some(hubs) = self.runtime.hubs.lock().await.as_ref() {
            probes.extend(hubs.join_probes().map(|handle| ("legacy hub", handle)));
        }
        if let Some(task) = self.runtime.plugin_settings_watch.lock().await.as_ref() {
            probes.push(("settings bridge", task.abort_handle()));
        }
        probes
    }

    pub async fn shutdown(&self) -> io::Result<()> {
        self.runtime.shutdown().await
    }
    pub async fn finish_run(
        &self,
        result: io::Result<crate::app::StartupOutcome>,
    ) -> io::Result<()> {
        finish_runtime_run(result, self.runtime.shutdown()).await
    }
    pub async fn shutdown_with_plugin_destroy(
        &self,
        destroy: impl Future<Output = io::Result<()>>,
    ) -> io::Result<()> {
        self.runtime.shutdown_with_plugin_destroy(destroy).await
    }
}

pub struct TestAdapters {
    adapters: Arc<adapters::Adapters>,
    hubs: Option<WebSocketHubs>,
}
impl TestAdapters {
    pub fn router(&self, host: Arc<PluginHost>) -> SurfaceRouter {
        self.adapters.router(host)
    }
    pub async fn stop_adapters(&self) -> io::Result<()> {
        self.adapters.shutdown().await
    }
    pub async fn stop_hubs(&mut self) {
        self.hubs.take().expect("hubs are running").shutdown().await;
    }
}
