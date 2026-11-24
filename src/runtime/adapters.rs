//! One requester-bound provider per schema, shared by HTTP and WebSocket consumers.
use super::*;
use crate::server::bus_provider::BusStateProvider;
use crate::server::websocket::StateDemand;
use vosu_core::plugin::PluginInstanceId;

pub(super) struct Adapters {
    schemas: [Arc<BusStateProvider>; 4],
    chart: Arc<vosu_adapter_streamcompanion::provider::Provider>,
    pub(super) files: vosu_adapter_upstream::files::Provider,
    requesters: [Arc<dyn vosu_surface::requester::BundledRequester>; 3],
    routes: Mutex<Vec<vosu_surface::routes::RegistrationLease>>,
    config: Arc<ConfigStore>,
}
impl Adapters {
    pub(super) async fn start(
        host: &Arc<PluginHost>,
        config: Arc<ConfigStore>,
    ) -> io::Result<(Arc<Self>, WebSocketHubs)> {
        let adapters = Arc::new(Self::new(host, config)?);
        let hubs = WebSocketHubs::start(adapters.clone());
        if let Err(error) = adapters.mount(host, &hubs) {
            hubs.shutdown().await;
            adapters.shutdown().await?;
            return Err(error);
        }
        Ok((adapters, hubs))
    }
    pub(super) fn new(host: &Arc<PluginHost>, config: Arc<ConfigStore>) -> io::Result<Self> {
        let requester = |index| {
            host.surface_requester(PluginInstanceId::new(index))
                .map_err(io::Error::other)
        };
        let upstream = requester(1)?;
        let gosumemory = requester(2)?;
        let streamcompanion = requester(3)?;
        let chart = Arc::new(vosu_adapter_streamcompanion::provider::Provider::new(
            streamcompanion.clone(),
        ));
        let current = config.current();
        let regular = poll_duration(current.poll_rate);
        let precise = poll_duration(current.precise_data_poll_rate);
        Ok(Self {
            requesters: [
                upstream.clone(),
                gosumemory.clone(),
                streamcompanion.clone(),
            ],
            routes: Mutex::new(Vec::new()),
            schemas: [
                Arc::new(BusStateProvider::new(
                    WebSocketSchema::V1,
                    gosumemory,
                    Arc::new(vosu_adapter_gosumemory::provider::Provider),
                    regular,
                )),
                Arc::new(BusStateProvider::new(
                    WebSocketSchema::StreamCompanion,
                    streamcompanion,
                    chart.clone(),
                    regular,
                )),
                Arc::new(BusStateProvider::new(
                    WebSocketSchema::V2,
                    upstream.clone(),
                    Arc::new(vosu_adapter_upstream::provider::Regular),
                    regular,
                )),
                Arc::new(BusStateProvider::new(
                    WebSocketSchema::V2Precise,
                    upstream.clone(),
                    Arc::new(vosu_adapter_upstream::provider::Precise),
                    precise,
                )),
            ],
            files: vosu_adapter_upstream::files::Provider::new(upstream),
            chart,
            config,
        })
    }
    fn provider(&self, schema: WebSocketSchema) -> &Arc<BusStateProvider> {
        &self.schemas[match schema {
            WebSocketSchema::V1 => 0,
            WebSocketSchema::StreamCompanion => 1,
            WebSocketSchema::V2 => 2,
            WebSocketSchema::V2Precise => 3,
        }]
    }
    pub(super) fn router(&self, host: Arc<PluginHost>) -> crate::server::service::SurfaceRouter {
        crate::server::service::SurfaceRouter::new(host, self.requesters[0].clone())
    }
    pub(super) fn mount(&self, host: &Arc<PluginHost>, hubs: &WebSocketHubs) -> io::Result<()> {
        use crate::server::service::RegisteredProvider as Provider;
        use vosu_surface::listener::ListenerRole;
        use vosu_surface::routes::{RouteDescriptor, RoutePath, RouteProvider};
        let mut groups: [Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>; 3] =
            std::array::from_fn(|_| Vec::new());
        let mut add =
            |owner: usize, path: RoutePath, method: &str, websocket: bool, provider: Provider| {
                groups[owner].push((
                    RouteDescriptor {
                        listener: ListenerRole::Overlay,
                        method: method.into(),
                        path,
                        websocket,
                    },
                    Arc::new(provider),
                ));
            };
        for (owner, schema, path, ws, hub) in [
            (1, WebSocketSchema::V1, "/json", "/ws", &hubs.v1),
            (
                2,
                WebSocketSchema::StreamCompanion,
                "/json/sc",
                "/tokens",
                &hubs.stream_companion,
            ),
            (
                0,
                WebSocketSchema::V2,
                "/json/v2",
                "/websocket/v2",
                &hubs.v2,
            ),
            (
                0,
                WebSocketSchema::V2Precise,
                "/json/v2/precise",
                "/websocket/v2/precise",
                &hubs.v2_precise,
            ),
        ] {
            let route = match schema {
                WebSocketSchema::V1 => StateRoute::V1,
                WebSocketSchema::StreamCompanion => StateRoute::StreamCompanion,
                WebSocketSchema::V2 => StateRoute::V2,
                WebSocketSchema::V2Precise => StateRoute::V2Precise,
            };
            add(
                owner,
                RoutePath::Exact(path.into()),
                "GET",
                false,
                Provider::State {
                    provider: self.provider(schema).clone(),
                    route,
                },
            );
            add(
                owner,
                RoutePath::Exact(ws.into()),
                "GET",
                true,
                Provider::Feed(hub.clone()),
            );
        }
        add(
            0,
            RoutePath::Exact("/api/calculate/pp".into()),
            "GET",
            false,
            Provider::Calculate(self.files.clone()),
        );
        for (path, methods) in [
            ("/backgroundImage", &["GET"][..]),
            ("/Songs", &["GET"][..]),
            ("/files/beatmap", &["GET", "HEAD"][..]),
            ("/files/skin", &["GET", "HEAD"][..]),
        ] {
            for method in methods {
                let matcher = if path == "/backgroundImage" {
                    RoutePath::Exact(path.into())
                } else {
                    RoutePath::Children(path.into())
                };
                add(
                    0,
                    matcher,
                    method,
                    false,
                    Provider::File {
                        provider: self.files.clone(),
                    },
                );
            }
        }
        let mut leases =
            vec![
                crate::server::service::reserve_core_routes(host).map_err(|error| {
                    io::Error::other(format!("core route reservation failed: {error:?}"))
                })?,
            ];
        for (requester, group) in self.requesters.iter().zip(groups) {
            leases.push(requester.register_routes(group).map_err(|error| {
                io::Error::other(format!("adapter registration failed: {error:?}"))
            })?);
        }
        *self.routes.lock().unwrap() = leases;
        Ok(())
    }
    pub(super) async fn shutdown(&self) -> io::Result<()> {
        self.routes.lock().unwrap().clear();
        self.chart
            .shutdown()
            .await
            .map(|_| ())
            .map_err(io::Error::other)
    }
}
impl StateJsonProvider for Adapters {
    fn cached_json(
        &self,
        route: StateRoute,
    ) -> crate::server::routes::ProviderFuture<'_, CachedJson> {
        let schema = match route {
            StateRoute::V1 => WebSocketSchema::V1,
            StateRoute::StreamCompanion => WebSocketSchema::StreamCompanion,
            StateRoute::V2 => WebSocketSchema::V2,
            StateRoute::V2Precise => WebSocketSchema::V2Precise,
        };
        self.provider(schema).cached_json(route)
    }
}
impl WebSocketStateProvider for Adapters {
    fn poll_rate(&self, schema: WebSocketSchema) -> Duration {
        let config = self.config.current();
        poll_duration(if schema == WebSocketSchema::V2Precise {
            config.precise_data_poll_rate
        } else {
            config.poll_rate
        })
    }
    fn subscribe(&self, schema: WebSocketSchema) -> watch::Receiver<u64> {
        self.provider(schema).subscribe(schema)
    }
    fn capture(&self, schema: WebSocketSchema) -> Option<Arc<dyn WebSocketStateSnapshot>> {
        self.provider(schema).capture(schema)
    }
    fn demand(
        &self,
        schema: WebSocketSchema,
        filters: &[Filter],
    ) -> Result<Option<Box<dyn StateDemand>>, String> {
        self.provider(schema).demand(schema, filters)
    }
}
