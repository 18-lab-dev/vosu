//! The production API v1 router over a test's `PluginHost`, served on a
//! loopback port (M5 I0): the same authorization, route dispatcher,
//! upgrade tracker, subscription hub and resource HTTP the daemon
//! composes, as `tests/api_v1/support.rs` serves it for in-process
//! plugins. Legacy state and calculation providers are never reached.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Request, State};
use axum::response::Response;
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;
use vosu::server::api_v1::resources::ResourceHttp;
use vosu::server::api_v1::router::ApiV1Router;
use vosu::server::api_v1::subscriptions::ApiV1SubscriptionHub;
use vosu::server::auth::AuthorizationPolicy;
use vosu::server::origins::{OriginAuthorizer, Origins};
use vosu::server::routes::{
    self, CachedJson, CalculatePpProvider, CalculateResult, Endpoint, ProviderFuture,
    RouteDelegate, RouteProviders, StateJsonProvider, StateRoute,
};
use vosu::server::websocket::UpgradeCallbackTracker;
use vosu_adapter_upstream::calculate::PpQuery;
use vosu_core::plugin::PluginHost;

use super::services::BOUND;

pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// `future`, failing the test past [`BOUND`].
pub async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("bounded API operation")
}

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

/// One served API, with its own runtime.
pub struct Api {
    /// `http://127.0.0.1:<port>`.
    pub origin: String,
    pub client: reqwest::Client,
    hub: ApiV1SubscriptionHub,
    stop: tokio_util::sync::CancellationToken,
    pub runtime: tokio::runtime::Runtime,
}

impl Api {
    /// Serves `host`'s API v1 with a resource deadline of `deadline`.
    pub fn new(host: &Arc<PluginHost>, deadline: Duration) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (origin, hub, stop) = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let hub = ApiV1SubscriptionHub::new(host.api_state_source(), Duration::ZERO);
            let origins = Arc::new(
                Origins::new(format!("http://{address}"), "http://127.0.0.1:24051".into()).unwrap(),
            );
            let providers = RouteProviders {
                authorization: Arc::new(OriginAuthorizer {
                    inner: Arc::new(AuthorizationPolicy::new("127.0.0.1,localhost", "127.0.0.1")),
                    origins,
                    dashboard: false,
                }),
                state: Arc::new(Legacy),
                calculation: Arc::new(Legacy),
                delegate: Arc::new(Delegate(
                    ApiV1Router::new(host.api_state_source(), hub.clone())
                        .with_resources(Arc::new(ResourceHttp::new(host, deadline))),
                )),
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
            tokio::spawn(async move {
                axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
                .unwrap();
            });
            (format!("http://{address}"), hub, stop)
        });
        Self {
            origin,
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(BOUND)
                .build()
                .unwrap(),
            hub,
            stop,
            runtime,
        }
    }

    /// `path` below `/api/v1`, or an absolute path such as a resource URL.
    pub fn url(&self, path: &str) -> String {
        if path.starts_with("/api/") {
            format!("{}{path}", self.origin)
        } else {
            format!("{}/api/v1{path}", self.origin)
        }
    }

    /// `GET` of a JSON API path: its status and body text.
    pub fn get(&self, path: &str) -> (u16, String) {
        self.runtime.block_on(bounded(async {
            let response = self.client.get(self.url(path)).send().await.unwrap();
            let status = response.status().as_u16();
            (status, response.text().await.unwrap())
        }))
    }

    /// `GET` of a JSON API path that must answer 200.
    pub fn json(&self, path: &str) -> Value {
        let (status, text) = self.get(path);
        assert_eq!(status, 200, "GET {path}: {text}");
        serde_json::from_str(&text).unwrap()
    }

    /// A WebSocket at `path` below `/api/v1`.
    pub async fn ws(&self, path: &str) -> Socket {
        bounded(tokio_tungstenite::connect_async(
            self.url(path).replacen("http:", "ws:", 1),
        ))
        .await
        .unwrap()
        .0
    }
}

/// Sends a pointer selection and returns the initial frame.
pub async fn select(socket: &mut Socket, pointers: &[&str]) -> Value {
    bounded(socket.send(Message::Text(
        serde_json::to_string(pointers).unwrap().into(),
    )))
    .await
    .unwrap();
    frame(socket).await
}

/// The next JSON frame.
pub async fn frame(socket: &mut Socket) -> Value {
    bounded(async {
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Pong(_) => {}
                other => panic!("expected a JSON frame, got {other:?}"),
            }
        }
    })
    .await
}

/// A ping answered by its pong: every frame the server queued before the
/// pong was already received, and none arrived (it fails otherwise).
pub async fn fence(socket: &mut Socket) {
    bounded(socket.send(Message::Ping(Bytes::from_static(b"i0-fence"))))
        .await
        .unwrap();
    match bounded(socket.next()).await.unwrap().unwrap() {
        Message::Pong(_) => {}
        other => panic!("expected only the pong, got {other:?}"),
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        self.hub.shutdown();
        self.stop.cancel();
    }
}
