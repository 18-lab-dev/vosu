use axum::{
    body::Bytes,
    extract::{ConnectInfo, Request, State},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{future::Future, net::SocketAddr, sync::Arc, time::Duration};
use vosu::server::{
    api_v1::{router::ApiV1Router, subscriptions::ApiV1SubscriptionHub},
    auth::AuthorizationPolicy,
    origins::{OriginAuthorizer, Origins},
    routes::{
        self, CachedJson, CalculatePpProvider, CalculateResult, Endpoint, PpQuery, ProviderFuture,
        RouteDelegate, RouteProviders, StateJsonProvider, StateRoute,
    },
    websocket::UpgradeCallbackTracker,
};
use vosu_core::{
    api::SessionTarget,
    plugin::{self, PluginHost, PluginInstanceId},
};
use vosu_plugin_api::{
    HostServices, PublishBatchWriter, PublishValueRef, vosu_opcode, vosu_status,
};
use vosu_plugin_testkit::publisher::{self, PublisherEvent};

pub const WAIT: Duration = Duration::from_secs(15);
pub const SENTINEL: &str = "/private/S4-distinct-install-7913";
pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
pub use tokio_tungstenite::tungstenite::Message;

pub async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, future)
        .await
        .expect("bounded API test operation")
}

struct Storage;
impl plugin::settings::SettingsStorage for Storage {
    fn load(
        &self,
        _: &str,
    ) -> Result<serde_json::Map<String, Value>, plugin::settings::SettingsError> {
        Ok(Default::default())
    }
    fn store(
        &self,
        _: &str,
        _: &serde_json::Map<String, Value>,
    ) -> Result<(), plugin::settings::SettingsError> {
        Ok(())
    }
}
struct Console;
impl vosu_core::logging::ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}

pub struct Harness {
    pub host: Arc<PluginHost>,
    pub runtime: tokio::runtime::Runtime,
    pub foreground: Arc<plugin::SyntheticForegroundSource>,
    root: tempfile::TempDir,
    next: u64,
}
impl Harness {
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let logger = {
            let _enter = runtime.enter();
            vosu_core::logging::Logger::new(
                root.path(),
                vosu_core::logging::LoggerOptions::default(),
                Arc::new(Console),
            )
            .unwrap()
        };
        let foreground = Arc::new(plugin::SyntheticForegroundSource::new(None));
        let host = PluginHost::new_with_foreground(
            logger,
            Arc::new(plugin::settings::PluginSettingsService::new(Arc::new(
                Storage,
            ))),
            runtime.handle().clone(),
            foreground.clone(),
        );
        Self {
            host,
            runtime,
            foreground,
            root,
            next: 1,
        }
    }
    pub fn load(
        &mut self,
        manifest: &str,
        schemas: &[&str],
        actions: &[(&str, &str)],
        vtable: fn() -> vosu_plugin_api::vosu_plugin_vtable,
    ) {
        let loaded = plugin::load_bundled(&[plugin::BundledPlugin {
            manifest,
            settings_schema: None,
            namespace_schemas: schemas,
            action_schemas: actions,
            vtable,
        }])
        .unwrap();
        self.host
            .create_instance(
                PluginInstanceId::new(self.next),
                &loaded[0],
                self.root.path().into(),
                None,
            )
            .unwrap();
        self.next += 1;
    }
    pub fn publisher(
        &mut self,
        id: &str,
        pid: u32,
    ) -> (u64, std::sync::mpsc::Receiver<PublisherEvent>) {
        let manifest = format!(
            "{}\n[[provides]]\nid='field:{id}'\nversion='1.2.0'\nschema='state.json'\n[match]\nprocess_names=['{id}']\n",
            manifest(id)
        );
        publisher::register_manifest(id, &manifest);
        let events = publisher::register(id);
        let schema = if id == "vosu.osu" {
            include_str!("../../docs/v2/schemas/vosu.osu.schema.json").to_owned()
        } else {
            format!(r#"{{"title":"{id}","version":"1.2.0","type":"object","properties":{{}}}}"#)
        };
        self.load(&manifest, &[&schema], &[], publisher::vosu_plugin_vtable);
        self.host.process_appeared(record(pid, id));
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if let PublisherEvent::Attached { handle } = events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
            {
                return (handle, events);
            }
        }
    }
    pub fn id(&self) -> String {
        let value = self
            .host
            .api_state_source()
            .session(&SessionTarget::Active)
            .unwrap();
        let mut bytes = Vec::new();
        value.write_json(&mut bytes);
        serde_json::from_slice::<Value>(&bytes).unwrap()["session"]
            .as_str()
            .unwrap()
            .into()
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.host.destroy_all(WAIT);
    }
}
pub fn manifest(id: &str) -> String {
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='{id}'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n"
    )
}
pub fn record(pid: u32, id: &str) -> plugin::ProcessRecord {
    plugin::ProcessRecord {
        pid,
        name: id.into(),
        path: SENTINEL.into(),
        command_line: SENTINEL.into(),
        argv: Some(vec![SENTINEL.into()]),
        bitness: vosu_plugin_api::Bitness::X64,
        start_time: 7913,
    }
}
pub fn publish(id: &str, handle: u64, fields: &[(&str, PublishValueRef<'_>)]) {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    for (pointer, value) in fields {
        writer.push(id, pointer, *value);
    }
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    assert_eq!(
        publisher::host_services(id).send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes),
        vosu_status::VOSU_OK
    );
}

