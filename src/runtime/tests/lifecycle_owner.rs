use super::*;
use vosu_core::logging::ConsoleSink;
use vosu_core::plugin::{DiscoveryService, ProcessRecord, ProcessSource, WorkerPool};
use vosu_osu::bridge_b1::{B1Event, B1Inputs};
use vosu_surface::LegacySink;

const WAIT: Duration = Duration::from_secs(10);

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

async fn owner(root: &Path) -> (ProductionRuntime, Arc<Console>) {
    let config = Arc::new(
        ConfigStore::initialize(root.join("vosu.toml"))
            .await
            .unwrap(),
    );
    let current = config.current();
    let console = Arc::new(Console::default());
    let logger = Logger::new(root, LoggerOptions::default(), console.clone()).unwrap();
    let settings = Arc::new(ConfigSettingsBridgeB2::new(config.clone()));
    let plugins = PluginHost::new(
        logger.clone(),
        Arc::new(PluginSettingsService::new(settings)),
        Handle::current(),
    );
    (
        ProductionRuntime {
            changes: config.subscribe(),
            config,
            logger,
            paths: crate::runtime::tests::runtime_test_paths(root),
            snapshots: Arc::new(SnapshotProviders::new(
                Arc::new(ManagerSnapshot::not_ready(0, ApiRuntimeSettings::default())),
                Arc::new(ProductionSnapshotEncoder),
            )),
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
            dev_tools: vec![],
            dev_plugins: vec![],
            plugins,
            plugin_settings_watch: AsyncMutex::new(None),
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
            processor: AsyncMutex::new(None),
            relays: AsyncMutex::new(None),
            bridge: AsyncMutex::new(None),
            bridge_installed: AsyncMutex::new(None),
            ordinary_role: false,
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

#[tokio::test]
async fn required_plugin_failure_aborts_composition_and_shuts_down_owned_services() {
    tokio::time::timeout(WAIT, async {
        let root = tempfile::tempdir().unwrap();
        let (mut owner, console) = owner(root.path()).await;
        // Deterministically make create_instance fail, without a process-global
        // fault switch or a different production plugin loading path.
        owner.plugins.destroy_all(Duration::ZERO);
        *owner.hubs.get_mut() = Some(WebSocketHubs::start(
            owner.snapshots.clone(),
            owner.snapshots.clone(),
            Duration::ZERO,
        ));
        let error = owner.compose().await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("required bundled plugin vosu.osu failed")
        );
        let outcome = finish_runtime_run(Err(error), owner.shutdown()).await;
        assert!(
            outcome.is_err(),
            "startup failure must propagate to the CLI's non-zero exit path"
        );
        assert!(owner.shutdown_token.is_cancelled());
        assert!(owner.hubs.lock().await.is_none());
        let output = console.0.lock().unwrap();
        assert!(output.contains("Plugin startup failed"));
        assert!(!output.contains("Searching for"));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn required_registry_failure_is_fatal_but_optional_fixture_failure_is_not() {
    tokio::time::timeout(WAIT, async {
        let root = tempfile::tempdir().unwrap();
        let (owner, console) = owner(root.path()).await;
        let pool = WorkerPool::new(2, 65536);
        let broken = vosu_core::plugin::BundledPlugin {
            manifest: "not a manifest",
            settings_schema: None,
            vtable: vosu_osu::vosu_plugin_vtable,
        };
        assert!(
            dev_plugins::load_bundled(
                &owner.plugins,
                &owner.paths,
                &owner.logger,
                &pool,
                std::slice::from_ref(&broken)
            )
            .is_err()
        );
        assert!(!owner.plugins.has_enabled_matchers());
        let entries = [
            vosu_core::plugin::BundledPlugin {
                manifest: vosu_osu::MANIFEST,
                settings_schema: Some(vosu_osu::SETTINGS_SCHEMA),
                vtable: vosu_osu::vosu_plugin_vtable,
            },
            broken,
        ];
        assert_eq!(
            dev_plugins::load_bundled(&owner.plugins, &owner.paths, &owner.logger, &pool, &entries)
                .unwrap(),
            2
        );
        assert!(owner.plugins.has_enabled_matchers());
        owner.shutdown().await.unwrap();
        assert!(
            console
                .0
                .lock()
                .unwrap()
                .contains("optional bundled plugin at index 1 failed")
        );
    })
    .await
    .unwrap();
}

struct Source(Arc<AtomicBool>);
impl Drop for Source {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl ProcessSource for Source {
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
        Ok(vec![ProcessRecord {
            pid: 0x7c30_0001,
            name: "osu!.exe".into(),
            path: "/missing/c3/osu!.exe".into(),
            command_line: "osu!.exe".into(),
            argv: Some(vec!["osu!.exe".into()]),
            bitness: vosu_plugin_api::Bitness::X86,
            start_time: 1,
        }])
    }
    fn is_alive(&self, _: u32) -> bool {
        true
    }
}

struct ObserveDetach {
    bridge: Arc<bridge_b1::LegacyBridge>,
    discovery_stopped: Arc<AtomicBool>,
    ordered_detach: AtomicBool,
}
impl LegacySink<B1Event, B1Inputs> for ObserveDetach {
    fn inputs(&self) -> watch::Receiver<B1Inputs> {
        self.bridge.inputs()
    }
    fn send(&self, event: B1Event) {
        if let B1Event::Detached { key } = &event {
            // A bridge closed before destroy has already cleared its roster;
            // a discovery service stopped after destroy still owns its Source.
            let was_attached = self
                .bridge
                .lifecycle()
                .borrow()
                .sessions
                .iter()
                .any(|s| s.pid == key.pid && s.generation == key.generation);
            self.ordered_detach.store(
                was_attached && self.discovery_stopped.load(Ordering::Acquire),
                Ordering::Release,
            );
        }
        self.bridge.send(event);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bundled_osu_discovery_and_real_runtime_shutdown_are_joined() {
    tokio::time::timeout(WAIT, async {
        let root = tempfile::tempdir().unwrap();
        let (mut owner, console) = owner(root.path()).await;
        let (_demand, demand) = watch::channel(Default::default());
        let bridge = bridge_b1::LegacyBridge::new(
            owner.config.subscribe_current(),
            demand,
            owner.snapshots.clone(),
            owner.logger.clone(),
        );
        let stopped = Arc::new(AtomicBool::new(false));
        let sink = Arc::new(ObserveDetach {
            bridge: bridge.clone(),
            discovery_stopped: stopped.clone(),
            ordered_detach: AtomicBool::new(false),
        });
        *owner.bridge_installed.get_mut() = Some(vosu_surface::install(sink.clone()).unwrap());
        *owner.bridge.get_mut() = Some(bridge.start(Arc::new(
            vosu_core::plugin::SyntheticForegroundSource::new(None),
        )));
        let mut lifecycle = bridge.lifecycle();
        dev_plugins::load_all(&owner.plugins, vec![], &owner.paths, &owner.logger).unwrap();
        let source = Arc::new(super::super::discovery::operational::LoggedSource::new(
            Arc::new(Source(stopped)),
            owner.logger.clone(),
        ));
        *owner.discovery.get_mut() = Some(DiscoveryService::start(
            &owner.plugins,
            source,
            Handle::current(),
        ));
        while lifecycle.borrow_and_update().sessions.is_empty() {
            lifecycle.changed().await.unwrap();
        }
        assert_eq!(lifecycle.borrow().sessions[0].pid, 0x7c30_0001);
        owner.shutdown().await.unwrap();
        assert!(
            sink.ordered_detach.load(Ordering::Acquire),
            "discovery must stop before plugin detach, while the bridge remains open"
        );
        assert!(lifecycle.borrow().sessions.is_empty());
        assert!(owner.discovery.lock().await.is_none());
        assert!(owner.bridge.lock().await.is_none());
        assert!(vosu_surface::current::<B1Event, B1Inputs>().is_none());
        {
            let output = console.0.lock().unwrap();
            assert!(output.contains(&format!(
                "Found osu! process PID {} (Stable); probing memory",
                0x7c30_0001
            )));
            assert!(
                output.contains("Initial process discovery completed: 1 osu! process candidate(s)")
            );
        }
        lifecycle.borrow_and_update();
        drop(sink);
        drop(bridge);
        assert!(lifecycle.changed().await.is_err());
    })
    .await
    .unwrap();
}
