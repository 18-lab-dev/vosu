use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::StreamExt;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use vosu::{
    app::{
        App, EnvironmentSnapshot, HookFuture, RuntimeHooks, StartupArgs, StartupOutcome,
        StartupPlatform,
    },
    config::{Config, ConfigChanged, ConfigStore},
    counters::{CounterOpener, CounterService, DownloadRequest, HardenedCounterDownloader},
    net::{Deadlines, DownloadLimits, NetworkError, StreamingClient},
    runtime::{TestProductionCompositionOptions, start_test_production_composition},
    server::runtime::ListenTarget,
    server::service::{DeferredUpdateTrigger, UpdateRouteResult, UpdaterHandoff},
};
use vosu_test_harness::{MockNetwork, USER_AGENT};

#[derive(Default)]
struct RecordingOpener(AtomicUsize);
impl CounterOpener for RecordingOpener {
    fn open(&self, path: &Path) -> io::Result<()> {
        assert!(
            path.is_dir(),
            "only an installed counter directory may be opened"
        );
        self.0.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

fn deadlines() -> Deadlines {
    Deadlines {
        connect: Duration::from_secs(1),
        idle: Duration::from_millis(150),
        total: Duration::from_secs(2),
    }
}

async fn run_updates(State(updater): State<Arc<dyn UpdaterHandoff>>) -> Response {
    match updater.run_update().await {
        Ok(UpdateRouteResult::Latest) => {
            Json(serde_json::json!({"status":"latest"})).into_response()
        }
        Ok(UpdateRouteResult::NoFiles) => {
            Json(serde_json::json!({"status":"noFiles"})).into_response()
        }
        Ok(UpdateRouteResult::Updated(_)) => {
            Json(serde_json::json!({"status":"updated"})).into_response()
        }
        Err(error) => Json(serde_json::json!({"error":error})).into_response(),
    }
}

#[tokio::test]
async fn exact_listener_policy_drives_all_harness_endpoints_and_counter_lifecycle() {
    let mock = MockNetwork::start().await.expect("start loopback fixture");
    let client = StreamingClient::for_test_network(
        mock.test_network_config()
            .expect("valid exact-listener config"),
        deadlines(),
        DownloadLimits::default(),
    );
    let cancel = CancellationToken::new();

    let catalog: serde_json::Value = client
        .request_json(&mock.endpoints().catalog, USER_AGENT, &cancel)
        .await
        .expect("catalog through reserved host");
    assert_eq!(catalog[0]["name"], "Harness Counter");

    let release: serde_json::Value = client
        .request_json(&mock.endpoints().release_latest, USER_AGENT, &cancel)
        .await
        .expect("release endpoint through reserved host");
    assert_eq!(release["tag_name"], "v0.1.0");

    let offsets: serde_json::Value = client
        .request_json(
            &format!("{}/2026.714.0.json", mock.endpoints().offsets_base),
            USER_AGENT,
            &cancel,
        )
        .await
        .expect("offset endpoint through reserved host");
    assert_eq!(offsets["OsuVersion"], "2026.714.0");

    let roots = TempDir::new().expect("temporary roots");
    let update_program = roots.path().join("update-program");
    std::fs::create_dir_all(&update_program).unwrap();
    let update_target = update_program.join(if cfg!(windows) { "vosu.exe" } else { "vosu" });
    std::fs::write(&update_target, b"unchanged harness executable").unwrap();
    let updater = vosu::runtime::test_updater_handoff(
        update_target.clone(),
        update_program.clone(),
        CancellationToken::new(),
        mock.test_network_config()
            .expect("updater exact-listener config"),
    )
    .expect("construct injected updater handoff");
    let update_listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind in-process application route");
    let update_address = update_listener.local_addr().unwrap();
    let update_stop = CancellationToken::new();
    let update_shutdown = update_stop.clone();
    let update_app = Router::new()
        .route("/api/runUpdates", get(run_updates))
        .with_state(updater);
    let update_task = tokio::spawn(async move {
        axum::serve(update_listener, update_app)
            .with_graceful_shutdown(update_shutdown.cancelled_owned())
            .await
    });
    let route_client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = route_client
        .get(format!("http://{update_address}/api/runUpdates"))
        .send()
        .await
        .expect("call in-process update HTTP route");
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap(),
        serde_json::json!({"status":"latest"})
    );
    update_stop.cancel();
    update_task.await.unwrap().unwrap();
    let update_residue: Vec<_> = std::fs::read_dir(update_program.join(".cache"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    assert!(
        update_residue.is_empty(),
        "updater transaction residue: {update_residue:?}"
    );
    let static_root = roots.path().join("static");
    let settings_root = roots.path().join("settings");
    let cache_root = roots.path().join("cache");
    let opener = Arc::new(RecordingOpener::default());
    let downloader = Arc::new(HardenedCounterDownloader::with_client(
        client.clone(),
        cache_root.clone(),
    ));
    let counters = CounterService::new(
        static_root.clone(),
        settings_root,
        opener.clone(),
        downloader,
    );
    counters
        .download(&DownloadRequest {
            url: mock.endpoints().counter_redirect.clone(),
            name: "harness-counter".to_owned(),
            update: false,
        })
        .await
        .expect("download and atomically extract counter");

    assert_eq!(
        std::fs::read_to_string(static_root.join("harness-counter/assets/value.txt")).unwrap(),
        "downloaded through exact-listener policy"
    );
    let discovered = counters.discover().expect("discover installed counter");
    assert_eq!(discovered.len(), 1);
    assert_eq!(discovered[0].name, "Harness Counter");
    counters
        .open("harness-counter")
        .expect("open installed counter");
    assert_eq!(opener.0.load(Ordering::Acquire), 1);
    counters
        .delete("harness-counter")
        .expect("delete installed counter");
    assert!(!static_root.join("harness-counter").exists());
    counters.shutdown().await;

    let requests = mock.requests();
    assert!(requests.iter().all(|request| {
        request
            .user_agent
            .as_deref()
            .is_some_and(|value| value.starts_with("vosu/0.1.0"))
    }));
    assert!(
        requests
            .iter()
            .any(|request| request.path_and_query == "/downloads/example-counter.zip?token=a%26b"),
        "redirect must preserve escaped query characters"
    );
    assert!(
        !cache_root.exists() || std::fs::read_dir(&cache_root).unwrap().next().is_none(),
        "temporary counter archives must be cleaned"
    );
    mock.shutdown()
        .await
        .expect("fixture drains without active requests");
}

#[tokio::test]
async fn production_policy_rejects_identical_loopback_url_and_harness_is_exact() {
    let mock = MockNetwork::start().await.expect("start loopback fixture");
    let cancel = CancellationToken::new();
    let production = StreamingClient::production()
        .with_deadlines_and_limits(deadlines(), DownloadLimits::default());
    let rejection = production
        .request_bytes(&mock.endpoints().counter_zip, USER_AGENT, &cancel)
        .await;
    assert!(
        rejection.is_err(),
        "production policy accepted a harness-only URL"
    );
    assert!(
        mock.requests().is_empty(),
        "production policy reached the loopback listener"
    );

    let wrong_port = mock.address().port().checked_add(1).unwrap();
    let wrong_url = format!("http://vosu-harness.invalid:{wrong_port}/api.json");
    let harness = StreamingClient::for_test_network(
        mock.test_network_config()
            .expect("valid exact-listener config"),
        deadlines(),
        DownloadLimits::default(),
    );
    assert!(matches!(
        harness.request_bytes(&wrong_url, USER_AGENT, &cancel).await,
        Err(NetworkError::Destination(message)) if message.contains("allowlist")
    ));

    let status_url = format!(
        "http://vosu-harness.invalid:{}/status/503",
        mock.address().port()
    );
    let response = harness
        .request_bytes(&status_url, USER_AGENT, &cancel)
        .await
        .expect("status response");
    assert_eq!(response.status, 503);
    assert!(
        response.body.is_empty(),
        "error response bodies are never buffered"
    );

    let slow_url = format!("http://vosu-harness.invalid:{}/slow", mock.address().port());
    assert!(matches!(
        harness.request_bytes(&slow_url, USER_AGENT, &cancel).await,
        Err(NetworkError::Timeout)
    ));
    mock.shutdown()
        .await
        .expect("timed-out request is released before shutdown completes");
}

struct InProcessHooks {
    client: StreamingClient,
    catalog_url: String,
    cancel: CancellationToken,
    update_enabled: Option<bool>,
    runtime_started: bool,
}

impl RuntimeHooks for InProcessHooks {
    fn warn<'a>(&'a mut self, message: &'a str) -> HookFuture<'a, ()> {
        Box::pin(async move { Err(io::Error::other(format!("unexpected warning: {message}"))) })
    }

    fn apply_update_decision(&mut self, enabled: bool) -> HookFuture<'_, ()> {
        Box::pin(async move {
            self.update_enabled = Some(enabled);
            Ok(())
        })
    }

    fn start_runtime(&mut self) -> HookFuture<'_, ()> {
        Box::pin(async move {
            let catalog: serde_json::Value = self
                .client
                .request_json(&self.catalog_url, USER_AGENT, &self.cancel)
                .await
                .map_err(io::Error::other)?;
            if catalog[0]["name"] != "Harness Counter" {
                return Err(io::Error::other("in-process catalog mismatch"));
            }
            self.runtime_started = true;
            Ok(())
        })
    }

    fn next_config_change(&mut self) -> HookFuture<'_, Option<ConfigChanged>> {
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn real_app_lifecycle_uses_only_the_injected_in_process_origin() {
    let mock = MockNetwork::start().await.expect("start loopback fixture");
    let client = StreamingClient::for_test_network(
        mock.test_network_config()
            .expect("valid exact-listener config"),
        deadlines(),
        DownloadLimits::default(),
    );
    let hooks = InProcessHooks {
        client,
        catalog_url: mock.endpoints().catalog.clone(),
        cancel: CancellationToken::new(),
        update_enabled: None,
        runtime_started: false,
    };
    let config = Config {
        enable_auto_update: false,
        open_dashboard_on_startup: false,
        ..Config::default()
    };
    let mut app = App::new(
        StartupArgs::parse([OsString::from("vosu"), OsString::from("--update=false")]),
        EnvironmentSnapshot::default(),
        PathBuf::from("/temporary/program/vosu"),
        StartupPlatform::Other,
        Arc::new(config),
        hooks,
    );
    assert_eq!(
        app.run().await.expect("run in-process App"),
        StartupOutcome::Started
    );
    assert_eq!(app.hooks().update_enabled, Some(false));
    assert!(app.hooks().runtime_started);
    assert_eq!(
        mock.requests().len(),
        1,
        "App must not contact any implicit endpoint"
    );
    mock.shutdown().await.expect("App fixture drains cleanly");
}

struct PreparedUpdate {
    triggered: CancellationToken,
}

impl UpdaterHandoff for PreparedUpdate {
    fn run_update(
        &self,
    ) -> vosu::server::routes::ProviderFuture<'_, Result<UpdateRouteResult, String>> {
        let triggered = self.triggered.clone();
        Box::pin(async move {
            Ok(UpdateRouteResult::Updated(DeferredUpdateTrigger::new(
                move || {
                    triggered.cancel();
                },
            )))
        })
    }
}

#[tokio::test]
async fn production_delegate_flushes_updated_response_before_shutdown_and_ws_1001() {
    let mock = MockNetwork::start()
        .await
        .expect("start reserved outbound fixture");
    let roots = TempDir::new().expect("temporary production composition roots");
    let config = Arc::new(
        ConfigStore::initialize(roots.path().join("config/vosu.env"))
            .await
            .expect("initialize production config"),
    );
    let triggered = CancellationToken::new();
    let composition = start_test_production_composition(TestProductionCompositionOptions {
        target: ListenTarget::new("127.0.0.1", 0),
        config,
        static_root: roots.path().join("static"),
        settings_root: roots.path().join("settings"),
        cache_root: roots.path().join("cache"),
        logs_root: roots.path().join("logs"),
        updater: Arc::new(PreparedUpdate {
            triggered: triggered.clone(),
        }),
        network: mock
            .test_network_config()
            .expect("exact-listener production network"),
    })
    .await
    .expect("start actual ProductionRouteDelegate listener");
    let address = composition.address();

    let (mut websocket, _) = tokio_tungstenite::connect_async(format!("ws://{address}/ws"))
        .await
        .expect("connect production v1 WebSocket");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .get(format!("http://{address}/api/runUpdates"))
        .send()
        .await
        .expect("call production update route");
    assert_eq!(response.status(), 200);
    let declared = response
        .content_length()
        .expect("updated response must publish Content-Length") as usize;
    let body = response
        .bytes()
        .await
        .expect("receive complete updated response");
    assert_eq!(body.len(), declared);
    assert_eq!(body.as_ref(), br#"{"status":"updated"}"#);

    tokio::time::timeout(Duration::from_secs(2), triggered.cancelled())
        .await
        .expect("flush barrier must release deferred shutdown after response");
    let shutdown = tokio::spawn(composition.shutdown());
    let close = tokio::time::timeout(Duration::from_secs(7), websocket.next())
        .await
        .expect("WebSocket close timeout")
        .expect("WebSocket ended without close frame")
        .expect("WebSocket close read error");
    match close {
        tokio_tungstenite::tungstenite::Message::Close(Some(frame)) => {
            assert_eq!(
                frame.code,
                tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Away
            );
        }
        other => panic!("expected WebSocket 1001 close, got {other:?}"),
    }
    shutdown.await.unwrap();

    let cache_entries: Vec<_> = std::fs::read_dir(roots.path().join("cache"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .collect();
    assert!(
        cache_entries.is_empty(),
        "prepared-update fixture left cache residue"
    );
    mock.shutdown()
        .await
        .expect("reserved fixture drains cleanly");
}
