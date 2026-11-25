use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use serde::Deserialize;
#[path = "../adapter_fixture/mod.rs"]
pub mod adapter_fixture;
use adapter_fixture::Fixture;
#[path = "oracle_input.rs"]
mod input;
pub use input::*;
use vosu_osu_calc::beatmap::BeatmapEngine;

#[derive(Clone, Copy)]
pub struct Revisions {
    pub upstream_source: &'static str,
    pub lazer_calculator: &'static str,
    pub osu: &'static str,
    pub calculator_package: &'static str,
    pub calculator_tree: &'static str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionDocument {
    // The committed source-oracle inventory uses the upstream tosu revision key as parity evidence.
    #[serde(rename = "tosu")]
    pub upstream_source: String,
    pub lazer_calculator: String,
    pub osu: String,
    pub calculator_package: String,
    pub calculator_tree: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub format: u64,
    pub revisions: RevisionDocument,
    pub required_coverage: Vec<String>,
    pub cases: Vec<Case>,
}

#[derive(Deserialize)]
pub struct Case {
    pub id: String,
    pub tags: Vec<String>,
    pub input: String,
    pub expected: BTreeMap<String, String>,
}

pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle")
}

pub struct Replay {
    pub fixture: Fixture,
    pub instances: Vec<SourceInstance>,
}
pub fn replay(input: &Input) -> Replay {
    replay_with_engine(input, fixture_engine(input))
}
/// The adapter-side calculation seam a replay of `input` installs: the
/// named `ZeroEngine` for the two SC cases, the real engine otherwise.
pub fn fixture_engine(input: &Input) -> Option<Arc<dyn BeatmapEngine + Send + Sync>> {
    matches!(input.id.as_str(), "sc-equal-bpm" | "sc-range-bpm")
        .then(|| Arc::new(ZeroEngine) as Arc<dyn BeatmapEngine + Send + Sync>)
}
pub fn replay_with_engine(
    input: &Input,
    fixture_engine: Option<Arc<dyn BeatmapEngine + Send + Sync>>,
) -> Replay {
    let client = client(input);
    let fixture = Fixture::with_chart(client, chart_content(input).as_deref(), fixture_engine);
    let instances = instances(input);
    let mut fixture = fixture;
    for instance in &instances {
        assert_eq!(instance.state.source, client);
        let publisher = fixture.attach(
            instance.settings.clone(),
            instance.manager,
            instance.pid == 10,
        );
        publisher
            .publish(instance.state.clone(), vosu_osu_state::Delivery::All)
            .unwrap();
    }
    Replay { fixture, instances }
}

pub fn encode(schema: &str, replay: &Replay) -> Vec<u8> {
    replay.fixture.encode(schema)
}
