//! One requester-bound provider per schema, shared by HTTP and WebSocket
//! consumers. The bundle supplies the projections and route table; core
//! owns the latest-only providers, the registrations and their shutdown.
use super::*;
use crate::plugin::PluginInstanceId;
use crate::server::bus_provider::BusStateProvider;
use crate::server::routes::StateJsonProvider;
use crate::server::websocket::StateDemand;

pub(super) struct Adapters {
    schemas: [Arc<BusStateProvider>; 4],
    pub(super) services: Box<dyn AdapterServices>,
    requesters: AdapterRequesters,
    routes: Mutex<Vec<vosu_surface::routes::RegistrationLease>>,
    config: Arc<ConfigStore>,
    bundle: &'static Bundle,
}
impl Adapters {
    /// `dashboard` is the A7.3 switch: off mounts no dashboard host routes.
    pub(super) async fn start(
        bundle: &'static Bundle,
        host: &Arc<PluginHost>,
        config: Arc<ConfigStore>,
        dashboard: bool,
    ) -> io::Result<(Arc<Self>, WebSocketHubs)> {
        let adapters = Arc::new(Self::new(bundle, host, config)?);
        let hubs = WebSocketHubs::start(adapters.clone());
        if let Err(error) = adapters.mount(host, &hubs, dashboard) {
            hubs.shutdown().await;
            adapters.shutdown().await?;
            return Err(error);
        }
        Ok((adapters, hubs))
    }
    /// The adapters are the three bundled plugins after the game provider
    /// (instances 1, 2 and 3).
    pub(super) fn new(
        bundle: &'static Bundle,
        host: &Arc<PluginHost>,
        config: Arc<ConfigStore>,
    ) -> io::Result<Self> {
        let requester = |index| {
            host.surface_requester(PluginInstanceId::new(index))
                .map_err(io::Error::other)
        };
        let requesters = AdapterRequesters {
            upstream: requester(1)?,
            gosumemory: requester(2)?,
            stream_companion: requester(3)?,
        };
        let projections = bundle.hosts.projections(&requesters);
        let current = config.current();
        let regular = bundle.hosts.poll_duration(current.poll_rate);
        let precise = bundle.hosts.poll_duration(current.precise_data_poll_rate);
        let provider = |schema,
                        requester: &Arc<dyn vosu_surface::requester::BundledRequester>,
                        projection,
                        cadence| {
            Arc::new(BusStateProvider::new(
                schema,
                requester.clone(),
                projection,
                cadence,
            ))
        };
        Ok(Self {
            schemas: [
                provider(
                    WebSocketSchema::V1,
                    &requesters.gosumemory,
                    projections.v1,
                    regular,
                ),
                provider(
                    WebSocketSchema::StreamCompanion,
                    &requesters.stream_companion,
                    projections.stream_companion,
                    regular,
                ),
                provider(
                    WebSocketSchema::V2,
                    &requesters.upstream,
                    projections.v2,
                    regular,
                ),
                provider(
                    WebSocketSchema::V2Precise,
                    &requesters.upstream,
                    projections.v2_precise,
                    precise,
                ),
            ],
            services: projections.services,
            requesters,
            routes: Mutex::new(Vec::new()),
            config,
            bundle,
        })
    }
    #[cfg(feature = "test-harness")]
    pub(super) fn provider(&self, schema: WebSocketSchema) -> &Arc<BusStateProvider> {
        &self.schemas[schema_index(schema)]
    }
    pub(super) fn router(&self, host: Arc<PluginHost>) -> crate::server::service::SurfaceRouter {
        crate::server::service::SurfaceRouter::new(host, self.requesters.upstream.clone())
    }
    pub(super) fn mount(
        &self,
        host: &Arc<PluginHost>,
        hubs: &WebSocketHubs,
        dashboard: bool,
    ) -> io::Result<()> {
        let groups = self.bundle.hosts.adapter_routes(
            std::array::from_fn(|index| self.schemas[index].clone() as Arc<dyn StateJsonProvider>),
            [
                hubs.v1.clone(),
                hubs.stream_companion.clone(),
                hubs.v2.clone(),
                hubs.v2_precise.clone(),
            ],
            self.requesters.upstream.clone(),
        );
        let mut leases = vec![
            crate::server::service::reserve_core_routes(host, dashboard).map_err(|error| {
                io::Error::other(format!("core route reservation failed: {error:?}"))
            })?,
        ];
        let owners = [
            &self.requesters.upstream,
            &self.requesters.gosumemory,
            &self.requesters.stream_companion,
        ];
        for (requester, group) in owners.into_iter().zip(groups) {
            leases.push(requester.register_routes(group).map_err(|error| {
                io::Error::other(format!("adapter registration failed: {error:?}"))
            })?);
        }
        // The dashboard host's page shells and assets (M7 D0). Off (A7.3), it
        // was never created, so it registers nothing.
        if dashboard {
            leases.push(super::dashboard::mount(self.bundle, host)?);
        }
        *self.routes.lock().unwrap() = leases;
        Ok(())
    }
    pub(super) async fn shutdown(&self) -> io::Result<()> {
        self.routes.lock().unwrap().clear();
        self.services.shutdown().await
    }
}
fn schema_index(schema: WebSocketSchema) -> usize {
    match schema {
        WebSocketSchema::V1 => 0,
        WebSocketSchema::StreamCompanion => 1,
        WebSocketSchema::V2 => 2,
        WebSocketSchema::V2Precise => 3,
    }
}
impl WebSocketStateProvider for Adapters {
    fn poll_rate(&self, schema: WebSocketSchema) -> Duration {
        let config = self.config.current();
        self.bundle
            .hosts
            .poll_duration(if schema == WebSocketSchema::V2Precise {
                config.precise_data_poll_rate
            } else {
                config.poll_rate
            })
    }
    fn subscribe(&self, schema: WebSocketSchema) -> watch::Receiver<u64> {
        self.schemas[schema_index(schema)].subscribe(schema)
    }
    fn capture(&self, schema: WebSocketSchema) -> Option<Arc<dyn WebSocketStateSnapshot>> {
        self.schemas[schema_index(schema)].capture(schema)
    }
    fn demand(
        &self,
        schema: WebSocketSchema,
        filters: &[Filter],
    ) -> Result<Option<Box<dyn StateDemand>>, String> {
        self.schemas[schema_index(schema)].demand(schema, filters)
    }
}
#[cfg(feature = "test-harness")]
impl crate::server::adapter_capture::endpoint::CaptureSource for Adapters {
    fn capture(
        &self,
        schema: WebSocketSchema,
    ) -> Result<crate::server::adapter_capture::AdapterCaptureSample, String> {
        self.provider(schema).capture_for_harness()
    }
}
