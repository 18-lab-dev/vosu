//! Concrete legacy projections; core owns the subscribers and their routes.
use std::{io, sync::Arc};

use vosu_core::{
    runtime::{AdapterRequesters, AdapterServices, Projections},
    server::routes::ProviderFuture,
};

mod routes;
pub(super) use routes::adapter_routes;

pub(super) fn projections(requesters: &AdapterRequesters) -> Projections {
    let chart = Arc::new(vosu_adapter_streamcompanion::provider::Provider::new(
        requesters.stream_companion.clone(),
    ));
    Projections {
        v1: Arc::new(vosu_adapter_gosumemory::provider::Provider),
        stream_companion: chart.clone(),
        v2: Arc::new(vosu_adapter_upstream::provider::Regular),
        v2_precise: Arc::new(vosu_adapter_upstream::provider::Precise),
        services: Box::new(Services {
            files: vosu_adapter_upstream::files::Provider::new(requesters.upstream.clone()),
            chart,
        }),
    }
}

struct Services {
    files: vosu_adapter_upstream::files::Provider,
    chart: Arc<vosu_adapter_streamcompanion::provider::Provider>,
}

impl AdapterServices for Services {
    fn client_version(&self, session: &str) -> Option<String> {
        self.files.client_version(session).ok()
    }

    fn shutdown(&self) -> ProviderFuture<'_, io::Result<()>> {
        Box::pin(async { self.chart.shutdown().await.map(|_| ()).map_err(io::Error::other) })
    }
}
