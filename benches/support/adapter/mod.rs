//! Production adapter rig shared by D17 benchmarks and cost invariants:
//! source snapshot → real osu! projection/ABI batch → host bus commit →
//! coherent surface capture → real adapter → `BusStateProvider` → transport.
//! The including crate root declares `mod adapter_fixture` (tests/adapter_fixture).
mod charts;
pub mod counted;
pub mod input;
pub use crate::adapter_fixture::host::bounded;
use crate::adapter_fixture::{Fixture, host};
use bytes::Bytes;
pub use charts::Charts;
use counted::{Counted, Counts};
use std::{sync::Arc, time::Duration};
use vosu::server::bus_provider::BusStateProvider;
use vosu_core::server::{
    routes::{CachedJson, StateJsonProvider, StateRoute},
    websocket::{Connection, Outbound, OutboundReceiver, Schema, WebSocketHub},
};
use vosu_core::{plugin::PluginInstanceId, state::SessionKey};
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_state::{Delivery, StateBundleSnapshot};

/// Instance ids assigned by `adapter_fixture::Fixture` load order.
const UPSTREAM: u64 = 2;
const GOSUMEMORY: u64 = 3;
const STREAMCOMPANION: u64 = 4;
/// 1.x regular cadence. Delivery in these rigs is acknowledgement-driven; the
/// cadence only bounds a frame that no acknowledgement forced.
pub const CADENCE: Duration = Duration::from_millis(100);

pub fn settings() -> InstanceSettings {
    InstanceSettings {
        tournament_spectator: false,
        tournament_ipc_id: None,
        devserver_endpoint: None,
    }
}

pub struct Rig {
    pub fixture: Fixture,
    pub publishers: Vec<Arc<vosu_osu::fixtures::Publisher>>,
}
impl Rig {
    /// One selected stable session with the common nonzero D17 input committed.
    pub fn new() -> Self {
        Self::with(Fixture::new(ClientType::Stable), 1)
    }
    pub fn with(mut fixture: Fixture, sessions: usize) -> Self {
        let publishers = (0..sessions)
            .map(|_| fixture.attach(settings(), false, true))
            .collect();
        let rig = Self {
            fixture,
            publishers,
        };
        for publisher in &rig.publishers {
            publisher
                .publish(
                    input::snapshot(input::SCORES[0], input::K1_COUNTS[0]),
                    Delivery::All,
                )
                .unwrap();
        }
        // The resource worker commits its first references asynchronously,
        // possibly inside a caller's commit. Measure only after it settled.
        let deadline = std::time::Instant::now() + host::WAIT;
        while !rig.publishers.iter().all(|publisher| publisher.settled()) {
            assert!(
                std::time::Instant::now() < deadline,
                "osu! publication did not settle"
            );
            std::thread::yield_now();
        }
        rig
    }
    pub fn runtime(&self) -> &tokio::runtime::Runtime {
        &self.fixture.h.runtime
    }
    pub fn publish(&self, snapshot: Arc<StateBundleSnapshot>, delivery: Delivery) {
        self.publishers[0].publish(snapshot, delivery).unwrap();
    }
    /// A production provider over the real adapter, with work counters.
    pub fn provider(
        &self,
        schema: Schema,
        cadence: Duration,
    ) -> (Arc<BusStateProvider>, Arc<Counts>) {
        let (id, adapter): (u64, Arc<dyn vosu_surface::projection::Projection<_, Bytes>>) =
            match schema {
                Schema::V1 => (
                    GOSUMEMORY,
                    Arc::new(vosu_adapter_gosumemory::provider::Provider),
                ),
                Schema::V2 => (UPSTREAM, Arc::new(vosu_adapter_upstream::provider::Regular)),
                Schema::V2Precise => (UPSTREAM, Arc::new(vosu_adapter_upstream::provider::Precise)),
                Schema::StreamCompanion => (STREAMCOMPANION, self.fixture.chart.clone()),
            };
        let counted = Counted::new(adapter);
        let counts = Arc::clone(&counted.counts);
        let requester = self
            .fixture
            .h
            .host
            .surface_requester(PluginInstanceId::new(id))
            .unwrap();
        (
            Arc::new(BusStateProvider::new(schema, requester, counted, cadence)),
            counts,
        )
    }
    /// The production HTTP route payload; anything but cached bytes fails.
    pub fn http(&self, provider: &BusStateProvider, route: StateRoute) -> Bytes {
        match self.runtime().block_on(provider.cached_json(route)) {
            CachedJson::Ready(bytes) => bytes,
            CachedJson::NotReady => panic!("{route:?} is not ready"),
            CachedJson::Failed { body, .. } => {
                panic!("{route:?} failed: {}", String::from_utf8_lossy(&body))
            }
        }
    }
    pub fn hub(&self, schema: Schema, provider: Arc<BusStateProvider>) -> Hub {
        let hub = WebSocketHub::new(schema, provider);
        let run = self.runtime().spawn({
            let hub = hub.clone();
            async move { hub.run().await }
        });
        Hub {
            hub,
            run: Some(run),
        }
    }
    /// The osu! session's bus key, for host demand inspection.
    pub fn session_key(&self, index: usize) -> SessionKey {
        let state = self.fixture.h.host.state();
        let keys: Vec<_> = (1..=256)
            .map(SessionKey)
            .filter(|key| state.store.current(*key).is_some())
            .collect();
        keys[index]
    }
    /// Complete sampling selectors requested for a session by every subscriber.
    pub fn demand(&self, key: SessionKey) -> Vec<(Arc<str>, Arc<str>)> {
        self.fixture
            .h
            .host
            .state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(key, Some("play"))
            .selectors
    }
}

pub struct Hub {
    pub hub: WebSocketHub,
    run: Option<tokio::task::JoinHandle<()>>,
}
impl Hub {
    /// Connects a real hub client and waits for its first frame, leaving that
    /// frame in flight: later frames require the transport's acknowledgement.
    pub fn connect(&self, rig: &Rig, filters: Option<Vec<vosu_surface::filter::Filter>>) -> Client {
        let _enter = rig.runtime().enter();
        let (connection, receiver) = self.hub.connect();
        let mut client = Client {
            connection,
            receiver,
        };
        if let Some(filters) = filters {
            client.connection.set_filters(filters);
        }
        rig.runtime().block_on(client.frame());
        client
    }
    pub fn stop(mut self, rig: &Rig) {
        self.hub.shutdown();
        if let Some(run) = self.run.take() {
            rig.runtime()
                .block_on(host::bounded(run))
                .expect("hub run task");
        }
    }
}
impl Drop for Hub {
    fn drop(&mut self) {
        self.hub.shutdown();
    }
}

pub struct Client {
    pub connection: Connection,
    pub receiver: OutboundReceiver,
}
impl Client {
    pub async fn frame(&mut self) -> Bytes {
        match host::bounded(self.receiver.recv()).await {
            Some(Outbound::Text(bytes)) => bytes,
            frame => panic!("expected a state frame, got {frame:?}"),
        }
    }
    /// Transport write completion: the only way to release the in-flight slot.
    pub fn ack(&self) {
        self.connection.write_completed();
    }
}

/// Parses a JSON payload and returns the value at `pointer`.
pub fn json_at(bytes: &[u8], pointer: &str) -> serde_json::Value {
    let value: serde_json::Value = serde_json::from_slice(bytes).expect("JSON payload");
    value
        .pointer(pointer)
        .unwrap_or_else(|| panic!("{pointer} missing from {value}"))
        .clone()
}