// Only legacy providers are inert. API requests pass through the production
// authorization/route dispatcher, API router, upgrade tracker and WS bridge.
struct Legacy;
impl StateJsonProvider for Legacy {
    fn cached_json(&self, _: StateRoute) -> ProviderFuture<'_, CachedJson> {
        Box::pin(async { panic!("legacy state route reached") })
    }
}
impl CalculatePpProvider for Legacy {
    fn calculate_pp(&self, _: PpQuery) -> ProviderFuture<'_, CalculateResult> {
        Box::pin(async { panic!("legacy calculation reached") })
    }
}
struct Delegate(ApiV1Router);
impl RouteDelegate for Delegate {
    fn handle(
        &self,
        endpoint: Endpoint,
        request: Request,
        _: Bytes,
    ) -> ProviderFuture<'_, Response> {
        assert_eq!(endpoint, Endpoint::ApiV1);
        Box::pin(self.0.handle(request))
    }
}
pub struct Server {
    pub base: String,
    pub client: reqwest::Client,
    pub hub: ApiV1SubscriptionHub,
    stop: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    pub async fn new(h: &Harness, dashboard: bool) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hub = ApiV1SubscriptionHub::new(h.host.api_state_source(), Duration::ZERO);
        let origins = Arc::new(
            Origins::new(format!("http://{address}"), "http://127.0.0.1:24051".into()).unwrap(),
        );
        let providers = RouteProviders {
            authorization: Arc::new(OriginAuthorizer {
                inner: Arc::new(AuthorizationPolicy::new("127.0.0.1,localhost", "127.0.0.1")),
                origins,
                dashboard,
            }),
            state: Arc::new(Legacy),
            calculation: Arc::new(Legacy),
            delegate: Arc::new(Delegate(ApiV1Router::new(
                h.host.api_state_source(),
                hub.clone(),
            ))),
        };
        let router = axum::Router::new()
            .fallback(
                |State(providers): State<RouteProviders>,
                 ConnectInfo(peer): ConnectInfo<SocketAddr>,
                 mut request: Request| async move {
                    request
                        .extensions_mut()
                        .insert(ConnectInfo(routes::PeerAddress(peer)));
                    routes::dispatch(State(providers), request).await
                },
            )
            .with_state(providers)
            .layer(axum::Extension(Arc::new(UpgradeCallbackTracker::new())));
        let stop = tokio_util::sync::CancellationToken::new();
        let stopped = stop.clone();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(stopped.cancelled_owned())
            .await
            .unwrap();
        });
        Self {
            base: format!("http://{address}/api/v1"),
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(WAIT)
                .build()
                .unwrap(),
            hub,
            stop,
            task,
        }
    }
    pub async fn get(&self, path: &str) -> (reqwest::StatusCode, Value) {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap();
        let status = response.status();
        assert_eq!(response.headers()["cache-control"], "no-store");
        (status, response.json().await.unwrap())
    }
    pub async fn ws(&self, path: &str) -> Socket {
        bounded(tokio_tungstenite::connect_async(format!(
            "{}{path}",
            self.base.replacen("http:", "ws:", 1)
        )))
        .await
        .unwrap()
        .0
    }
    pub async fn shutdown(self) {
        self.hub.shutdown();
        self.stop.cancel();
        bounded(self.task).await.unwrap();
    }
}
pub async fn select(socket: &mut Socket, pointers: &[&str]) -> Value {
    bounded(socket.send(Message::Text(json!(pointers).to_string().into())))
        .await
        .unwrap();
    frame(socket).await
}
pub async fn frame(socket: &mut Socket) -> Value {
    bounded(async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Pong(_) => {}
                other => panic!("expected JSON frame, got {other:?}"),
            }
        }
    })
    .await
}
pub async fn close_code(socket: &mut Socket) -> u16 {
    match bounded(socket.next()).await.unwrap().unwrap() {
        Message::Close(Some(close)) => u16::from(close.code),
        other => panic!("expected close, got {other:?}"),
    }
}
pub fn clean(value: impl std::fmt::Debug) {
    assert!(!format!("{value:?}").contains(SENTINEL));
}

pub mod f5 {
    use vosu_plugin_testkit::fixture::ApiFixturePlugin;
    vosu_plugin_api::export_plugin!(bundled ApiFixturePlugin);
}
