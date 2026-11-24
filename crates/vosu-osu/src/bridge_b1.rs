//! Interim latest-only bridge B1; removed with the 1.x surfaces in M4a.
use crate::{
    capture::{api_state, equivalent_state_snapshots},
    settings::OsuSettings,
};
use std::sync::{Arc, Mutex};
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_state::{
    Delivery, ReaderSettings, SnapshotPublisher, StateBundleSnapshot, StateWorkerError,
    SubscriptionDemand,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct B1SessionKey {
    pub pid: u32,
    pub generation: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum B1Phase {
    ResolvingOffsets,
    StartingReader,
    Reading,
}
pub enum B1Event {
    Attached {
        key: B1SessionKey,
        client: ClientType,
        settings: InstanceSettings,
        is_tournament_manager: bool,
    },
    Phase {
        key: B1SessionKey,
        phase: B1Phase,
    },
    Publish {
        key: B1SessionKey,
        api: Arc<StateBundleSnapshot>,
        live: Arc<StateBundleSnapshot>,
    },
    Detached {
        key: B1SessionKey,
    },
}
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct B1Inputs {
    pub demand: SubscriptionDemand,
}
impl B1Inputs {
    pub fn reader_settings(self, osu: OsuSettings) -> ReaderSettings {
        ReaderSettings {
            calculate_pp: osu.calculate_pp,
            poll_rate: osu.poll_rate,
            precise_data_poll_rate: osu.precise_data_poll_rate,
            show_mp_commands: osu.show_mp_commands,
            read_mania_scroll_speed: osu.read_mania_scroll_speed,
            collect_hit_errors: false,
        }
    }
}
pub type B1Sink = dyn vosu_surface::LegacySink<B1Event, B1Inputs>;

#[derive(Default)]
struct PublishedStates {
    api: Option<Arc<StateBundleSnapshot>>,
    live: Option<Arc<StateBundleSnapshot>>,
}

pub struct B1Publisher {
    key: B1SessionKey,
    sink: Arc<B1Sink>,
    last: Mutex<PublishedStates>,
}
impl B1Publisher {
    pub fn new(key: B1SessionKey, sink: Arc<B1Sink>) -> Self {
        Self {
            key,
            sink,
            last: Mutex::new(PublishedStates::default()),
        }
    }
}
impl SnapshotPublisher<Arc<StateBundleSnapshot>> for B1Publisher {
    fn publish(
        &self,
        snapshot: Arc<StateBundleSnapshot>,
        delivery: Delivery,
    ) -> Result<(), StateWorkerError> {
        let (api, first_publication) = {
            let mut last = self
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let api = api_state(last.api.as_ref(), &snapshot, delivery);
            let unchanged = |previous: &Option<Arc<StateBundleSnapshot>>, next| {
                previous
                    .as_ref()
                    .is_some_and(|previous| equivalent_state_snapshots(previous, next))
            };
            if unchanged(&last.live, &snapshot) && unchanged(&last.api, &api) {
                return Ok(());
            }
            let first_publication = last.live.is_none();
            last.api = Some(Arc::clone(&api));
            last.live = Some(Arc::clone(&snapshot));
            (api, first_publication)
        };
        // No publication lock is held across the sink callback: it may re-enter.
        // LegacySink::send is non-blocking and coalesces at the root boundary.
        self.sink.send(B1Event::Publish {
            key: self.key,
            api,
            live: snapshot,
        });
        if first_publication {
            self.sink.send(B1Event::Phase {
                key: self.key,
                phase: B1Phase::Reading,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
