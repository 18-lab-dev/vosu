use super::*;

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
