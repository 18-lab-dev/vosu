use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value, json};
use vosu_pp::{
    BeatmapDifficultyData, DifficultyConfig, LiveLazerEngine, OSU_FORWARD_REVISION, ROSU_BASELINE,
    ROSU_SOURCE_REVISION, Ruleset, ScoreInfoData, ScoreMultiplierScoreContext, StructuredMod,
    great_hit_window, score_multiplier, source_map::SOURCE_PORTS,
};

const ABS_TOLERANCE: f64 = 0.000_01;
const MAP: &[u8] = include_bytes!("fixture.osu");
const OSU_DIFFCALC: &[u8] =
    include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/osu-diffcalc-test.osu");
const TAIKO_DIFFCALC: &[u8] =
    include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/taiko-diffcalc-test.osu");
const CATCH_DIFFCALC: &[u8] =
    include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/catch-diffcalc-test.osu");
const MANIA_DIFFCALC: &[u8] =
    include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/mania-diffcalc-test.osu");
const MANIFEST: &str = include_str!("../../../tools/oracle/calculator-diff-manifest.json");
const CALCULATOR_REVISION: &str = "9a22c4c74f8e584fa64133a79a204dd045646bac";
const FIXTURES: &str = include_str!("../../../tests/fixtures/calculator/csharp-oracle.json");
const CSHARP_EVIDENCE: &str =
    include_str!("../../../tests/fixtures/calculator/csharp-test-evidence.json");

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pins {
    source_revision: String,
    rosu_baseline: String,
    osu_revision: String,
    calculator_revision: String,
    calculator_package: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestItem {
    id: String,
    source_port: String,
    authority: String,
    #[serde(default)]
    requires: Vec<String>,
    location: String,
    #[serde(rename = "baselineLocation")]
    baseline_location: String,
    #[serde(rename = "rustLocation")]
    rust_location: String,
    delta: String,
    vectors: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema: u32,
    pins: Pins,
    items: Vec<ManifestItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CsharpTestEvidence {
    schema: u32,
    osu_revision: String,
    dotnet_version: String,
    tests: Vec<CsharpTestRun>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CsharpTestRun {
    project: String,
    filter: String,
    stdout_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OraclePins {
    calculator_revision: String,
    csharp_test_evidence: CsharpTestEvidence,
    osu_revision: String,
    rosu_baseline: String,
    package_version: String,
}

#[derive(Debug, Deserialize)]
struct FixtureFile {
    schema: u32,
    oracle: OraclePins,
    map: String,
    vectors: Vec<FixtureVector>,
    #[serde(default)]
    contracts: Vec<ContractFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureVector {
    id: String,
    mode: String,
    #[serde(default = "default_fixture_map")]
    map: String,
    actions: Vec<String>,
    observed_capabilities: Vec<String>,
    mods: Vec<StructuredMod>,
    target_accuracy: f64,
    expected: Value,
}

#[derive(Debug, Deserialize)]
struct ContractFixture {
    id: String,
    actions: Vec<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(rename = "targetAccuracy")]
    target_accuracy: Option<f64>,
    #[serde(default)]
    classic: bool,
    #[serde(rename = "observedCapabilities")]
    observed_capabilities: Vec<String>,
    expected: Value,
}

fn default_fixture_map() -> String {
    "fixture.osu".into()
}

fn vector_map(name: &str) -> &'static [u8] {
    match name {
        "fixture.osu" => MAP,
        "osu-diffcalc-test.osu" => OSU_DIFFCALC,
        "taiko-diffcalc-test.osu" => TAIKO_DIFFCALC,
        "catch-diffcalc-test.osu" => CATCH_DIFFCALC,
        "mania-diffcalc-test.osu" => MANIA_DIFFCALC,
        "osu-branches/snap.osu" => {
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/osu-branches/snap.osu")
        }
        "osu-branches/flow.osu" => {
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/osu-branches/flow.osu")
        }
        "osu-branches/agility.osu" => {
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/osu-branches/agility.osu")
        }
        "osu-branches/speed-rhythm.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/osu-branches/speed-rhythm.osu"
        ),
        "osu-branches/reading-fl.osu" => {
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/osu-branches/reading-fl.osu")
        }
        "osu-branches/sliders.osu" => {
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/osu-branches/sliders.osu")
        }
        "osu-branches/sections.osu" => {
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/osu-branches/sections.osu")
        }
        "mania-branches/individual-jack.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/mania-branches/individual-jack.osu"
        ),
        "mania-branches/individual-alternate.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/mania-branches/individual-alternate.osu"
        ),
        "mania-branches/overall-chord.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/mania-branches/overall-chord.osu"
        ),
        "mania-branches/overall-separated.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/mania-branches/overall-separated.osu"
        ),
        "mania-branches/hold-nested.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/mania-branches/hold-nested.osu"
        ),
        "mania-branches/hold-control.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/mania-branches/hold-control.osu"
        ),
        "taiko-branches/group-two-left.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/group-two-left.osu"
        ),
        "taiko-branches/group-two-right.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/group-two-right.osu"
        ),
        "taiko-branches/long-gap-left.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/long-gap-left.osu"
        ),
        "taiko-branches/long-gap-right.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/long-gap-right.osu"
        ),
        "taiko-branches/reading-band-left.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/reading-band-left.osu"
        ),
        "taiko-branches/reading-band-right.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/reading-band-right.osu"
        ),
        "taiko-branches/island-left.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/island-left.osu"
        ),
        "taiko-branches/island-right.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/island-right.osu"
        ),
        "taiko-branches/mono-left.osu" => {
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/mono-left.osu")
        }
        "taiko-branches/mono-right.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/mono-right.osu"
        ),
        "taiko-branches/nonhit-left.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/nonhit-left.osu"
        ),
        "taiko-branches/nonhit-right.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/taiko-branches/nonhit-right.osu"
        ),
        "catch-branches/movement-left.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/catch-branches/movement-left.osu"
        ),
        "catch-branches/movement-right.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/catch-branches/movement-right.osu"
        ),
        "catch-branches/direction-left.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/catch-branches/direction-left.osu"
        ),
        "catch-branches/direction-right.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/catch-branches/direction-right.osu"
        ),
        "catch-branches/hyperdash-left.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/catch-branches/hyperdash-left.osu"
        ),
        "catch-branches/hyperdash-right.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/catch-branches/hyperdash-right.osu"
        ),
        "catch-branches/cs-left.osu" => {
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/catch-branches/cs-left.osu")
        }
        "catch-branches/cs-right.osu" => {
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/catch-branches/cs-right.osu")
        }
        "catch-branches/sections-left.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/catch-branches/sections-left.osu"
        ),
        "catch-branches/sections-right.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/catch-branches/sections-right.osu"
        ),
        "mania-branches/section-peaks-4k.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/mania-branches/section-peaks-4k.osu"
        ),
        "mania-branches/column-count-4k.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/mania-branches/column-count-4k.osu"
        ),
        "mania-branches/column-count-7k.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/mania-branches/column-count-7k.osu"
        ),
        "mania-branches/performance-4k.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/mania-branches/performance-4k.osu"
        ),
        "osu-branches/aggregate-skills.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/osu-branches/aggregate-skills.osu"
        ),
        "osu-branches/slider-miss-performance.osu" => include_bytes!(
            "../../../tools/oracle/inputs/osu-97e94bab/osu-branches/slider-miss-performance.osu"
        ),
        other => panic!("unknown committed calculator map: {other}"),
    }
}

