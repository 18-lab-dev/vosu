use serde_json::Value;
use std::{collections::BTreeSet, fs};
#[path = "state_api_compat/oracle.rs"]
mod oracle;
use oracle::*;

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
fn committed_source_oracles_match_all_rust_wire_builders_byte_for_byte() {
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

    for case in inventory.cases {
        let input: Input = serde_json::from_slice(
            &fs::read(root.join(&case.input)).expect("input fixture must exist"),
        )
        .expect("input fixture must be valid");
        assert_eq!(input.id, case.id);
        let snapshot = manager(&input);
        for schema in ["v1", "v2", "v2-precise", "sc"] {
            let expected_path = case
                .expected
                .get(schema)
                .unwrap_or_else(|| panic!("{} missing {schema}", case.id));
            let expected = fs::read(root.join(expected_path)).expect("expected fixture must exist");
            #[cfg(not(target_os = "windows"))]
            let expected = if schema == "v2" {
                let mut value: Value =
                    serde_json::from_slice(&expected).expect("expected v2 fixture must be valid");
                if let Some(game) = value.get_mut("game").and_then(Value::as_object_mut) {
                    game.remove("focused");
                }
                serde_json::to_vec(&value).expect("normalized v2 fixture must encode")
            } else {
                expected
            };
            let mut actual: Value = serde_json::from_slice(&encode(schema, &snapshot))
                .expect("actual output must be valid");
            strip_vosu_extensions(&mut actual);
            if matches!(schema, "v2" | "v2-precise") {
                let object = actual.as_object_mut().expect("v2 output must be an object");
                if schema == "v2-precise" {
                    if let Some(keys) = object.get_mut("keys").and_then(Value::as_object_mut) {
                        keys.shift_remove("all");
                    }
                    if let Some(tourney) = object.get_mut("tourney").and_then(Value::as_array_mut) {
                        for entry in tourney {
                            entry
                                .get_mut("keys")
                                .and_then(Value::as_object_mut)
                                .expect("v2 precise tournament keys must be an object")
                                .shift_remove("all");
                        }
                    }
                } else {
                    // Mode-agnostic keys are a vosu extension to the source schema.
                    object.shift_remove("keys");
                }
            }
            let actual = serde_json::to_vec(&actual).expect("normalized output must encode");
            assert_eq!(
                String::from_utf8_lossy(&actual),
                String::from_utf8_lossy(&expected),
                "oracle mismatch for {} / {schema}",
                case.id
            );
        }
    }
}
