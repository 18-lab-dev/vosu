use serde_json::{Value, json};
use std::{collections::BTreeSet, fs};
use vosu_core::server::routes::CachedJson;
#[path = "state_api_compat/oracle.rs"]
mod oracle;
use oracle::*;
#[path = "state_api_compat/bus.rs"]
mod bus;
#[path = "state_api_compat/extensions.rs"]
mod extensions;
#[path = "state_bus_compat/seeds.rs"]
#[expect(
    dead_code,
    reason = "Shared nonzero seeds; the state-bus coverage helper is unused here"
)]
mod seeds;
#[path = "state_api_compat/tournament.rs"]
mod tournament;

/// The pinned corpus's spelling of a typed not-ready outcome. It is accepted
/// only for the no-client case; the HTTP envelope is pinned by server_compat.
const NOT_READY_CORPUS: &[u8] = br#"{"error":"not_ready"}"#;

/// Exactly one `game.focused` member is platform-conditional: Windows reports
/// the all-osu-session foreground OR, other platforms omit it. No other bytes
/// of the committed expectation are rewritten.
fn platform_expected(schema: &str, expected: Vec<u8>) -> Vec<u8> {
    const FOCUSED: &str = r#""game":{"focused":true,"paused":"#;
    if schema != "v2" || expected == NOT_READY_CORPUS {
        return expected;
    }
    let text = String::from_utf8(expected).expect("v2 oracle is UTF-8");
    assert_eq!(text.matches(FOCUSED).count(), 1, "one focused game member");
    assert_eq!(text.matches(r#""focused""#).count(), 1, "no other focus");
    if cfg!(target_os = "windows") {
        text.into_bytes()
    } else {
        text.replacen(FOCUSED, r#""game":{"paused":"#, 1)
            .into_bytes()
    }
}

/// Rejects the counterexample in which an absent or unpublished session
/// produces a not-ready placeholder that happens to equal an expectation.
fn ready(case: &str, schema: &str, outcome: CachedJson) -> Vec<u8> {
    match outcome {
        CachedJson::Ready(bytes) => {
            assert_ne!(bytes.as_ref(), NOT_READY_CORPUS, "{case}/{schema}");
            bytes.to_vec()
        }
        other => panic!("{case}/{schema}: sourced replay was not ready: {other:?}"),
    }
}

const EXPECTED_REVISIONS: Revisions = Revisions {
    upstream_source: "2359305a63f2aaebb201bc8d2eee58177f52590e",
    lazer_calculator: "323bc04198b256b5a00f4e63c5c4cbb506f43cc5",
    osu: "fe30ae7eea2beeb1f0d3a5ce8dde12bb78ca3f48",
    calculator_package: "0.6.1-20260729-main.0",
    calculator_tree: "b12ac98627ab365e373fcfb7505465253f25fcfa",
};
const REQUIRED_COVERAGE: &[&str] = &[
    "no-client",
    "defaults",
    "menu",
    "play",
    "result",
    "lobby",
    "stable",
    "lazer",
    "osu",
    "taiko",
    "catch",
    "mania",
    "HD",
    "FL",
    "DT",
    "HT",
    "CL",
    "DA",
    "structured-speed",
    "leaderboard-empty",
    "leaderboard-populated",
    "stable-tournament",
    "lazer-tournament",
    "precise-short-keys",
    "multi-IPC",
    "SC-equal-BPM",
    "SC-range-BPM",
    "known-omissions",
    "placeholders",
];

#[test]
fn committed_source_oracles_match_bus_adapters_byte_for_byte() {
    let root = fixture_root();
    let inventory: Inventory = serde_json::from_slice(
        &fs::read(root.join("inventory.json")).expect("inventory must exist"),
    )
    .expect("inventory must be valid");
    assert_eq!(inventory.format, 1);
    assert_eq!(
        inventory.revisions.upstream_source,
        EXPECTED_REVISIONS.upstream_source
    );
    assert_eq!(
        inventory.revisions.lazer_calculator,
        EXPECTED_REVISIONS.lazer_calculator
    );
    assert_eq!(inventory.revisions.osu, EXPECTED_REVISIONS.osu);
    assert_eq!(
        inventory.revisions.calculator_package,
        EXPECTED_REVISIONS.calculator_package
    );
    assert_eq!(
        inventory.revisions.calculator_tree,
        EXPECTED_REVISIONS.calculator_tree
    );

    let declared: BTreeSet<_> = inventory
        .required_coverage
        .iter()
        .map(String::as_str)
        .collect();
    let required: BTreeSet<_> = REQUIRED_COVERAGE.iter().copied().collect();
    assert_eq!(
        declared, required,
        "fixture inventory coverage contract changed"
    );

    let covered: BTreeSet<_> = inventory
        .cases
        .iter()
        .flat_map(|case| case.tags.iter().cloned())
        .collect();
    for requirement in &inventory.required_coverage {
        assert!(
            covered.contains(requirement),
            "missing required fixture coverage: {requirement}"
        );
    }
    let mut case_ids = BTreeSet::new();
    for case in &inventory.cases {
        assert!(
            case_ids.insert(case.id.as_str()),
            "duplicate fixture case id: {}",
            case.id
        );
        let schemas: BTreeSet<_> = case.expected.keys().map(String::as_str).collect();
        assert_eq!(
            schemas,
            BTreeSet::from(["sc", "v1", "v2", "v2-precise"]),
            "{} has an incomplete schema inventory",
            case.id
        );
    }

    let mut images = BTreeSet::new();
    for case in inventory.cases {
        let input: Input = serde_json::from_slice(
            &fs::read(root.join(&case.input)).expect("input fixture must exist"),
        )
        .expect("input fixture must be valid");
        assert_eq!(input.id, case.id);
        let replay = replay(&input);
        assert_eq!(
            replay.instances.len(),
            if input.source.is_none() {
                0
            } else if input.tournament.as_deref() == Some("stable") {
                4
            } else {
                1
            }
        );
        if input.source.is_some() {
            let image = bus::published(&replay, &input);
            assert!(
                images.insert(image.to_string()),
                "{}: bus image duplicates another case's input",
                case.id
            );
        }
        for schema in ["v1", "v2", "v2-precise", "sc"] {
            let expected_path = case
                .expected
                .get(schema)
                .unwrap_or_else(|| panic!("{} missing {schema}", case.id));
            let expected = fs::read(root.join(expected_path)).expect("expected fixture must exist");
            let outcome = replay.fixture.outcome(schema);
            let actual = if input.source.is_none() {
                // The only accepted not-ready result: no client was ever attached.
                assert_eq!(outcome, CachedJson::NotReady, "{}/{schema}", case.id);
                assert_eq!(expected, NOT_READY_CORPUS, "{}/{schema}", case.id);
                NOT_READY_CORPUS.to_vec()
            } else {
                ready(&case.id, schema, outcome)
            };
            let expected = platform_expected(schema, expected);
            assert_eq!(
                String::from_utf8_lossy(&actual),
                String::from_utf8_lossy(&expected),
                "oracle mismatch for {} / {schema}",
                case.id
            );
        }
    }
    assert_eq!(images.len(), 12, "every sourced case reached the bus");
}

#[test]
fn guards_reject_vacuous_not_ready_and_unpublished_sources() {
    // A sourced case whose producer never published must not pass as parity.
    let fixture = adapter_fixture::Fixture::new(vosu_osu_model::enums::ClientType::Stable);
    for schema in ["v1", "v2", "v2-precise", "sc"] {
        let outcome = fixture.outcome(schema);
        assert_eq!(outcome, CachedJson::NotReady, "{schema}");
        assert!(
            std::panic::catch_unwind(|| ready("unpublished", schema, outcome)).is_err(),
            "{schema}: a not-ready result must not be accepted for a sourced case"
        );
        assert!(
            std::panic::catch_unwind(|| ready(
                "placeholder",
                schema,
                CachedJson::Ready(NOT_READY_CORPUS.into())
            ))
            .is_err()
        );
    }
    drop(fixture);
    // The focus expectation is a single member edit, never a JSON rewrite.
    let tampered = br#"{"game":{"focused":true,"paused":false},"x":{"focused":true}}"#.to_vec();
    assert!(std::panic::catch_unwind(|| platform_expected("v2", tampered)).is_err());
    let unchanged = br#"{"a":1.0,"b":{"c":"d"}}"#.to_vec();
    assert_eq!(platform_expected("v1", unchanged.clone()), unchanged);
}

#[test]
fn real_sc_calculation_retains_one_object_combo_and_zero_padded_strains() {
    let input: Input =
        serde_json::from_slice(&fs::read(fixture_root().join("sc-equal-bpm/input.json")).unwrap())
            .unwrap();
    // Same resource bytes as the pinned ZeroEngine case, deliberately with the
    // unmodified production engine. Never make real output fit the stub oracle.
    let replay = replay_with_engine(&input, None);
    let bytes = encode("sc", &replay);
    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["maxCombo"], 1);
    let strains = wire["mapStrains"].as_object().unwrap();
    assert_eq!(
        strains.keys().cloned().collect::<Vec<_>>(),
        (0..=10_000)
            .step_by(400)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
    );
    assert!(strains.values().all(|value| value.as_f64() == Some(0.0)));
    assert_eq!(replay.fixture.chart.counts().parses, 1);
}
