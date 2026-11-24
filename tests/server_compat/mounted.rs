//! The production overlay listener, dispatcher, `ProductionRouteDelegate`,
//! surface registrations, bus-backed providers and hubs, mounted over a real
//! osu! producer/ABI/core fixture with distinct nonzero source values.
//!
//! Only the bundled plugin set is a fixture: the osu! plugin publishes through
//! its production projection, and the three adapters are the real bundled
//! manifests and vtables. Routes are registered from the production
//! `adapter_routes` table that `Adapters::mount` also uses, never a copy.
use super::*;
#[path = "../state_api_compat/oracle.rs"]
#[expect(
    dead_code,
    reason = "Shared source-oracle helpers also serve the full parity inventory"
)]
pub(super) mod oracle;
#[path = "../state_bus_compat/seeds.rs"]
#[expect(
    dead_code,
    reason = "Shared nonzero seeds; the state-bus coverage helper is unused here"
)]
pub(super) mod seeds;
pub(super) use futures_util::{SinkExt, StreamExt};
use oracle::adapter_fixture::Fixture;
pub(super) use oracle::adapter_fixture::host::WAIT;
use vosu::{
    runtime::{ListenerOwner, WebSocketHubs},
    server::{
        api_v1::{router::ApiV1Router, subscriptions::ApiV1SubscriptionHub},
        bus_provider::BusStateProvider,
        origins::{OriginAuthorizer, Origins},
        pages::{DynamicPage, EmbeddedResponse},
        service::{
            BeatmapFiles, ConfigUpdateService, CurrentBeatmapProvider, DeclaredCalculateProvider,
            PageDataProvider, ProductionRouteDelegate, ProductionServices, ReportInputProvider,
            SurfaceRouter, UpdateRouteResult, UpdaterHandoff, WebSocketEndpoints,
            WebSocketRelaySink, adapter_routes, reserve_core_routes,
        },
        websocket::StateDemand,
    },
};
use vosu_core::plugin::{PluginHost, PluginInstanceId};
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_state::Delivery;
use vosu_surface::routes::RegistrationLease;

pub(super) type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
pub(super) use tokio_tungstenite::tungstenite::Message as WsMessage;

/// `Adapters`' schema dispatch, over the same four providers per schema.
pub(super) struct Schemas(pub [Arc<BusStateProvider>; 4]);
impl Schemas {
    pub(super) fn get(&self, schema: Schema) -> &Arc<BusStateProvider> {
        &self.0[match schema {
            Schema::V1 => 0,
            Schema::StreamCompanion => 1,
            Schema::V2 => 2,
            Schema::V2Precise => 3,
        }]
    }
}
impl StateProvider for Schemas {
    fn poll_rate(&self, schema: Schema) -> Duration {
        self.get(schema).poll_rate(schema)
    }
    fn subscribe(&self, schema: Schema) -> watch::Receiver<u64> {
        self.get(schema).subscribe(schema)
    }
    fn capture(&self, schema: Schema) -> Option<Arc<dyn StateSnapshot>> {
        self.get(schema).capture(schema)
    }
    fn demand(
        &self,
        schema: Schema,
        filters: &[Filter],
    ) -> Result<Option<Box<dyn StateDemand>>, String> {
        self.get(schema).demand(schema, filters)
    }
}
impl StateJsonProvider for Schemas {
    fn cached_json(&self, route: StateRoute) -> ProviderFuture<'_, CachedJson> {
        self.get(schema_of(route)).cached_json(route)
    }
}
pub(super) fn schema_of(route: StateRoute) -> Schema {
    match route {
        StateRoute::V1 => Schema::V1,
        StateRoute::StreamCompanion => Schema::StreamCompanion,
        StateRoute::V2 => Schema::V2,
        StateRoute::V2Precise => Schema::V2Precise,
    }
}

struct Unused;
impl PageDataProvider for Unused {
    fn render(
        &self,
        _: DynamicPage,
        _: Option<String>,
    ) -> ProviderFuture<'_, Result<EmbeddedResponse, String>> {
        Box::pin(async { Err("unused".into()) })
    }
    fn search_counters(
        &self,
        _: &str,
        _: bool,
        _: Option<String>,
    ) -> ProviderFuture<'_, Result<Bytes, String>> {
        Box::pin(async { Err("unused".into()) })
    }
}
impl ConfigUpdateService for Unused {
    fn save(&self, _: Value) -> ProviderFuture<'_, Result<(), String>> {
        Box::pin(async { Err("unused".into()) })
    }
}
impl ReportInputProvider for Unused {
    fn capture(&self) -> Result<vosu_core::report::ReportInput, String> {
        Err("unused".into())
    }
}
impl UpdaterHandoff for Unused {
    fn run_update(&self, _: bool) -> ProviderFuture<'_, Result<UpdateRouteResult, String>> {
        Box::pin(async { Err("unused".into()) })
    }
}
struct NoBeatmap;
impl CurrentBeatmapProvider for NoBeatmap {
    fn files(&self) -> BeatmapFiles {
        BeatmapFiles::default()
    }
}

