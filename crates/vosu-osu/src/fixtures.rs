//! Instance-owned production publication fixtures; no process-global snapshots.
use crate::{
    demand::SessionDemand,
    project::SessionFacts,
    publish::{
        BusPublisher, SessionHost,
        tournament::{Controls, Tournament},
    },
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64},
};
use vosu_osu_state::{Delivery, SnapshotPublisher, StateBundleSnapshot, StateWorkerError};
use vosu_plugin_api::{HostServices, Message, vosu_status};

#[cfg(all(feature = "test-harness", debug_assertions))]
mod parity;

/// The same optional M5 publication ingress for the staged process fixture
/// and the X0 bundled daemon. Only an explicit control in the plugin's
/// isolated data directory opts in; the normal producer needs no control.
#[cfg(all(feature = "test-harness", debug_assertions))]
pub struct FixtureOrProducer(FixtureMode);

#[cfg(all(feature = "test-harness", debug_assertions))]
enum FixtureMode {
    Producer(Box<crate::plugin::OsuPlugin>),
    Parity(Box<parity::Parity>),
}

#[cfg(all(feature = "test-harness", debug_assertions))]
impl vosu_plugin_api::ConcurrentPlugin for FixtureOrProducer {
    fn create(
        host: vosu_plugin_api::VtableHostServices,
        init: vosu_plugin_api::Init,
    ) -> Result<Self, vosu_plugin_api::PluginError> {
        let mode = match parity::Parity::load(host, &init.data_dir) {
            Some(parity) => FixtureMode::Parity(Box::new(parity)),
            None => FixtureMode::Producer(Box::new(crate::plugin::OsuPlugin::create(host, init)?)),
        };
        Ok(Self(mode))
    }

    fn on_message(
        &self,
        message: Message,
        correlation_id: u64,
    ) -> Result<(), vosu_plugin_api::PluginError> {
        match &self.0 {
            FixtureMode::Producer(plugin) => plugin.on_message(message, correlation_id),
            FixtureMode::Parity(parity) => parity.on_message(message, correlation_id),
        }
    }
}

pub struct Roster {
    tournament: Arc<Tournament>,
    resources: bool,
}
impl Default for Roster {
    fn default() -> Self {
        Self {
            tournament: Arc::default(),
            resources: true,
        }
    }
}
pub struct Publisher {
    bus: Arc<BusPublisher<SessionHost>>,
    demand: SessionDemand,
    roster: Arc<Tournament>,
    handle: u64,
}
impl Roster {
    /// Use a fixture plugin’s explicit resource provider instead of native client paths.
    pub fn without_resources() -> Self {
        Self {
            tournament: Arc::default(),
            resources: false,
        }
    }
    pub fn attach(
        &self,
        host: Arc<dyn HostServices + Send + Sync>,
        handle: u64,
        facts: SessionFacts,
        manager: bool,
        selected: bool,
    ) -> Arc<Publisher> {
        let settings = facts.settings.clone();
        let stable = facts.client == vosu_osu_model::enums::ClientType::Stable;
        let bus = Arc::new(BusPublisher::new(
            SessionHost(host.clone()),
            handle,
            Arc::new(AtomicBool::new(true)),
            facts,
        ));
        if self.resources {
            let refresh: Arc<dyn crate::resources::publication::RefreshPublication> = bus.clone();
            bus.resources
                .set(crate::resources::publication::Publication::start(
                    Arc::downgrade(&refresh),
                    host,
                    handle,
                    Arc::new(AtomicU64::new(1)),
                ))
                .ok()
                .expect("one resource owner");
        }
        let demand = SessionDemand::new();
        // Match plugin/registration.rs: only stable sessions join the manager
        // roster; lazer projects its own multi-spectating source directly.
        if stable {
            bus.tournament
                .set((self.tournament.clone(), handle))
                .ok()
                .expect("one roster");
            self.tournament.attach(
                &bus,
                handle,
                manager,
                settings,
                Controls {
                    selected: Arc::new(AtomicBool::new(selected)),
                    demand: demand.tournament.clone(),
                },
            );
        }
        Arc::new(Publisher {
            bus,
            demand,
            roster: self.tournament.clone(),
            handle,
        })
    }
}
impl Publisher {
    pub fn publish(
        &self,
        snapshot: Arc<StateBundleSnapshot>,
        delivery: Delivery,
    ) -> Result<(), StateWorkerError> {
        self.bus.publish(snapshot, delivery)
    }
    pub fn request(&self, message: Message, correlation: u64) -> Result<(), vosu_status> {
        self.bus
            .resources
            .get()
            .expect("resource owner")
            .request(message, correlation)
    }
    /// Whether asynchronous resource publication has committed and the
    /// publisher mailbox is idle, so the next publish commits on its caller.
    pub fn settled(&self) -> bool {
        self.bus.settled()
    }
    /// Stream-ownership routing, as the production plugin's `dispatch_resource`.
    pub fn owns_stream(&self, stream: u64) -> bool {
        self.bus
            .resources
            .get()
            .is_some_and(|resources| resources.owns_stream(stream))
    }
}
impl Drop for Publisher {
    fn drop(&mut self) {
        self.bus.deactivate();
        self.demand.cancel();
        self.roster.detach(self.handle);
        if let Some(resources) = self.bus.resources.get() {
            resources.cancel();
            resources.join();
        }
    }
}
