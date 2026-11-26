//! Two distinct real `.osu` charts under a songs root, selected through the
//! production osu! cold-resource publication (ROOT_DECLARE + resource refs), so
//! the streamCompanion adapter reads chart bytes only through the surface.
use super::{Rig, input};
use bytes::Bytes;
use std::{path::PathBuf, sync::Arc};
use tokio::sync::watch;
use vosu_core::server::bus_provider::BusStateProvider;
use vosu_core::server::routes::StateRoute;
use vosu_osu_state::StateBundleSnapshot;

const HEADER: &str = "osu file format v14\n\n[General]\nMode:0\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,1,80,1,0\n\n[HitObjects]\n";
/// Chart 0: three circles, a slider and a spinner. Chart 1 adds two circles.
const OBJECTS: [&str; 2] = [
    "64,192,1000,1,0,0:0:0:0:\n128,192,1400,1,0,0:0:0:0:\n256,192,1800,2,0,B|384:192,1,140\n384,192,2400,1,0,0:0:0:0:\n448,192,3000,8,0,3500\n",
    "64,192,1000,1,0,0:0:0:0:\n128,192,1400,1,0,0:0:0:0:\n256,192,1800,2,0,B|384:192,1,140\n384,192,2400,1,0,0:0:0:0:\n448,192,3000,8,0,3500\n96,192,4000,1,0,0:0:0:0:\n160,192,4400,1,0,0:0:0:0:\n",
];
const MARKERS: [&[u8]; 2] = [b"\"circles\":3", b"\"circles\":5"];
const FILES: [&str; 2] = ["chart-a.osu", "chart-b.osu"];

pub struct Charts {
    songs: PathBuf,
}
impl Charts {
    #[expect(
        clippy::disallowed_methods,
        reason = "Disposable immutable benchmark chart fixtures, never user configuration or state"
    )]
    pub fn new(rig: &Rig) -> Self {
        let songs = rig.fixture.h.root.path().join("Songs");
        std::fs::create_dir_all(songs.join("set")).unwrap();
        for (file, objects) in FILES.iter().zip(OBJECTS) {
            std::fs::write(songs.join("set").join(file), format!("{HEADER}{objects}")).unwrap();
        }
        Self { songs }
    }
    /// The common D17 input with `chart` selected in the menu facts.
    pub fn snapshot(&self, score: f64, chart: usize) -> Arc<StateBundleSnapshot> {
        let mut snapshot = (*input::snapshot(score, input::K1_COUNTS[0])).clone();
        let regular = Arc::make_mut(&mut snapshot.regular);
        regular.global.songs_folder = self.songs.to_string_lossy().into_owned();
        regular.menu.folder = Some("set".into());
        regular.menu.filename = Some(FILES[chart].into());
        regular.menu.checksum = Some(format!("d17-chart-{chart}"));
        regular.menu.resource_generation = chart as u64 + 1;
        Arc::new(snapshot)
    }
    pub fn marker(&self, chart: usize) -> &'static [u8] {
        MARKERS[chart]
    }
    /// Event-driven barrier: re-reads the production HTTP payload after each
    /// provider notification (bus change or derived chart completion) until it
    /// carries `chart`'s derived facts. Requires a live demand lease.
    pub fn await_bytes(
        &self,
        rig: &Rig,
        provider: &BusStateProvider,
        events: &mut watch::Receiver<u64>,
        chart: usize,
    ) -> Bytes {
        let deadline = std::time::Instant::now() + super::host::WAIT;
        loop {
            events.borrow_and_update();
            let body = rig.http(provider, StateRoute::StreamCompanion);
            if memchr::memmem::find(&body, MARKERS[chart]).is_some() {
                break body;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            rig.runtime()
                .block_on(async { tokio::time::timeout(remaining, events.changed()).await })
                .expect("derived chart deadline")
                .expect("provider alive");
        }
    }
}