/// Registers the production adapter route table under the fixture's
/// upstream (2), gosuMemory (3) and streamCompanion (4) plugin instances.
fn mount(
    host: &Arc<PluginHost>,
    schemas: &Schemas,
    hubs: &WebSocketHubs,
    files: &vosu_adapter_upstream::files::Provider,
) -> Vec<RegistrationLease> {
    let groups = adapter_routes(
        std::array::from_fn(|index| schemas.0[index].clone() as Arc<dyn StateJsonProvider>),
        [
            hubs.v1.clone(),
            hubs.stream_companion.clone(),
            hubs.v2.clone(),
            hubs.v2_precise.clone(),
        ],
        files,
    );
    let mut leases = vec![reserve_core_routes(host).unwrap()];
    for (index, group) in groups.into_iter().enumerate() {
        let requester = host
            .surface_requester(PluginInstanceId::new(index as u64 + 2))
            .unwrap();
        leases.push(requester.register_routes(group).unwrap());
    }
    leases
}

pub(super) fn seeded_play() -> oracle::Input {
    let original: oracle::Input = serde_json::from_slice(
        &std::fs::read(oracle::fixture_root().join("stable-play-osu-fl/input.json")).unwrap(),
    )
    .unwrap();
    seeds::seeded(&original)
}