fn ruleset(name: &str) -> Ruleset {
    match name {
        "osu" => Ruleset::Osu,
        "taiko" => Ruleset::Taiko,
        "catch" => Ruleset::Catch,
        "mania" => Ruleset::Mania,
        other => panic!("unknown fixture ruleset: {other}"),
    }
}

fn compare_oracle(actual: &Value, expected: &Value, path: &str, mismatches: &mut Vec<String>) {
    match (actual, expected) {
        (Value::Number(actual), Value::Number(expected))
            if actual.is_f64() || expected.is_f64() =>
        {
            let actual = actual
                .as_f64()
                .expect("actual number must be f64-compatible");
            let expected = expected
                .as_f64()
                .expect("expected number must be f64-compatible");
            let difference = (actual - expected).abs();
            if !actual.is_finite() || !expected.is_finite() || difference > ABS_TOLERANCE {
                mismatches.push(format!(
                    "{path}: actual={actual:.15}, expected={expected:.15}, abs_diff={difference:.15}"
                ));
            }
        }
        (Value::Array(actual), Value::Array(expected)) => {
            if actual.len() != expected.len() {
                mismatches.push(format!(
                    "{path}: array length actual={}, expected={}",
                    actual.len(),
                    expected.len()
                ));
            }
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                compare_oracle(actual, expected, &format!("{path}/{index}"), mismatches);
            }
        }
        (Value::Object(actual), Value::Object(expected)) => {
            let actual_keys = actual.keys().collect::<BTreeSet<_>>();
            let expected_keys = expected.keys().collect::<BTreeSet<_>>();
            if actual_keys != expected_keys {
                mismatches.push(format!(
                    "{path}: JSON fields actual={actual_keys:?}, expected={expected_keys:?}"
                ));
            }
            for (key, expected) in expected {
                if let Some(actual) = actual.get(key) {
                    compare_oracle(actual, expected, &format!("{path}/{key}"), mismatches);
                }
            }
        }
        _ if actual != expected => {
            mismatches.push(format!("{path}: actual={actual}, expected={expected}"));
        }
        _ => {}
    }
}

fn gradual_point(
    gradual: &mut vosu_pp::GradualDifficulty,
    target_accuracy: f64,
    include_strains: bool,
) -> Value {
    let difficulty = gradual.create_difficulty_attrs().unwrap();
    let score = gradual
        .create_progressive_score(target_accuracy)
        .expect("progressive C# score vector must be representable");
    let mut point = Map::new();
    point.insert("processed".into(), json!(gradual.processed_objects()));
    point.insert("difficulty".into(), json!(difficulty));
    if include_strains {
        point.insert(
            "strains".into(),
            json!(gradual.get_current_strains().unwrap()),
        );
    }
    point.insert(
        "performance".into(),
        json!(gradual.performance_from_score(score).unwrap()),
    );
    point.insert("score".into(), json!(score));
    point.insert(
        "accuracy".into(),
        json!(gradual.calculate_progressive_accuracy(score).unwrap()),
    );
    Value::Object(point)
}

fn calculate_vector(vector: &FixtureVector) -> Value {
    let engine = LiveLazerEngine;
    let parsed = engine.parse(vector_map(&vector.map)).unwrap();
    let mut map = engine.convert(&parsed, ruleset(&vector.mode)).unwrap();
    engine.apply_mods(&mut map, &vector.mods).unwrap();
    let config = DifficultyConfig::default();
    let difficulty = engine.difficulty_data(&map, &config).unwrap();
    let include_strains = vector.expected.get("strains").is_some();
    let strains = include_strains.then(|| engine.strains_data(&map, &config).unwrap());
    let score = engine.create_score(&map, vector.target_accuracy).unwrap();
    let performance = engine.performance_data(&map, &config, score).unwrap();
    let accuracy = engine.calculate_accuracy(&map, score).unwrap();

    let mut prefix_gradual = engine.gradual_difficulty(&map, &config).unwrap();
    assert!(prefix_gradual.advance());
    let prefix = gradual_point(&mut prefix_gradual, vector.target_accuracy, false);
    let mut full_gradual = engine.gradual_difficulty(&map, &config).unwrap();
    while full_gradual.advance() {}
    let full = gradual_point(&mut full_gradual, vector.target_accuracy, include_strains);
    let mut replay_gradual = engine.gradual_difficulty(&map, &config).unwrap();
    while replay_gradual.advance() {}
    let replay = gradual_point(&mut replay_gradual, vector.target_accuracy, include_strains);

    let mut expected = Map::new();
    expected.insert(
        "mode".into(),
        json!(match map.mode() {
            Ruleset::Osu => "osu",
            Ruleset::Taiko => "taiko",
            Ruleset::Catch => "catch",
            Ruleset::Mania => "mania",
        }),
    );
    expected.insert("objectCount".into(), json!(map.object_count()));
    expected.insert("difficulty".into(), json!(difficulty));
    if let Some(strains) = strains {
        expected.insert("strains".into(), json!(strains));
    }
    expected.insert("performance".into(), json!(performance));
    expected.insert("score".into(), json!(score));
    expected.insert("accuracy".into(), json!(accuracy));
    expected.insert(
        "gradual".into(),
        json!({
            "prefix1": prefix,
            "full": full,
            "replay": replay,
        }),
    );
    Value::Object(expected)
}

