//! The capture side of a round trip. It stands in for a live vosu daemon:
//! the production osu! plugin projection publishes a seeded corpus play
//! through the ABI into a real host, and each real adapter provider's
//! `capture_for_harness` dumps the envelope that the harness endpoint
//! serves. Only those envelope bytes and the chart file the source served
//! reach the replay. The corpus input never does.
use super::{assets::ReplayAssets, envelope::Envelope, oracle, seeds};
use vosu::server::adapter_capture::{SCHEMAS, schema_name};
use vosu_core::server::routes::CachedJson;
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_state::Delivery;

/// The slot and name `adapter_fixture`'s chart is declared and resolved as.
const CHART_SLOT: &str = "songs";
const CHART_NAME: &str = "oracle-chart.osu";

pub struct Source {
    /// Raw envelope bytes in `SCHEMAS` order.
    pub envelopes: Vec<Vec<u8>>,
    /// The chart file the source served, as a replay asset.
    pub assets: ReplayAssets,
}
impl Source {
    pub fn parsed(&self) -> Vec<Envelope> {
        self.envelopes
            .iter()
            .map(|bytes| Envelope::parse(bytes).expect("source envelope"))
            .collect()
    }
}

/// `sessions` stable clients attach in order; the last one is selected and
/// owns the foreground. Zero attaches no client.
pub fn capture(sessions: usize) -> Source {
    let original: oracle::Input = serde_json::from_slice(
        &std::fs::read(oracle::fixture_root().join("stable-play-osu-fl/input.json")).unwrap(),
    )
    .unwrap();
    let input = seeds::seeded(&original);
    let chart = oracle::chart_content(&input).expect("corpus play has a chart");
    let mut fixture =
        oracle::adapter_fixture::Fixture::with_chart(ClientType::Stable, Some(&chart), None);
    let settings = || InstanceSettings {
        tournament_spectator: false,
        tournament_ipc_id: None,
        devserver_endpoint: None,
    };
    let mut selected = None;
    for index in 0..sessions {
        let last = index + 1 == sessions;
        let publisher = fixture.attach(settings(), false, last);
        if last {
            publisher
                .publish(oracle::state(&input), Delivery::All)
                .unwrap();
        }
        selected = Some(publisher);
    }
    if selected.is_some() {
        // Settle the chart-derived view, as a capture's stable window does.
        assert!(matches!(fixture.outcome("sc"), CachedJson::Ready(_)));
    }
    let envelopes = {
        let _runtime = fixture.h.runtime.enter();
        SCHEMAS
            .iter()
            .enumerate()
            .map(|(index, (name, schema))| {
                assert_eq!(*name, schema_name(*schema));
                let sample = fixture.providers[index].capture_for_harness().unwrap();
                assert_eq!(sample.schema, *schema);
                sample.envelope().unwrap()
            })
            .collect()
    };
    drop(selected);
    drop(fixture);
    Source {
        envelopes,
        assets: ReplayAssets::chart(CHART_SLOT, CHART_NAME, chart.as_bytes()),
    }
}