/// Fields drop in order: transport leases and services before the publisher,
/// and the publisher before the fixture that owns the plugin host.
pub(super) struct Mounted {
    leases: Vec<RegistrationLease>,
    pub input: oracle::Input,
    pub port: u16,
    pub base: String,
    pub origin: String,
    pub providers: RouteProviders,
    pub delegate: Arc<ProductionRouteDelegate>,
    pub hubs: Option<WebSocketHubs>,
    pub client: reqwest::Client,
    listener: Option<ListenerOwner>,
    api: ApiV1SubscriptionHub,
    processor: Arc<CommandProcessor>,
    relays: Arc<WebSocketRelaySink>,
    counters: Arc<vosu::counters::ReloadableCounterService>,
    _roots: TempDir,
    pub publisher: Option<Arc<vosu_osu::fixtures::Publisher>>,
    pub fixture: Fixture,
}
impl Mounted {
    /// `input: None` attaches no client, exercising the no-client envelope.
    pub(super) fn new(input: Option<oracle::Input>) -> Self {
        let sourced = input.clone().unwrap_or_else(seeded_play);
        let mut fixture = Fixture::with_chart(
            ClientType::Stable,
            oracle::chart_content(&sourced).as_deref(),
            None,
        );
        let publisher = input.as_ref().map(|input| {
            let publisher = fixture.attach(
                InstanceSettings {
                    tournament_spectator: false,
                    tournament_ipc_id: None,
                    devserver_endpoint: None,
                },
                false,
                true,
            );
            publisher
                .publish(oracle::state(input), Delivery::All)
                .unwrap();
            publisher
        });
        if publisher.is_some() {
            // Settle the chart-derived SC view before any transport reads it.
            assert!(matches!(fixture.outcome("sc"), CachedJson::Ready(_)));
        }
        let roots = TempDir::new().unwrap();
        std::fs::create_dir_all(roots.path().join("static/counter")).unwrap();
        std::fs::write(roots.path().join("static/counter/index.html"), "overlay").unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let origin = format!("http://127.0.0.1:{port}");
        let host = fixture.h.host.clone();
        let schemas = Arc::new(Schemas(fixture.providers.clone()));
        let files = fixture.files.clone();
        let (hubs, leases, api, processor, relays, counters, delegate, providers, listener) =
            fixture.h.runtime.block_on(async {
                let hubs = WebSocketHubs::start(schemas.clone());
                let leases = mount(&host, &schemas, &hubs, &files);
                let source = host.api_state_source();
                let api = ApiV1SubscriptionHub::new(source.clone(), Duration::ZERO);
                let relays = Arc::new(WebSocketRelaySink::new());
                let processor = Arc::new(CommandProcessor::new(
                    Arc::new(StaticSettings),
                    relays.clone(),
                ));
                let counters = Arc::new(vosu::counters::ReloadableCounterService::production(
                    roots.path().join("static"),
                    roots.path().join("settings"),
                    roots.path().join("cache"),
                ));
                let unused = Arc::new(Unused);
                let services = ProductionServices::with_native_report_probe(
                    vosu::server::pages::PageService,
                    unused.clone(),
                    counters.clone(),
                    Arc::new(NoBeatmap),
                    unused.clone(),
                    unused.clone(),
                    unused,
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
                let upstream = host.surface_requester(PluginInstanceId::new(2)).unwrap();
                let delegate = Arc::new(
                    ProductionRouteDelegate::new(services)
                        .with_surface(SurfaceRouter::new(host.clone(), upstream))
                        .with_api_v1(ApiV1Router::new(source, api.clone())),
                );
                let providers = RouteProviders {
                    authorization: Arc::new(OriginAuthorizer {
                        inner: Arc::new(AuthorizationPolicy::new(
                            "127.0.0.1,localhost",
                            "127.0.0.1",
                        )),
                        origins: Arc::new(
                            Origins::new(origin.clone(), "http://127.0.0.1:24051".into()).unwrap(),
                        ),
                        dashboard: false,
                    }),
                    state: schemas.clone(),
                    calculation: Arc::new(DeclaredCalculateProvider(files.clone())),
                    delegate: delegate.clone(),
                };
                let listener = ListenerOwner::start(
                    providers.clone(),
                    vosu::server::runtime::ListenTarget {
                        host: "127.0.0.1".into(),
                        port,
                    },
                )
                .await
                .unwrap();
                (
                    hubs, leases, api, processor, relays, counters, delegate, providers, listener,
                )
            });
        Self {
            publisher,
            fixture,
            input: sourced,
            port,
            base: origin.clone(),
            origin,
            providers,
            delegate,
            hubs: Some(hubs),
            client: reqwest::Client::builder()
                .no_proxy()
                .timeout(WAIT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            listener: Some(listener),
            api,
            processor,
            relays,
            counters,
            leases,
            _roots: roots,
        }
    }
    pub(super) fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        self.fixture
            .h
            .runtime
            .block_on(async { tokio::time::timeout(WAIT, future).await.expect("bounded") })
    }
    /// The in-process provider's exact bytes, which state_api_compat proves
    /// equal to the committed source oracle for unseeded inputs.
    pub(super) fn expected(&self, schema: &str) -> Bytes {
        match self.fixture.outcome(schema) {
            CachedJson::Ready(bytes) => bytes,
            other => panic!("{schema} not ready: {other:?}"),
        }
    }
    pub(super) async fn ws(&self, path: &str, origin: Option<&str>) -> Result<Ws, StatusCode> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut request = format!("ws://127.0.0.1:{}{path}", self.port)
            .into_client_request()
            .unwrap();
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
        }
        match tokio_tungstenite::connect_async(request).await {
            Ok((socket, response)) => {
                assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
                Ok(socket)
            }
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert!(!response.headers().contains_key(header::UPGRADE));
                Err(response.status())
            }
            Err(error) => panic!("{path}: {error}"),
        }
    }
    /// The focused session's union of sampling demand across every subscriber.
    pub(super) fn demand(&self) -> Vec<String> {
        let host = &self.fixture.h.host;
        let handle = host.diagnostic_sessions()[0].handle;
        host.state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(vosu_core::state::SessionKey(handle), Some("play"))
            .selectors
            .iter()
            .map(|selector| format!("{selector:?}"))
            .collect()
    }
    pub(super) fn shutdown(mut self) {
        let listener = self.listener.take().unwrap();
        let hubs = self.hubs.take().unwrap();
        let (api, processor, relays, counters) = (
            self.api.clone(),
            self.processor.clone(),
            self.relays.clone(),
            self.counters.clone(),
        );
        self.block_on(async move {
            listener.stop_accepting();
            hubs.shutdown().await;
            listener.join().await;
            api.shutdown();
            processor.shutdown().await;
            relays.shutdown().await;
            counters.shutdown().await;
        });
        self.leases.clear();
    }
}

/// Reads the next text frame, skipping control frames.
pub(super) async fn text(socket: &mut Ws) -> String {
    loop {
        match socket.next().await.expect("open socket").expect("frame") {
            WsMessage::Text(text) => return text.to_string(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => {}
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}
