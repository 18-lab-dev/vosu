use super::*;

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
