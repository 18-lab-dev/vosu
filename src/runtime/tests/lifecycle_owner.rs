use super::*;
use vosu_core::logging::ConsoleSink;
use vosu_core::plugin::{DiscoveryService, ProcessRecord, ProcessSource, WorkerPool};

const WAIT: Duration = Duration::from_secs(10);

#[path = "shutdown_close.rs"]
mod shutdown_close;

#[cfg(feature = "test-harness")]
#[path = "lifecycle_owner/cli_broker.rs"]
mod cli_broker;

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
        struct NoState;
        impl WebSocketStateProvider for NoState {
            fn poll_rate(&self, _: WebSocketSchema) -> Duration {
                Duration::from_millis(10)
            }
            fn subscribe(&self, _: WebSocketSchema) -> watch::Receiver<u64> {
                watch::channel(0).1
            }
            fn capture(&self, _: WebSocketSchema) -> Option<Arc<dyn WebSocketStateSnapshot>> {
                None
            }
        }
        *owner.hubs.get_mut() = Some(WebSocketHubs::start(Arc::new(NoState)));
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
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: vosu_osu::vosu_plugin_vtable,
        };
        assert!(
            dev_plugins::load_bundled(
                &owner.plugins,
                &owner.paths,
                &owner.logger,
                &pool,
                std::slice::from_ref(&broken),
                &[],
                &mut plugin_management::Roster::default(),
            )
            .is_err()
        );
        assert!(!owner.plugins.has_enabled_matchers());
        let mut entries = crate::plugins::BUNDLED[..dev_plugins::REQUIRED.len()].to_vec();
        entries.push(broken);
        assert_eq!(
            dev_plugins::load_bundled(
                &owner.plugins,
                &owner.paths,
                &owner.logger,
                &pool,
                &entries,
                &[],
                &mut plugin_management::Roster::default(),
            )
            .unwrap(),
            5
        );
        assert!(owner.plugins.has_enabled_matchers());
        owner.shutdown().await.unwrap();
        assert!(
            console
                .0
                .lock()
                .unwrap()
                .contains("optional bundled plugin at index 4 failed")
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bundled_osu_discovery_and_real_runtime_shutdown_are_joined() {
    tokio::time::timeout(WAIT, async {
        let root = tempfile::tempdir().unwrap();
        let (mut owner, console) = owner(root.path()).await;
        let stopped = Arc::new(AtomicBool::new(false));
        dev_plugins::load_all(&owner.plugins, vec![], &owner.paths, &owner.logger).unwrap();
        let source = Arc::new(super::super::discovery::operational::LoggedSource::new(
            Arc::new(Source(stopped.clone())),
            owner.logger.clone(),
        ));
        *owner.discovery.get_mut() = Some(DiscoveryService::start(
            &owner.plugins,
            source,
            Handle::current(),
        ));
        loop {
            if !owner.plugins.diagnostic_sessions().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let sessions = owner.plugins.diagnostic_sessions();
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.pid, 0x7c30_0001);
        assert_eq!(session.bitness, vosu_plugin_api::Bitness::X86);
        assert_eq!(session.state, "starting reader");
        assert!(!session.session.is_empty());
        let plugins = owner.plugins.clone();
        owner
            .shutdown_with_plugin_destroy(async move {
                assert!(
                    stopped.load(Ordering::Acquire),
                    "discovery stops before plugin destruction"
                );
                tokio::task::spawn_blocking(move || {
                    plugins.destroy_all(dev_plugins::PLUGIN_DESTROY_GRACE)
                })
                .await
                .map_err(io::Error::other)
            })
            .await
            .unwrap();
        assert!(owner.plugins.diagnostic_sessions().is_empty());
        assert!(owner.discovery.lock().await.is_none());
        {
            let output = console.0.lock().unwrap();
            assert!(
                output.contains("Initial process discovery completed: 1 osu! process candidate(s)")
            );
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn bundled_adapter_registration_owns_routes_and_tears_down_feeds() {
    tokio::time::timeout(WAIT, async {
        let root = tempfile::tempdir().unwrap();
        let (owner, _) = owner(root.path()).await;
        let pool = WorkerPool::new(2, 65536);
        dev_plugins::load_bundled(
            &owner.plugins,
            &owner.paths,
            &owner.logger,
            &pool,
            crate::plugins::BUNDLED,
            &[],
            &mut plugin_management::Roster::default(),
        )
        .unwrap();
        let (adapters, hubs) = adapters::Adapters::start(&owner.plugins, owner.config.clone())
            .await
            .unwrap();
        let router = adapters.router(owner.plugins.clone());
        for path in [
            "/json",
            "/json/sc",
            "/json/v2",
            "/json/v2/precise",
            "/Songs/missing",
            "/files/beatmap/file",
            "/api/calculate/pp",
        ] {
            let request = axum::http::Request::builder()
                .uri(path)
                .body(axum::body::Body::empty())
                .unwrap();
            assert_eq!(
                router.http(&request).await.status(),
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "{path}"
            );
        }
        use vosu_surface::{
            listener::ListenerRole,
            routes::{ProviderResponse, RegistrationError, RouteDescriptor, RoutePath},
        };
        let requester = owner
            .plugins
            .surface_requester(vosu_core::plugin::PluginInstanceId::new(1))
            .unwrap();
        let conflict = requester.register_routes(vec![(
            RouteDescriptor {
                listener: ListenerRole::Overlay,
                method: "GET".into(),
                path: RoutePath::Exact("/api/v1/sessions".into()),
                websocket: false,
            },
            Arc::new(crate::server::service::RegisteredProvider::Calculate(
                Default::default(),
            )),
        )]);
        assert!(matches!(conflict, Err(RegistrationError::Collision)));
        let mut feeds = Vec::new();
        for path in ["/ws", "/tokens", "/websocket/v2", "/websocket/v2/precise"] {
            let request = axum::http::Request::builder()
                .uri(path)
                .body(axum::body::Body::empty())
                .unwrap();
            let Some(ProviderResponse::Feed(feed)) = router.dispatch(&request, true).await else {
                panic!("missing registered feed: {path}");
            };
            feeds.push(feed);
        }
        adapters.shutdown().await.unwrap();
        assert!(feeds.iter().all(|feed| feed.state.is_closed()));
        let request = axum::http::Request::builder()
            .uri("/json/v2")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            router.http(&request).await.status(),
            axum::http::StatusCode::NOT_FOUND
        );
        drop(feeds);
        hubs.shutdown().await;
        owner.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn every_required_adapter_is_mandatory_before_any_plugin_is_created() {
    tokio::time::timeout(WAIT, async {
        for omitted in 0..dev_plugins::REQUIRED.len() {
            let root = tempfile::tempdir().unwrap();
            let (owner, _) = owner(root.path()).await;
            let pool = WorkerPool::new(2, 65536);
            let mut entries = crate::plugins::BUNDLED[..dev_plugins::REQUIRED.len()].to_vec();
            entries.remove(omitted);
            assert!(
                dev_plugins::load_bundled(
                    &owner.plugins,
                    &owner.paths,
                    &owner.logger,
                    &pool,
                    &entries,
                    &[],
                    &mut plugin_management::Roster::default(),
                )
                .is_err(),
                "required slot {omitted}"
            );
            assert!(!owner.plugins.has_enabled_matchers());
            owner.shutdown().await.unwrap();
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn signal_task_panic_still_joins_config_and_logger_before_returning_error() {
    tokio::time::timeout(WAIT, async {
        let root = tempfile::tempdir().unwrap();
        let (mut owner, _) = owner(root.path()).await;
        let config = owner.config.clone();
        config.update(|current| current.poll_rate = 275.0);
        // Leave the worker idle, not waiting on a timer that might panic
        // during runtime drop and accidentally conceal a missing shutdown.
        config.flush().await.unwrap();
        *owner.signal_task.get_mut() = Some(tokio::spawn(async {
            panic!("injected signal shutdown panic");
        }));

        let error = owner.shutdown().await.unwrap_err();
        assert!(error.to_string().contains("signal task failed"));
        assert!(error.to_string().contains("injected signal shutdown panic"));
        // ConfigStore::shutdown closes the command receiver and awaits its
        // worker handle. Retain this Arc to rule out ordinary owner drop as
        // the reason the worker stopped.
        assert_eq!(
            config.flush().await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            owner.logger.flush().await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    })
    .await
    .expect("shutdown must finish after a signal task panic");
}

#[tokio::test]
async fn plugin_destroy_failure_still_joins_config_and_logger_before_returning_error() {
    tokio::time::timeout(WAIT, async {
        let root = tempfile::tempdir().unwrap();
        let (owner, _) = owner(root.path()).await;
        let config = owner.config.clone();
        config.flush().await.unwrap();

        let error = owner
            .shutdown_with_plugin_destroy(async {
                Err(io::Error::other("injected plugin destroy failure"))
            })
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected plugin destroy failure")
        );
        assert!(owner.shutdown_token.is_cancelled());
        assert_eq!(
            config.flush().await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            owner.logger.flush().await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    })
    .await
    .expect("shutdown must finish after a plugin destroy failure");
}