fn replay_gradual_contract(mode: Ruleset, expected: &Value) -> Value {
    let engine = LiveLazerEngine;
    let parsed = engine.parse(MAP).unwrap();
    let map = engine.convert(&parsed, mode).unwrap();
    let config = DifficultyConfig::default();

    let mut advance_one = engine.gradual_difficulty(&map, &config).unwrap();
    let advance_one_result = advance_one.advance();

    let mut skip_one = engine.gradual_difficulty(&map, &config).unwrap();
    skip_one.skip(1);
    let skip_one_then_advance = skip_one.advance();

    let mut before = engine.gradual_difficulty(&map, &config).unwrap();
    let before_count = before.skip_to_time(999.0);
    let mut start = engine.gradual_difficulty(&map, &config).unwrap();
    let start_count = start.skip_to_time(1_000.0);
    let mut end = engine.gradual_difficulty(&map, &config).unwrap();
    let end_count = end.skip_to_time(3_500.0);
    let mut nan = engine.gradual_difficulty(&map, &config).unwrap();
    let nan_count = nan.skip_to_time(f64::NAN);

    let mut full = engine.gradual_difficulty(&map, &config).unwrap();
    full.skip_to_end();
    let difficulty = full.create_difficulty_attrs().unwrap();
    let strains = full.get_current_strains().unwrap();

    let mut replay = engine.gradual_difficulty(&map, &config).unwrap();
    replay.skip_to_end();

    let mut actual = json!({
        "advanceOne": advance_one_result,
        "skipOneThenAdvance": skip_one_then_advance,
        "before": before_count,
        "start": start_count,
        "end": end_count,
        "nan": nan_count,
        "skipEnd": difficulty,
        "skipEndStrains": strains,
        "replay": replay.create_difficulty_attrs().unwrap(),
    });

    // Catch exposes an additional boundary exactly at the first converted object.
    if expected.get("first").is_some() {
        let mut first = engine.gradual_difficulty(&map, &config).unwrap();
        actual["first"] = json!(first.skip_to_time(0.0));
    }

    actual
}

#[test]
fn two_source_manifest_is_pinned_and_every_item_has_an_oracle_vector() {
    let manifest: Manifest = serde_json::from_str(MANIFEST).unwrap();
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    assert_eq!(manifest.schema, 1);
    assert_eq!(fixtures.schema, 1);
    assert_eq!(fixtures.map, "fixture.osu");
    assert_eq!(
        manifest.pins.source_revision,
        "1a67fd64cc38fe60306328dde0a429e3211ca4ed"
    );
    assert_eq!(manifest.pins.rosu_baseline, ROSU_BASELINE);
    assert_eq!(
        ROSU_SOURCE_REVISION,
        "27a67242cad155a2767f5a25a423ca4ec3e3c274"
    );
    assert_eq!(manifest.pins.osu_revision, OSU_FORWARD_REVISION);
    assert_eq!(manifest.pins.calculator_revision, CALCULATOR_REVISION);
    assert_eq!(manifest.pins.calculator_package, "0.5.0-20260702-main.0");
    assert_eq!(fixtures.oracle.rosu_baseline, ROSU_BASELINE);
    assert_eq!(fixtures.oracle.osu_revision, OSU_FORWARD_REVISION);
    assert_eq!(fixtures.oracle.calculator_revision, CALCULATOR_REVISION);
    assert_eq!(
        fixtures.oracle.package_version,
        manifest.pins.calculator_package
    );

    let fixture_value: Value = serde_json::from_str(FIXTURES).unwrap();
    let external_evidence: Value = serde_json::from_str(CSHARP_EVIDENCE).unwrap();
    assert_eq!(
        fixture_value["oracle"]["csharpTestEvidence"], external_evidence,
        "embedded C# evidence must exactly equal the separately executed artifact"
    );
    let mut scenarios = BTreeMap::<&str, BTreeSet<&str>>::new();
    let evidence = &fixtures.oracle.csharp_test_evidence;
    assert_eq!(evidence.schema, 1);
    assert_eq!(evidence.osu_revision, OSU_FORWARD_REVISION);
    assert!(evidence.dotnet_version.starts_with("8."));
    assert_eq!(evidence.tests.len(), 4);
    for run in &evidence.tests {
        assert!(run.project.ends_with(".Tests.csproj"));
        assert!(run.filter.contains("ScoreMultiplierTest"));
        if !run.project.contains(".Osu.") {
            assert!(run.filter.contains("BeatmapConversionTest"));
        }
        assert_eq!(run.stdout_sha256.len(), 64);
        assert!(
            run.stdout_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
    }
    for vector in &fixtures.vectors {
        assert!(
            !vector.actions.is_empty(),
            "scenario has no oracle actions: {}",
            vector.id
        );
        assert!(
            scenarios
                .insert(
                    vector.id.as_str(),
                    vector
                        .observed_capabilities
                        .iter()
                        .map(String::as_str)
                        .collect(),
                )
                .is_none(),
            "duplicate scenario id: {}",
            vector.id
        );
    }
    for contract in &fixtures.contracts {
        assert!(
            !contract.actions.is_empty(),
            "scenario has no oracle actions: {}",
            contract.id
        );
        assert!(
            scenarios
                .insert(
                    contract.id.as_str(),
                    contract
                        .observed_capabilities
                        .iter()
                        .map(String::as_str)
                        .collect(),
                )
                .is_none(),
            "duplicate scenario id: {}",
            contract.id
        );
    }
    let source_ports = SOURCE_PORTS
        .iter()
        .map(|port| port.scope)
        .collect::<BTreeSet<_>>();
    let mut covered_ports = BTreeSet::new();
    let mut item_ids = BTreeSet::new();
    let mut authority_counts = BTreeMap::<&str, usize>::new();
    for item in &manifest.items {
        assert!(
            item_ids.insert(item.id.as_str()),
            "duplicate manifest item: {}",
            item.id
        );
        assert!(
            source_ports.contains(item.source_port.as_str()),
            "unknown SOURCE_PORTS scope: {}",
            item.source_port
        );
        assert!(matches!(
            item.authority.as_str(),
            "osu-forward" | "calculator-wrapper"
        ));
        *authority_counts.entry(item.authority.as_str()).or_default() += 1;
        assert!(
            item.location.contains('/') || item.location.contains("repository diff"),
            "manifest item lacks a concrete pinned source location: {}",
            item.id
        );
        assert!(!item.baseline_location.trim().is_empty());
        assert!(
            item.rust_location.contains(".rs") || item.rust_location.contains("src/"),
            "manifest item lacks a concrete Rust target: {}",
            item.id
        );
        assert!(
            item.delta.len() >= 70 && !item.delta.starts_with("Port target behavior"),
            "manifest item lacks a concrete baseline-to-target delta: {}",
            item.id
        );
        assert!(
            !item.requires.is_empty(),
            "manifest item has no behavioral requirements: {}",
            item.id
        );
        assert!(
            !item.vectors.is_empty(),
            "uncovered manifest item: {}",
            item.id
        );
        let requirements = item
            .requires
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert!(
            item.vectors.iter().any(|scenario| scenarios
                .get(scenario.as_str())
                .is_some_and(|capabilities| requirements.is_subset(capabilities))),
            "manifest item {} has no replay-validated proof for {:?}",
            item.id,
            item.requires
        );
        covered_ports.insert(item.source_port.as_str());
    }
    let unsupported = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "unsupported-conversion-null")
        .expect("missing unsupported conversion C# contract");
    assert_eq!(
        unsupported.expected,
        json!({"throws": true, "isNull": false})
    );
    assert!(Ruleset::from_mode_id(99).is_none());

    let prefix_zero = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "gradual-prefix-zero-attrs")
        .expect("missing prefix-zero gradual C# contract");
    assert_eq!(prefix_zero.expected, json!({"throws": false}));
    let engine = LiveLazerEngine;
    let map = engine.parse(MAP).unwrap();
    let mut gradual = engine
        .gradual_difficulty(&map, &DifficultyConfig::default())
        .unwrap();
    assert!(gradual.create_difficulty_attrs().is_ok());
    let hit_windows = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "hit-window-boundaries")
        .expect("missing hit-window C# contract");
    let actual_hit_windows = json!({
        "osuGreatOd5": great_hit_window(Ruleset::Osu, 5.0),
        "taikoGreatOd10": great_hit_window(Ruleset::Taiko, 10.0),
        "catchGreatOd5": great_hit_window(Ruleset::Catch, 5.0),
        "maniaGreatOd0": great_hit_window(Ruleset::Mania, 0.0),
    });
    let mut hit_window_mismatches = Vec::new();
    compare_oracle(
        &actual_hit_windows,
        &hit_windows.expected,
        "/hit-window-boundaries",
        &mut hit_window_mismatches,
    );
    assert!(
        hit_window_mismatches.is_empty(),
        "{}",
        hit_window_mismatches.join("\n")
    );
    for (mode_name, mode) in [
        ("osu", Ruleset::Osu),
        ("taiko", Ruleset::Taiko),
        ("catch", Ruleset::Catch),
        ("mania", Ruleset::Mania),
    ] {
        let contract = fixtures
            .contracts
            .iter()
            .find(|contract| contract.id == format!("score-multiplier-{mode_name}"))
            .expect("missing pinned osu! multiplier contract");
        let mods: Vec<StructuredMod> =
            serde_json::from_value(contract.expected["mods"].clone()).unwrap();
        let actual = score_multiplier(mode, BeatmapDifficultyData::default(), &mods, None).unwrap();
        let expected = contract.expected["multiplier"].as_f64().unwrap();
        assert!(
            (actual - expected).abs() <= ABS_TOLERANCE,
            "{mode_name} score multiplier: actual={actual}, expected={expected}"
        );
    }
    assert_eq!(
        covered_ports, source_ports,
        "manifest must cover every canonical SOURCE_PORTS item"
    );
    assert!(authority_counts["osu-forward"] > 0);
    assert!(authority_counts["calculator-wrapper"] > 0);
}

