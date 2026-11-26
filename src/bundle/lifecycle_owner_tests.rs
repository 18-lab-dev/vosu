#![cfg(feature = "test-harness")]

use crate::{BUNDLE, BUNDLED};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::watch;
use vosu_core::{
    plugin::{ProcessRecord, ProcessSource},
    runtime::{TestDaemon, WebSocketHubs},
    server::websocket::{
        Schema as WebSocketSchema, StateProvider as WebSocketStateProvider,
        StateSnapshot as WebSocketStateSnapshot,
    },
};

const WAIT: Duration = Duration::from_secs(10);
const PLUGIN_DESTROY_GRACE: Duration = Duration::from_secs(5);

#[path = "lifecycle_owner_tests/dashboard_off.rs"]
mod dashboard_off;
#[path = "lifecycle_owner_tests/shutdown_close.rs"]
mod shutdown_close;

struct NoProcesses;
impl ProcessSource for NoProcesses {
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
        Ok(Vec::new())
    }
    fn is_alive(&self, _: u32) -> bool {
        false
    }
}

struct UnreachableRoute;
impl vosu_surface::routes::RouteProvider for UnreachableRoute {
    fn respond(
        &self,
        _: vosu_surface::routes::SurfaceRequest,
    ) -> vosu_surface::routes::ResponseFuture<'_, vosu_surface::routes::ProviderResponse> {
        unreachable!("the conflicting route was refused")
    }
}

#[tokio::test]
async fn required_plugin_failure_aborts_composition_and_shuts_down_owned_services() {
    tokio::time::timeout(WAIT, async {
        let root = tempfile::tempdir().unwrap();
        let mut owner = TestDaemon::new(root.path(), &BUNDLE).await;
        // Deterministically make create_instance fail, without a process-global
        // fault switch or a different production plugin loading path.
        owner.plugins().destroy_all(Duration::ZERO);
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
        owner.set_hubs(WebSocketHubs::start(Arc::new(NoState)));
        let error = owner.compose(Arc::new(NoProcesses)).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("required bundled plugin vosu.osu failed")
        );
        let outcome = owner.finish_run(Err(error)).await;
        assert!(
            outcome.is_err(),
            "startup failure must propagate to the CLI's non-zero exit path"
        );
        assert!(owner.shutdown_token().is_cancelled());
        assert!(owner.commands_hub().await.is_none());
        let output = owner.output();
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
        let owner = TestDaemon::new(root.path(), &BUNDLE).await;
        let broken = vosu_core::plugin::BundledPlugin {
            manifest: "not a manifest",
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: vosu_osu::vosu_plugin_vtable,
        };
        assert!(owner.load_bundled(std::slice::from_ref(&broken)).is_err());
        assert!(!owner.plugins().has_enabled_matchers());
        let mut entries = BUNDLED[..BUNDLE.required.len()].to_vec();
        entries.push(broken);
        assert_eq!(
            owner.load_bundled(&entries).unwrap(),
            BUNDLE.required.len() as u64 + 1
        );
        assert!(owner.plugins().has_enabled_matchers());
        owner.shutdown().await.unwrap();
        assert!(owner.output().contains(&format!(
            "optional bundled plugin at index {} failed",
            BUNDLE.required.len()
        )));
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
        let mut owner = TestDaemon::new(root.path(), &BUNDLE).await;
        let stopped = Arc::new(AtomicBool::new(false));
        owner.load_all(true).unwrap();
        owner.install_discovery(Arc::new(Source(stopped.clone())));
        loop {
            if !owner.plugins().diagnostic_sessions().is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        let sessions = owner.plugins().diagnostic_sessions();
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.pid, 0x7c30_0001);
        assert_eq!(session.bitness, vosu_plugin_api::Bitness::X86);
        assert_eq!(session.state, "starting reader");
        assert!(!session.session.is_empty());
        let plugins = owner.plugins().clone();
        owner
            .shutdown_with_plugin_destroy(async move {
                assert!(
                    stopped.load(Ordering::Acquire),
                    "discovery stops before plugin destruction"
                );
                tokio::task::spawn_blocking(move || plugins.destroy_all(PLUGIN_DESTROY_GRACE))
                    .await
                    .map_err(io::Error::other)
            })
            .await
            .unwrap();
        assert!(owner.plugins().diagnostic_sessions().is_empty());
        assert!(!owner.discovery_running().await);
        {
            let output = owner.output();
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
        let owner = TestDaemon::new(root.path(), &BUNDLE).await;
        owner.load_bundled(BUNDLED).unwrap();
        let mut adapters = owner.start_adapters(true).await.unwrap();
        let router = adapters.router(owner.plugins().clone());
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
            .plugins()
            .surface_requester(vosu_core::plugin::PluginInstanceId::new(1))
            .unwrap();
        let conflict = requester.register_routes(vec![(
            RouteDescriptor {
                listener: ListenerRole::Overlay,
                method: "GET".into(),
                path: RoutePath::Exact("/api/v1/sessions".into()),
                websocket: false,
            },
            Arc::new(UnreachableRoute),
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
        adapters.stop_adapters().await.unwrap();
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
        adapters.stop_hubs().await;
        owner.shutdown().await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn every_required_adapter_is_mandatory_before_any_plugin_is_created() {
    tokio::time::timeout(WAIT, async {
        for omitted in 0..BUNDLE.required.len() {
            let root = tempfile::tempdir().unwrap();
            let owner = TestDaemon::new(root.path(), &BUNDLE).await;
            let mut entries = BUNDLED[..BUNDLE.required.len()].to_vec();
            entries.remove(omitted);
            assert!(
                owner.load_bundled(&entries).is_err(),
                "required slot {omitted}"
            );
            assert!(!owner.plugins().has_enabled_matchers());
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
        let mut owner = TestDaemon::new(root.path(), &BUNDLE).await;
        let config = owner.config().clone();
        config.update(|current| current.poll_rate = 275.0);
        // Leave the worker idle, not waiting on a timer that might panic
        // during runtime drop and accidentally conceal a missing shutdown.
        config.flush().await.unwrap();
        owner.set_signal_task(tokio::spawn(async {
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
            owner.logger().flush().await.unwrap_err().kind(),
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
        let owner = TestDaemon::new(root.path(), &BUNDLE).await;
        let config = owner.config().clone();
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
        assert!(owner.shutdown_token().is_cancelled());
        assert_eq!(
            config.flush().await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            owner.logger().flush().await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    })
    .await
    .expect("shutdown must finish after a plugin destroy failure");
}