#[test]
fn committed_csharp_vectors_match_full_and_gradual_rust_outputs() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let modes = fixtures
        .vectors
        .iter()
        .map(|vector| vector.mode.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(modes, BTreeSet::from(["osu", "taiko", "catch", "mania"]));
    let mut mismatches = Vec::new();
    for vector in &fixtures.vectors {
        compare_oracle(
            &calculate_vector(vector),
            &vector.expected,
            &format!("/{}", vector.id),
            &mut mismatches,
        );
    }
    assert!(
        mismatches.is_empty(),
        "{} C# oracle mismatch(es):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn committed_csharp_wrapper_actions_replay_stateful_mutation_and_retained_attrs() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let contract = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "wrapper-action-contract")
        .expect("missing wrapper action contract");
    let engine = LiveLazerEngine;
    let parsed = engine.parse(MAP).unwrap();
    let mut map = engine.convert(&parsed, Ruleset::Osu).unwrap();
    let config = DifficultyConfig::default();
    let original = engine.original_beatmap_difficulty(&map);
    let before = engine.beatmap_difficulty(&map);
    let base_attrs = engine.difficulty_data(&map, &config).unwrap();
    let base_score = engine.create_score(&map, 0.95).unwrap();
    let base_performance = engine
        .performance_data_from_attrs(&map, &config, &base_attrs, base_score)
        .unwrap();

    let mut settings = BTreeMap::new();
    settings.insert("overall_difficulty".into(), vosu_pp::ModValue::Number(7.4));
    settings.insert(
        "unknown_setting".into(),
        vosu_pp::ModValue::String("ignored".into()),
    );
    engine
        .apply_mods(
            &mut map,
            &[StructuredMod {
                acronym: "DA".into(),
                settings,
            }],
        )
        .unwrap();
    let adjusted = engine.beatmap_difficulty(&map);
    let adjusted_attrs = engine.difficulty_data(&map, &config).unwrap();
    let adjusted_score = engine.create_score(&map, 0.95).unwrap();
    let adjusted_performance = engine
        .performance_data_from_attrs(&map, &config, &adjusted_attrs, adjusted_score)
        .unwrap();
    let retained_performance = engine
        .performance_data_from_attrs(&map, &config, &base_attrs, base_score)
        .unwrap();

    engine
        .apply_mods(
            &mut map,
            &[StructuredMod {
                acronym: "HD".into(),
                settings: BTreeMap::new(),
            }],
        )
        .unwrap();
    let replacement = engine.beatmap_difficulty(&map);
    let converted = engine.convert(&map, Ruleset::Taiko).unwrap();
    let actual = json!({
        "original": original,
        "before": before,
        "adjusted": adjusted,
        "replacement": replacement,
        "convertedDifficulty": engine.beatmap_difficulty(&converted),
        "basePerformance": base_performance,
        "adjustedPerformance": adjusted_performance,
        "retainedPerformance": retained_performance,
        "baseAttrs": base_attrs,
        "adjustedAttrs": adjusted_attrs,
    });
    let mut mismatches = Vec::new();
    compare_oracle(
        &actual,
        &contract.expected,
        "/wrapper-action-contract",
        &mut mismatches,
    );
    assert!(
        mismatches.is_empty(),
        "{} wrapper action C# oracle mismatch(es):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn committed_csharp_display_retained_attrs_and_cross_mode_errors_replay_exactly() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let contract = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "wrapper-display-retained-attrs-contract")
        .expect("missing wrapper display/retained attrs contract");
    let engine = LiveLazerEngine;
    let parsed = engine.parse(MAP).unwrap();
    let config = DifficultyConfig::default();
    let game_mod = |acronym: &str, settings: &[(&str, f64)]| StructuredMod {
        acronym: acronym.into(),
        settings: settings
            .iter()
            .map(|(key, value)| ((*key).into(), vosu_pp::ModValue::Number(*value)))
            .collect(),
    };
    let variants = |mode: Ruleset| {
        let mut values = vec![
            ("base", Vec::new()),
            ("hr", vec![game_mod("HR", &[])]),
            ("ez", vec![game_mod("EZ", &[])]),
            ("dt", vec![game_mod("DT", &[("speed_change", 1.25)])]),
            ("ht", vec![game_mod("HT", &[("speed_change", 0.75)])]),
            (
                "mixedFixedRates",
                vec![
                    game_mod("DT", &[("speed_change", 1.1)]),
                    game_mod("HT", &[("speed_change", 0.8)]),
                ],
            ),
            (
                "duplicateFixedRates",
                vec![
                    game_mod("DT", &[("speed_change", 1.1)]),
                    game_mod("DT", &[("speed_change", 1.4)]),
                ],
            ),
            (
                "windUp",
                vec![game_mod(
                    "WU",
                    &[("initial_rate", 0.8), ("final_rate", 1.4)],
                )],
            ),
            (
                "windDown",
                vec![game_mod(
                    "WD",
                    &[("initial_rate", 1.2), ("final_rate", 0.7)],
                )],
            ),
        ];
        if mode != Ruleset::Catch {
            values.push((
                "adaptiveSpeed",
                vec![game_mod("AS", &[("initial_rate", 1.35)])],
            ));
        }
        values.push((
            "da",
            vec![game_mod(
                "DA",
                &[
                    ("approach_rate", 8.3),
                    (
                        "circle_size",
                        if mode == Ruleset::Mania { 7.0 } else { 4.2 },
                    ),
                    ("drain_rate", 6.1),
                    ("overall_difficulty", 7.4),
                ],
            )],
        ));
        values
    };

    let mut actual_displays = Map::new();
    for (name, mode) in [
        ("osu", Ruleset::Osu),
        ("taiko", Ruleset::Taiko),
        ("catch", Ruleset::Catch),
        ("mania", Ruleset::Mania),
    ] {
        let mut mode_values = Map::new();
        for (variant, mods) in variants(mode) {
            let mut map = engine.convert(&parsed, mode).unwrap();
            engine.apply_mods(&mut map, &mods).unwrap();
            mode_values.insert(variant.into(), json!(engine.beatmap_difficulty(&map)));
        }
        actual_displays.insert(name.into(), Value::Object(mode_values));
    }

    let mut actual_retained = Map::new();
    for (name, mode) in [
        ("osu", Ruleset::Osu),
        ("taiko", Ruleset::Taiko),
        ("catch", Ruleset::Catch),
    ] {
        let mut values = Map::new();
        for (variant, current_mod) in [
            ("hr", game_mod("HR", &[])),
            ("da", game_mod("DA", &[("overall_difficulty", 7.4)])),
        ] {
            let mut map = engine.convert(&parsed, mode).unwrap();
            let attrs = engine.difficulty_data(&map, &config).unwrap();
            let score = engine.create_score(&map, 0.95).unwrap();
            engine.apply_mods(&mut map, &[current_mod]).unwrap();
            values.insert(
                variant.into(),
                json!(
                    engine
                        .performance_data_from_attrs(&map, &config, &attrs, score)
                        .unwrap()
                ),
            );
        }
        actual_retained.insert(name.into(), Value::Object(values));
    }

    let mut actual_cross_mode = Map::new();
    for (source_name, source_mode) in [
        ("osu", Ruleset::Osu),
        ("taiko", Ruleset::Taiko),
        ("catch", Ruleset::Catch),
        ("mania", Ruleset::Mania),
    ] {
        let source = engine.convert(&parsed, source_mode).unwrap();
        let attrs = engine.difficulty_data(&source, &config).unwrap();
        for (target_name, target_mode) in [
            ("osu", Ruleset::Osu),
            ("taiko", Ruleset::Taiko),
            ("catch", Ruleset::Catch),
            ("mania", Ruleset::Mania),
        ] {
            if source_mode == target_mode {
                continue;
            }
            let target = engine.convert(&parsed, target_mode).unwrap();
            let score = engine.create_score(&target, 0.95).unwrap();
            actual_cross_mode.insert(
                format!("{source_name}-attrs-on-{target_name}"),
                json!(
                    engine
                        .performance_data_from_attrs(&target, &config, &attrs, score)
                        .is_err()
                ),
            );
        }
    }
    let actual = json!({
        "displays": actual_displays,
        "retained": actual_retained,
        "crossMode": actual_cross_mode,
    });
    let mut mismatches = Vec::new();
    compare_oracle(
        &actual,
        &contract.expected,
        "/wrapper-display-retained-attrs-contract",
        &mut mismatches,
    );
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn committed_csharp_partial_osu_strains_contract_replays_in_rust() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let contract = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "gradual-osu-partial-strains")
        .expect("missing partial osu strains contract");
    let engine = LiveLazerEngine;
    let parsed = engine.parse(MAP).unwrap();
    let map = engine.convert(&parsed, Ruleset::Osu).unwrap();
    let config = DifficultyConfig::default();

    let fresh = engine.gradual_difficulty(&map, &config).unwrap();
    let fresh_throws = fresh.get_current_strains().is_err();
    let mut partial = engine.gradual_difficulty(&map, &config).unwrap();
    assert!(partial.advance());
    let partial_throws = partial.get_current_strains().is_err();
    let mut full = engine.gradual_difficulty(&map, &config).unwrap();
    full.skip_to_end();
    let actual = json!({
        "freshThrows": fresh_throws,
        "partialThrows": partial_throws,
        "full": full.get_current_strains().unwrap(),
    });
    let mut mismatches = Vec::new();
    compare_oracle(
        &actual,
        &contract.expected,
        "/gradual-osu-partial-strains",
        &mut mismatches,
    );
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn committed_csharp_seedless_random_mods_remain_stable_within_one_map_lifecycle() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let contract = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "random-seed-persistence")
        .expect("missing random seed persistence contract");
    let engine = LiveLazerEngine;
    let parsed = engine.parse(MAP).unwrap();
    let config = DifficultyConfig::default();
    let mut actual = Map::new();
    for (mode, name, acronym) in [
        (Ruleset::Osu, "osu-rd", "RD"),
        (Ruleset::Osu, "osu-tp", "TP"),
        (Ruleset::Taiko, "taiko-rd", "RD"),
        (Ruleset::Mania, "mania-rd", "RD"),
    ] {
        let mut map = engine.convert(&parsed, mode).unwrap();
        engine
            .apply_mods(
                &mut map,
                &[StructuredMod {
                    acronym: acronym.into(),
                    settings: BTreeMap::new(),
                }],
            )
            .unwrap();
        let first = engine.difficulty_data(&map, &config).unwrap();
        let second = engine.difficulty_data(&map, &config).unwrap();
        actual.insert(
            name.into(),
            json!({ "difficultyStable": json!(first) == json!(second) }),
        );
    }
    let mut mismatches = Vec::new();
    compare_oracle(
        &Value::Object(actual),
        &contract.expected,
        "/random-seed-persistence",
        &mut mismatches,
    );
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn committed_csharp_paired_ruleset_discriminators_replay_both_sides() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    for (mode, id, left, right) in [
        (
            "mania",
            "mania-individual-strain-pair",
            "mania-branches/individual-jack.osu",
            "mania-branches/individual-alternate.osu",
        ),
        (
            "mania",
            "mania-overall-strain-pair",
            "mania-branches/overall-chord.osu",
            "mania-branches/overall-separated.osu",
        ),
        (
            "mania",
            "mania-hold-factor-pair",
            "mania-branches/hold-nested.osu",
            "mania-branches/hold-control.osu",
        ),
        (
            "mania",
            "mania-section-peaks-pair",
            "mania-branches/section-peaks-4k.osu",
            "mania-branches/performance-4k.osu",
        ),
        (
            "mania",
            "mania-column-count-pair",
            "mania-branches/column-count-4k.osu",
            "mania-branches/column-count-7k.osu",
        ),
        (
            "taiko",
            "taiko-group-logistic-pair",
            "taiko-branches/group-two-left.osu",
            "taiko-branches/group-two-right.osu",
        ),
        (
            "taiko",
            "taiko-long-gap-pair",
            "taiko-branches/long-gap-left.osu",
            "taiko-branches/long-gap-right.osu",
        ),
        (
            "taiko",
            "taiko-reading-band-pair",
            "taiko-branches/reading-band-left.osu",
            "taiko-branches/reading-band-right.osu",
        ),
        (
            "taiko",
            "taiko-rhythm-island-pair",
            "taiko-branches/island-left.osu",
            "taiko-branches/island-right.osu",
        ),
        (
            "taiko",
            "taiko-mono-stamina-pair",
            "taiko-branches/mono-left.osu",
            "taiko-branches/mono-right.osu",
        ),
        (
            "taiko",
            "taiko-nonhit-pair",
            "taiko-branches/nonhit-left.osu",
            "taiko-branches/nonhit-right.osu",
        ),
        (
            "catch",
            "catch-movement-pair",
            "catch-branches/movement-left.osu",
            "catch-branches/movement-right.osu",
        ),
        (
            "catch",
            "catch-direction-pair",
            "catch-branches/direction-left.osu",
            "catch-branches/direction-right.osu",
        ),
        (
            "catch",
            "catch-hyperdash-pair",
            "catch-branches/hyperdash-left.osu",
            "catch-branches/hyperdash-right.osu",
        ),
        (
            "catch",
            "catch-cs-pair",
            "catch-branches/cs-left.osu",
            "catch-branches/cs-right.osu",
        ),
        (
            "catch",
            "catch-sections-pair",
            "catch-branches/sections-left.osu",
            "catch-branches/sections-right.osu",
        ),
    ] {
        let contract = fixtures
            .contracts
            .iter()
            .find(|contract| contract.id == id)
            .unwrap_or_else(|| panic!("missing {id} contract"));
        for (side, map_name) in [("left", left), ("right", right)] {
            let vector = FixtureVector {
                id: format!("{id}-{side}"),
                mode: mode.into(),
                map: map_name.into(),
                actions: Vec::new(),
                observed_capabilities: Vec::new(),
                mods: Vec::new(),
                target_accuracy: 0.95,
                expected: contract.expected[side].clone(),
            };
            let actual = calculate_vector(&vector);
            let mut mismatches = Vec::new();
            compare_oracle(
                &actual,
                &contract.expected[side],
                &format!("/{id}/{side}"),
                &mut mismatches,
            );
            assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
        }
    }
}

#[test]
fn committed_pinned_csharp_multiplier_matrix_replays_each_branch_row() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let contract = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "exhaustive-score-multiplier-matrix")
        .expect("missing exhaustive score multiplier matrix");
    assert_eq!(contract.expected["sourceRevision"], OSU_FORWARD_REVISION);
    let source_evidence = contract.expected["sourceEvidence"].as_array().unwrap();
    assert_eq!(source_evidence.len(), 4);
    for source in source_evidence {
        assert!(
            source["path"]
                .as_str()
                .unwrap()
                .starts_with("score-multiplier/")
        );
        let hash = source["sha256"].as_str().unwrap();
        assert_eq!(hash.len(), 64);
        assert!(hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    let pinned_cases = contract.expected["pinnedCases"].as_array().unwrap();
    assert_eq!(contract.expected["pinnedCaseCount"].as_u64().unwrap(), 481);
    assert_eq!(pinned_cases.len(), 481);
    let rows = contract.expected["rows"].as_array().unwrap();
    let case_ids = pinned_cases
        .iter()
        .map(|case| case["caseId"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let replay_ids = rows
        .iter()
        .filter_map(|row| row.get("caseId").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        case_ids.len(),
        pinned_cases.len(),
        "duplicate pinned NUnit case identity"
    );
    assert_eq!(
        replay_ids.len(),
        pinned_cases.len(),
        "missing or duplicate pinned Rust replay row"
    );
    assert_eq!(
        case_ids, replay_ids,
        "missing or extra pinned NUnit/Rust replay case"
    );

    let catch_cases = pinned_cases.iter().filter(|case| {
        case["method"]
            .as_str()
            .unwrap()
            .contains(".Catch.Tests.CatchScoreMultiplierTest.TestMultipliers")
    });
    let mut speeds = BTreeMap::<&str, Vec<f64>>::new();
    let mut catch_relax = 0;
    let mut catch_flashlight = 0;
    for case in catch_cases {
        let mods = case["parameters"]["mods"].as_array().unwrap();
        let acronym = mods[0]["acronym"].as_str().unwrap();
        match acronym {
            "HT" | "DC" | "DT" | "NC" => speeds
                .entry(acronym)
                .or_default()
                .push(mods[0]["settings"]["speedChange"].as_f64().unwrap()),
            "RX" => catch_relax += 1,
            "FL" => catch_flashlight += 1,
            _ => {}
        }
    }
    for values in speeds.values_mut() {
        values.sort_by(f64::total_cmp);
    }
    assert_eq!(
        speeds["HT"],
        [
            0.50, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80, 0.85, 0.90, 0.95, 0.99
        ]
    );
    assert_eq!(
        speeds["DC"],
        [
            0.50, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80, 0.85, 0.90, 0.95, 0.99
        ]
    );
    let fast_speeds = [
        1.01, 1.05, 1.10, 1.15, 1.20, 1.25, 1.30, 1.35, 1.40, 1.45, 1.50, 1.55, 1.60, 1.65, 1.70,
        1.75, 1.80, 1.85, 1.90, 1.95, 2.00,
    ];
    assert_eq!(speeds["DT"], fast_speeds);
    assert_eq!(speeds["NC"], fast_speeds);
    assert_eq!(catch_relax, 1);
    assert_eq!(catch_flashlight, 2);

    let mut replay_mismatches = Vec::new();
    for row in contract.expected["rows"].as_array().unwrap() {
        let id = row["id"].as_str().unwrap();
        let mode = ruleset(row["mode"].as_str().unwrap());
        let mods: Vec<StructuredMod> = serde_json::from_value(row["mods"].clone()).unwrap();
        let context = row
            .get("context")
            .filter(|value| !value.is_null())
            .map(|value| ScoreMultiplierScoreContext {
                total_score_version: value["totalScoreVersion"].as_i64().unwrap() as i32,
                client_version: value["clientVersion"].as_str().unwrap().into(),
                date_unix_ms: value["dateUnixMs"].as_i64().unwrap(),
            });
        let difficulty = row
            .get("difficulty")
            .map(|value| serde_json::from_value(value.clone()).unwrap())
            .unwrap_or(BeatmapDifficultyData {
                approach_rate: 9.0,
                circle_size: 4.0,
                overall_difficulty: 8.0,
                drain_rate: 6.0,
            });
        let actual = score_multiplier(mode, difficulty, &mods, context.as_ref()).unwrap();
        let expected = row["expected"].as_f64().unwrap();
        if (actual - expected).abs() > ABS_TOLERANCE {
            replay_mismatches.push(format!("{id}: actual={actual}, expected={expected}"));
        }
    }
    assert!(
        replay_mismatches.is_empty(),
        "{}",
        replay_mismatches.join("\n")
    );
}

#[test]
fn committed_csharp_gradual_action_contracts_replay_in_rust() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let mut mismatches = Vec::new();

    for (name, mode) in [
        ("osu", Ruleset::Osu),
        ("taiko", Ruleset::Taiko),
        ("catch", Ruleset::Catch),
        ("mania", Ruleset::Mania),
    ] {
        let contract = fixtures
            .contracts
            .iter()
            .find(|contract| contract.id == format!("gradual-actions-{name}"))
            .expect("missing gradual action contract");
        compare_oracle(
            &replay_gradual_contract(mode, &contract.expected),
            &contract.expected,
            &format!("/{}", contract.id),
            &mut mismatches,
        );
    }

    assert!(
        mismatches.is_empty(),
        "{} gradual C# oracle mismatch(es):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn committed_csharp_custom_scores_replay_every_signed_counter_and_nan_input() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let engine = LiveLazerEngine;
    let parsed = engine.parse(MAP).unwrap();
    let mut mismatches = Vec::new();

    for (name, mode) in [
        ("osu", Ruleset::Osu),
        ("taiko", Ruleset::Taiko),
        ("catch", Ruleset::Catch),
        ("mania", Ruleset::Mania),
    ] {
        let contract = fixtures
            .contracts
            .iter()
            .find(|contract| contract.id == format!("custom-score-{name}"))
            .expect("missing custom score contract");
        let mut score_json = contract.expected["score"].clone();
        score_json["accuracy"] = json!(0.0);
        let mut score: ScoreInfoData = serde_json::from_value(score_json).unwrap();
        score.accuracy = f64::NAN;

        let mut map = engine.convert(&parsed, mode).unwrap();
        if mode == Ruleset::Osu {
            engine
                .apply_mods(
                    &mut map,
                    &[StructuredMod {
                        acronym: "CL".into(),
                        settings: BTreeMap::new(),
                    }],
                )
                .unwrap();
        }
        let actual = json!({
            "score": score,
            "accuracy": engine.calculate_accuracy(&map, score).unwrap(),
            "performance": engine
                .performance_data(&map, &DifficultyConfig::default(), score)
                .unwrap(),
        });
        compare_oracle(
            &actual,
            &contract.expected,
            &format!("/{}", contract.id),
            &mut mismatches,
        );
    }

    assert!(
        mismatches.is_empty(),
        "{} signed/NaN C# oracle mismatch(es):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[test]
fn committed_csharp_mania_performance_branches_replay_exactly() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let contract = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "mania-performance-branch-contract")
        .expect("missing mania performance branch contract");
    let engine = LiveLazerEngine;
    let parsed = engine
        .parse(vector_map("mania-branches/performance-4k.osu"))
        .unwrap();
    let map = engine.convert(&parsed, Ruleset::Mania).unwrap();
    let config = DifficultyConfig::default();
    let attrs = engine.difficulty_data(&map, &config).unwrap();
    let scores = &contract.expected["scores"];
    let score =
        |name: &str| -> ScoreInfoData { serde_json::from_value(scores[name].clone()).unwrap() };
    let performance = |candidate| {
        engine
            .performance_data_from_attrs(&map, &config, &attrs, candidate)
            .unwrap()
    };
    let mut score_v2_map = engine.convert(&parsed, Ruleset::Mania).unwrap();
    engine
        .apply_mods(
            &mut score_v2_map,
            &[StructuredMod {
                acronym: "SV2".into(),
                settings: BTreeMap::new(),
            }],
        )
        .unwrap();
    let score_v2_attrs = engine.difficulty_data(&score_v2_map, &config).unwrap();
    let actual = json!({
        "aggregate": performance(score("perfect")),
        "difficultyGate": {
            "high": performance(score("highAccuracy")),
            "low": performance(score("lowAccuracy")),
        },
        "totalScore": {
            "half": performance(score("totalHalf")),
            "full": performance(score("totalFull")),
        },
        "scoreV2": {
            "base": performance(score("totalHalf")),
            "modded": engine
                .performance_data_from_attrs(
                    &score_v2_map,
                    &config,
                    &score_v2_attrs,
                    score("totalHalf"),
                )
                .unwrap(),
        },
        "totalHits": {
            "normal": performance(score("totalHalf")),
            "long": performance(score("longScore")),
        },
        "scores": scores,
    });
    let mut mismatches = Vec::new();
    compare_oracle(
        &actual,
        &contract.expected,
        "/mania-performance-branch-contract",
        &mut mismatches,
    );
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn committed_csharp_taiko_and_osu_performance_branch_scores_replay_exactly() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let engine = LiveLazerEngine;
    let config = DifficultyConfig::default();
    let mut mismatches = Vec::new();

    let taiko = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "taiko-performance-branch-contract")
        .expect("missing taiko performance branch contract");
    let parsed = engine.parse(TAIKO_DIFFCALC).unwrap();
    let map = engine.convert(&parsed, Ruleset::Taiko).unwrap();
    let attrs = engine.difficulty_data(&map, &config).unwrap();
    let taiko_score = |name: &str| -> ScoreInfoData {
        serde_json::from_value(taiko.expected["scores"][name].clone()).unwrap()
    };
    let actual_taiko = json!({
        "objectCount": taiko.expected["objectCount"],
        "perfect": engine.performance_data_from_attrs(
            &map, &config, &attrs, taiko_score("perfect"),
        ).unwrap(),
        "mixed": engine.performance_data_from_attrs(
            &map, &config, &attrs, taiko_score("mixed"),
        ).unwrap(),
        "scores": taiko.expected["scores"],
    });
    compare_oracle(
        &actual_taiko,
        &taiko.expected,
        "/taiko-performance-branch-contract",
        &mut mismatches,
    );

    let osu = fixtures
        .contracts
        .iter()
        .find(|contract| contract.id == "osu-performance-miss-branches")
        .expect("missing osu performance miss contract");
    let parsed = engine
        .parse(vector_map("osu-branches/slider-miss-performance.osu"))
        .unwrap();
    let map = engine.convert(&parsed, Ruleset::Osu).unwrap();
    let attrs = engine.difficulty_data(&map, &config).unwrap();
    let osu_score = |name: &str| -> ScoreInfoData {
        serde_json::from_value(osu.expected["scores"][name].clone()).unwrap()
    };
    let actual_osu = json!({
        "effectiveMiss": engine.performance_data_from_attrs(
            &map, &config, &attrs, osu_score("effectiveMiss"),
        ).unwrap(),
        "legacyTail": engine.performance_data_from_attrs(
            &map, &config, &attrs, osu_score("legacyTail"),
        ).unwrap(),
        "lazerTick": engine.performance_data_from_attrs(
            &map, &config, &attrs, osu_score("lazerTick"),
        ).unwrap(),
        "scores": osu.expected["scores"],
    });
    compare_oracle(
        &actual_osu,
        &osu.expected,
        "/osu-performance-miss-branches",
        &mut mismatches,
    );
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn committed_csharp_score_generation_boundaries_replay_in_rust() {
    let fixtures: FixtureFile = serde_json::from_str(FIXTURES).unwrap();
    let engine = LiveLazerEngine;
    let parsed = engine.parse(MAP).unwrap();
    let mut mismatches = Vec::new();

    for contract in fixtures
        .contracts
        .iter()
        .filter(|contract| contract.id.starts_with("generated-score-"))
    {
        let mode = ruleset(contract.mode.as_deref().expect("generated score mode"));
        let mut map = engine.convert(&parsed, mode).unwrap();
        if contract.classic {
            engine
                .apply_mods(
                    &mut map,
                    &[StructuredMod {
                        acronym: "CL".into(),
                        settings: BTreeMap::new(),
                    }],
                )
                .unwrap();
        }
        let actual = json!({
            "score": engine
                .create_score(&map, contract.target_accuracy.expect("generated score target"))
                .unwrap(),
        });
        compare_oracle(
            &actual,
            &contract.expected,
            &format!("/{}", contract.id),
            &mut mismatches,
        );
    }

    assert!(
        mismatches.is_empty(),
        "{} generated-score C# mismatch(es):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}

#[cfg(debug_assertions)]
#[test]
fn committed_target_taiko_and_catch_conversion_mappings_replay_exact_rust_pipeline() {
    let engine = LiveLazerEngine;
    let taiko_cases: &[(&str, &[u8], &str)] = &[
        (
            "taiko-basic",
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/taiko/basic.osu"),
            include_str!(
                "../../../tools/oracle/inputs/osu-97e94bab/taiko/basic-expected-conversion.json"
            ),
        ),
        (
            "taiko-samples",
            include_bytes!(
                "../../../tools/oracle/inputs/osu-97e94bab/taiko/sample-to-type-conversions.osu"
            ),
            include_str!(
                "../../../tools/oracle/inputs/osu-97e94bab/taiko/sample-to-type-conversions-expected-conversion.json"
            ),
        ),
        (
            "taiko-slider-v14",
            include_bytes!(
                "../../../tools/oracle/inputs/osu-97e94bab/taiko/slider-conversion-v14.osu"
            ),
            include_str!(
                "../../../tools/oracle/inputs/osu-97e94bab/taiko/slider-conversion-v14-expected-conversion.json"
            ),
        ),
    ];
    let catch_cases: &[(&str, &[u8], &str)] = &[
        (
            "catch-hyperdash",
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/catch/basic-hyperdash.osu"),
            include_str!(
                "../../../tools/oracle/inputs/osu-97e94bab/catch/basic-hyperdash-expected-conversion.json"
            ),
        ),
        (
            "catch-slider",
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/catch/slider.osu"),
            include_str!(
                "../../../tools/oracle/inputs/osu-97e94bab/catch/slider-expected-conversion.json"
            ),
        ),
        (
            "catch-tiny-ticks",
            include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/catch/tiny-ticks.osu"),
            include_str!(
                "../../../tools/oracle/inputs/osu-97e94bab/catch/tiny-ticks-expected-conversion.json"
            ),
        ),
        (
            "catch-spinner-circles",
            include_bytes!(
                "../../../tools/oracle/inputs/osu-97e94bab/catch/spinner-and-circles.osu"
            ),
            include_str!(
                "../../../tools/oracle/inputs/osu-97e94bab/catch/spinner-and-circles-expected-conversion.json"
            ),
        ),
        (
            "catch-precision",
            include_bytes!(
                "../../../tools/oracle/inputs/osu-97e94bab/catch/high-speed-multiplier-precision.osu"
            ),
            include_str!(
                "../../../tools/oracle/inputs/osu-97e94bab/catch/high-speed-multiplier-precision-expected-conversion.json"
            ),
        ),
    ];
    let mut mismatches = Vec::new();

    for (name, input, expected) in taiko_cases {
        let parsed = engine.parse(input).unwrap();
        let mappings =
            rosu_pp::taiko::conversion_debug(parsed.debug_original_map(), parsed.debug_game_mods())
                .unwrap();
        let actual = json!({"Mappings": mappings.iter().map(|mapping| json!({
            "StartTime": mapping.start_time,
            "Objects": mapping.objects.iter().map(|object| json!({
                "StartTime": object.start_time,
                "EndTime": object.end_time,
                "IsRim": object.is_rim,
                "IsCentre": object.is_centre,
                "IsDrumRoll": object.is_drum_roll,
                "IsSwell": object.is_swell,
                "IsStrong": object.is_strong,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()});
        let expected: Value = serde_json::from_str(expected).unwrap();
        compare_oracle(&actual, &expected, &format!("/{name}"), &mut mismatches);
    }

    for (name, input, expected) in catch_cases {
        let parsed = engine.parse(input).unwrap();
        let mappings =
            rosu_pp::catch::conversion_debug(parsed.debug_original_map(), parsed.debug_game_mods())
                .unwrap();
        let actual = json!({"Mappings": mappings.iter().map(|mapping| json!({
            "StartTime": mapping.start_time,
            "Objects": mapping.objects.iter().map(|object| json!({
                "StartTime": object.start_time,
                "Position": object.position,
                "HyperDash": object.hyper_dash,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()});
        let expected: Value = serde_json::from_str(expected).unwrap();
        compare_oracle(&actual, &expected, &format!("/{name}"), &mut mismatches);
    }

    assert!(
        mismatches.is_empty(),
        "{} exact conversion mapping mismatch(es):\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
