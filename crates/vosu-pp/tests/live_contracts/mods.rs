use super::*;

#[test]
fn apply_mods_mutates_same_handle_resets_cache_and_retains_api_order() {
    let engine = LiveLazerEngine;
    let mut map = engine.parse(MAP).unwrap();
    let baseline = engine
        .difficulty(&map, &DifficultyConfig::default())
        .unwrap()
        .stars();
    let mods = [
        StructuredMod {
            acronym: "TP".into(),
            settings: BTreeMap::new(),
        },
        StructuredMod {
            acronym: "TP".into(),
            settings: BTreeMap::new(),
        },
    ];

    engine.apply_mods(&mut map, &mods).unwrap();
    assert_eq!(
        map.structured_mods()
            .iter()
            .map(|structured_mod| structured_mod.acronym.as_str())
            .collect::<Vec<_>>(),
        ["TP", "TP"],
    );
    assert!(map.structured_mods().iter().all(|structured_mod| matches!(
        structured_mod.settings.get("seed"),
        Some(ModValue::Number(seed)) if seed.is_finite()
    )));
    assert_eq!(
        engine.beatmap_difficulty(&map).approach_rate,
        map.debug_playable_map().ar,
    );

    engine.apply_mods(&mut map, &[]).unwrap();
    assert!(map.structured_mods().is_empty());
    assert_eq!(
        engine
            .difficulty(&map, &DifficultyConfig::default())
            .unwrap()
            .stars(),
        baseline
    );
}

#[test]
fn cloned_map_transformations_preserve_original_and_reset_source() {
    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let baseline = engine.beatmap_difficulty(&source);
    let config = DifficultyConfig::default();
    let difficulty = engine.difficulty(&source, &config).unwrap();
    let mut clone = source.clone();
    engine
        .apply_mods(
            &mut clone,
            &[StructuredMod {
                acronym: "DA".to_owned(),
                settings: BTreeMap::from([(
                    "overallDifficulty".to_owned(),
                    ModValue::Number(7.4),
                )]),
            }],
        )
        .unwrap();

    assert_eq!(engine.beatmap_difficulty(&clone).overall_difficulty, 7.4);
    assert_eq!(engine.beatmap_difficulty(&source), baseline);
    assert_eq!(engine.original_beatmap_difficulty(&clone), baseline);
    assert_eq!(engine.difficulty(&source, &config).unwrap(), difficulty);

    engine.apply_mods(&mut clone, &[]).unwrap();
    assert_eq!(engine.beatmap_difficulty(&clone), baseline);
    assert_eq!(engine.difficulty(&clone, &config).unwrap(), difficulty);
}

#[test]
fn ordered_duplicate_difficulty_and_preprocessing_mods_are_not_deduplicated() {
    fn game_mod(acronym: &str, settings: &[(&str, f64)]) -> StructuredMod {
        StructuredMod {
            acronym: acronym.to_owned(),
            settings: settings
                .iter()
                .map(|&(name, value)| (name.to_owned(), ModValue::Number(value)))
                .collect(),
        }
    }

    let engine = LiveLazerEngine;
    let original = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let base = engine.original_beatmap_difficulty(&original);

    let mut easy = original.clone();
    engine
        .apply_mods(&mut easy, &[game_mod("EZ", &[]), game_mod("EZ", &[])])
        .unwrap();
    let display = engine.beatmap_difficulty(&easy);
    assert_eq!(display.approach_rate, base.approach_rate * 0.25);
    assert_eq!(display.circle_size, base.circle_size * 0.25);
    assert_eq!(display.overall_difficulty, base.overall_difficulty * 0.25);
    assert_eq!(display.drain_rate, base.drain_rate * 0.25);

    let mut single_easy = original.clone();
    engine
        .apply_mods(&mut single_easy, &[game_mod("EZ", &[])])
        .unwrap();
    assert_ne!(
        engine
            .difficulty(&single_easy, &DifficultyConfig::default())
            .unwrap()
            .stars(),
        engine
            .difficulty(&easy, &DifficultyConfig::default())
            .unwrap()
            .stars(),
    );

    let tp_a = game_mod("TP", &[("seed", 11.0)]);
    let tp_b = game_mod("TP", &[("seed", 29.0)]);
    let mut ordered_tp = original.clone();
    engine
        .apply_mods(&mut ordered_tp, &[tp_a.clone(), tp_b.clone()])
        .unwrap();
    let mut reversed_tp = original.clone();
    engine.apply_mods(&mut reversed_tp, &[tp_b, tp_a]).unwrap();
    assert_ne!(
        engine
            .difficulty(&ordered_tp, &DifficultyConfig::default())
            .unwrap()
            .stars(),
        engine
            .difficulty(&reversed_tp, &DifficultyConfig::default())
            .unwrap()
            .stars(),
    );

    let random_a = game_mod("RD", &[("seed", 27.0), ("angleSharpness", 0.8)]);
    let random_b = game_mod("RD", &[("seed", 44.0), ("angleSharpness", 0.2)]);
    let mut ordered_random = original.clone();
    engine
        .apply_mods(&mut ordered_random, &[random_a.clone(), random_b.clone()])
        .unwrap();
    let mut reversed_random = original;
    engine
        .apply_mods(&mut reversed_random, &[random_b, random_a])
        .unwrap();
    assert_ne!(
        engine
            .difficulty(&ordered_random, &DifficultyConfig::default())
            .unwrap()
            .stars(),
        engine
            .difficulty(&reversed_random, &DifficultyConfig::default())
            .unwrap()
            .stars(),
    );

    let taiko = engine
        .convert(
            &engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap(),
            Ruleset::Taiko,
        )
        .unwrap();
    let taiko_rd_a = game_mod("RD", &[("seed", 13.0)]);
    let taiko_rd_b = game_mod("RD", &[("seed", 37.0)]);
    let mut ordered_taiko = taiko.clone();
    engine
        .apply_mods(
            &mut ordered_taiko,
            &[taiko_rd_a.clone(), taiko_rd_b.clone()],
        )
        .unwrap();
    let mut reversed_taiko = taiko;
    engine
        .apply_mods(&mut reversed_taiko, &[taiko_rd_b, taiko_rd_a])
        .unwrap();
    assert_ne!(
        engine
            .difficulty(&ordered_taiko, &DifficultyConfig::default())
            .unwrap()
            .stars(),
        engine
            .difficulty(&reversed_taiko, &DifficultyConfig::default())
            .unwrap()
            .stars(),
    );
}

#[test]
fn display_difficulty_uses_retained_values_after_playable_creation() {
    fn apply_once(map: &rosu_pp::Beatmap, mods: GameMods) -> BeatmapDifficultyData {
        let mut builder = map.attributes();
        builder.mods(mods);
        let attrs = builder.build();

        BeatmapDifficultyData {
            approach_rate: attrs.ar(),
            circle_size: attrs.cs(),
            overall_difficulty: attrs.od(),
            drain_rate: attrs.hp(),
        }
    }

    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();

    for mode in [Ruleset::Osu, Ruleset::Taiko, Ruleset::Catch] {
        let mut map = engine.convert(&source, mode).unwrap();
        let base = map.debug_playable_map().clone();
        let hard_rock = StructuredMod {
            acronym: "HR".to_owned(),
            settings: BTreeMap::new(),
        };
        let typed = game_mods(mode, std::slice::from_ref(&hard_rock)).unwrap();
        let applied_once = apply_once(&base, typed.clone());
        engine
            .apply_mods(&mut map, std::slice::from_ref(&hard_rock))
            .unwrap();
        let display = engine.beatmap_difficulty(&map);
        assert_eq!(display, applied_once, "{mode:?} HR retained");

        let setting = if mode == Ruleset::Catch {
            "approachRate"
        } else {
            "overallDifficulty"
        };
        engine
            .apply_mods(
                &mut map,
                &[StructuredMod {
                    acronym: "DA".to_owned(),
                    settings: BTreeMap::from([(setting.to_owned(), ModValue::Number(7.4))]),
                }],
            )
            .unwrap();
        let display = engine.beatmap_difficulty(&map);
        if mode == Ruleset::Catch {
            assert_eq!(display.approach_rate, 7.4);
        } else {
            assert_eq!(display.overall_difficulty, 7.4);
        }
    }
}

#[test]
fn preapplied_random_mods_are_resolved_retained_and_not_applied_twice() {
    fn random(seed: Option<f64>) -> StructuredMod {
        StructuredMod {
            acronym: "RD".to_owned(),
            settings: seed
                .map(|seed| [("seed".to_owned(), ModValue::Number(seed))].into())
                .unwrap_or_default(),
        }
    }

    fn contains_random(mods: &GameMods) -> bool {
        let GameMods::Lazer(mods) = mods else {
            return false;
        };

        mods.iter()
            .any(|game_mod| matches!(game_mod, GameMod::RandomTaiko(_) | GameMod::RandomMania(_)))
    }

    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();

    for mode in [Ruleset::Taiko, Ruleset::Mania] {
        let converted = engine.convert(&source, mode).unwrap();

        let mut explicit = converted.clone();
        engine
            .apply_mods(&mut explicit, &[random(Some(137.0))])
            .unwrap();
        assert!(!contains_random(explicit.debug_game_mods()));

        let direct = rosu_pp::Difficulty::new()
            .lazer(true)
            .calculate(explicit.debug_playable_map())
            .stars();
        let wrapped = engine
            .difficulty(&explicit, &DifficultyConfig::default())
            .unwrap()
            .stars();
        assert_eq!(
            wrapped, direct,
            "{mode:?} must calculate the preprocessed map exactly once"
        );
        match mode {
            Ruleset::Taiko => assert_ne!(
                explicit.debug_playable_map().hit_sounds,
                converted.debug_playable_map().hit_sounds,
                "Taiko RD fixture must mutate hit sounds",
            ),
            Ruleset::Mania => assert_ne!(
                explicit.debug_playable_map().hit_objects,
                converted.debug_playable_map().hit_objects,
                "Mania RD fixture must mutate columns",
            ),
            _ => unreachable!(),
        }

        let strains = engine
            .strains(&explicit, &DifficultyConfig::default())
            .unwrap();
        let has_strains = match strains {
            rosu_pp::any::Strains::Taiko(strains) => {
                !strains.color.is_empty()
                    || !strains.reading.is_empty()
                    || !strains.rhythm.is_empty()
                    || !strains.stamina.is_empty()
            }
            rosu_pp::any::Strains::Mania(strains) => !strains.strains.is_empty(),
            _ => unreachable!(),
        };
        assert!(has_strains, "{mode:?}");
        let performance = engine
            .performance(&explicit, &DifficultyConfig::default(), full_state(mode))
            .unwrap();
        assert!(
            performance.pp().is_finite() && performance.pp() > 0.0,
            "{mode:?}"
        );

        let mut absent = converted;
        engine.apply_mods(&mut absent, &[random(None)]).unwrap();
        let retained = absent.structured_mods().to_vec();
        assert!(matches!(
            retained[0].settings.get("seed"),
            Some(ModValue::Number(seed)) if seed.is_finite()
        ));
        let first = engine
            .difficulty(&absent, &DifficultyConfig::default())
            .unwrap()
            .stars();
        let second = engine
            .difficulty(&absent, &DifficultyConfig::default())
            .unwrap()
            .stars();
        assert_eq!(
            first, second,
            "{mode:?} retained seed must stabilize calculations"
        );
        assert_eq!(absent.structured_mods(), retained);
    }
}

#[test]
fn ordered_taiko_and_mania_playable_mods_are_consumed_once() {
    const TAIKO_RHYTHM: &str = r#"osu file format v14

[General]
Mode:1

[Difficulty]
HPDrainRate:5
CircleSize:5
OverallDifficulty:5
ApproachRate:5

[TimingPoints]
0,600,4,2,1,50,1,0

[HitObjects]
64,192,1000,1,0,0:0:0:0:
128,192,1100,1,0,0:0:0:0:
192,192,1200,1,0,0:0:0:0:
256,192,1300,1,0,0:0:0:0:
320,192,1400,1,0,0:0:0:0:
384,192,1500,1,0,0:0:0:0:
"#;
    fn api_mod(acronym: &str, settings: &[(&str, ModValue)]) -> StructuredMod {
        StructuredMod {
            acronym: acronym.to_owned(),
            settings: settings
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
        }
    }

    fn typed_mods(map: &vosu_pp::ParsedBeatmap) -> Vec<&GameMod> {
        match map.debug_game_mods() {
            GameMods::Lazer(mods) => mods.iter().collect(),
            _ => Vec::new(),
        }
    }

    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let taiko = engine.convert(&source, Ruleset::Taiko).unwrap();

    let mut swapped = taiko.clone();
    engine
        .apply_mods(&mut swapped, &[api_mod("SW", &[])])
        .unwrap();
    assert_eq!(
        swapped.debug_playable_map().hit_sounds,
        taiko.debug_playable_map().hit_sounds,
        "SW is consumed by the taiko wrapper without changing source sounds",
    );
    assert!(
        !typed_mods(&swapped)
            .iter()
            .any(|game_mod| matches!(game_mod, GameMod::SwapTaiko(_)))
    );

    let mut double_swapped = taiko.clone();
    engine
        .apply_mods(
            &mut double_swapped,
            &[api_mod("SW", &[]), api_mod("SW", &[])],
        )
        .unwrap();
    assert_eq!(
        double_swapped.debug_playable_map().hit_sounds,
        taiko.debug_playable_map().hit_sounds,
        "ordered duplicate SW applications retain source sounds",
    );

    let taiko_rhythm = engine.parse_str(TAIKO_RHYTHM).unwrap();
    let mut simplified = taiko_rhythm.clone();
    engine
        .apply_mods(
            &mut simplified,
            &[api_mod(
                "SR",
                &[
                    ("oneThirdConversion", ModValue::String("true".to_owned())),
                    ("oneSixthConversion", ModValue::String("true".to_owned())),
                    ("oneEighthConversion", ModValue::String("true".to_owned())),
                ],
            )],
        )
        .unwrap();
    assert_ne!(
        simplified.debug_playable_map().hit_objects,
        taiko_rhythm.debug_playable_map().hit_objects,
        "SR string settings must be coerced and mutate the fixture rhythm",
    );
    assert!(
        !typed_mods(&simplified)
            .iter()
            .any(|game_mod| matches!(game_mod, GameMod::SimplifiedRhythmTaiko(_)))
    );
    assert!(
        engine
            .performance(
                &simplified,
                &DifficultyConfig::default(),
                full_state(Ruleset::Taiko)
            )
            .unwrap()
            .pp()
            .is_finite()
    );

    let mania = engine.convert(&source, Ruleset::Mania).unwrap();
    let mut mirrored = mania.clone();
    engine
        .apply_mods(&mut mirrored, &[api_mod("MR", &[])])
        .unwrap();
    assert_ne!(
        mirrored.debug_playable_map().hit_objects,
        mania.debug_playable_map().hit_objects
    );
    assert!(
        !typed_mods(&mirrored)
            .iter()
            .any(|game_mod| matches!(game_mod, GameMod::MirrorMania(_)))
    );

    let mut double_mirrored = mania.clone();
    engine
        .apply_mods(
            &mut double_mirrored,
            &[api_mod("MR", &[]), api_mod("MR", &[])],
        )
        .unwrap();
    assert_eq!(
        double_mirrored.debug_playable_map().hit_objects,
        mania.debug_playable_map().hit_objects,
        "ordered duplicate MR applications must cancel",
    );

    for acronym in ["HO", "IN"] {
        let mut transformed = mania.clone();
        engine
            .apply_mods(&mut transformed, &[api_mod(acronym, &[])])
            .unwrap();
        assert_ne!(
            transformed.debug_playable_map().hit_objects,
            mania.debug_playable_map().hit_objects,
            "{acronym} fixture must be nondegenerate",
        );
        assert!(!typed_mods(&transformed).iter().any(|game_mod| matches!(
            (acronym, game_mod),
            ("HO", GameMod::HoldOffMania(_)) | ("IN", GameMod::InvertMania(_))
        )));
    }

    let mut scoring_flags = mania;
    engine
        .apply_mods(
            &mut scoring_flags,
            &[api_mod("SV2", &[]), api_mod("CL", &[])],
        )
        .unwrap();
    let retained = typed_mods(&scoring_flags);
    assert!(
        retained
            .iter()
            .any(|game_mod| matches!(game_mod, GameMod::ScoreV2Mania(_)))
    );
    assert!(
        retained
            .iter()
            .any(|game_mod| matches!(game_mod, GameMod::ClassicMania(_)))
    );
    let attrs = engine
        .difficulty(&scoring_flags, &DifficultyConfig::default())
        .unwrap();
    let perf = engine
        .performance(
            &scoring_flags,
            &DifficultyConfig::default(),
            full_state(Ruleset::Mania),
        )
        .unwrap();
    assert!(attrs.stars().is_finite() && perf.pp().is_finite());
}

#[test]
fn mania_classic_and_score_v2_sidecars_drive_map_scoring() {
    const NATIVE_MANIA: &str = r#"osu file format v14

[General]
Mode:3

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:5
ApproachRate:5

[TimingPoints]
0,500,4,2,1,50,1,0

[HitObjects]
64,192,1000,1,0,0:0:0:0:
192,192,1500,128,0,2000:0:0:0:0:
"#;

    fn plain(acronym: &str) -> StructuredMod {
        StructuredMod {
            acronym: acronym.to_owned(),
            settings: BTreeMap::new(),
        }
    }

    let engine = LiveLazerEngine;
    let mut native = engine.parse_str(NATIVE_MANIA).unwrap();
    engine.apply_mods(&mut native, &[plain("CL")]).unwrap();
    assert!(native.classic_scoring());
    assert_eq!(
        hit_window_for_map(&native, 5.0, HitResult::Great).unwrap(),
        Some(49.5)
    );

    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let converted = engine.convert(&source, Ruleset::Mania).unwrap();
    let mut classic = converted.clone();
    engine.apply_mods(&mut classic, &[plain("CL")]).unwrap();
    assert!(classic.classic_scoring());
    assert_eq!(
        hit_window_for_map(&classic, 5.0, HitResult::Great).unwrap(),
        Some(34.5)
    );

    let classic_score = engine.create_score(&classic, 0.95).unwrap();
    let mut score_v2 = converted;
    engine
        .apply_mods(&mut score_v2, &[plain("CL"), plain("SV2")])
        .unwrap();
    assert!(score_v2.classic_scoring());
    assert_eq!(
        hit_window_for_map(&score_v2, 5.0, HitResult::Great).unwrap(),
        hit_window(Ruleset::Mania, 5.0, HitResult::Great).unwrap(),
    );
    let score_v2_score = engine.create_score(&score_v2, 0.95).unwrap();
    assert_eq!(classic_score, score_v2_score);
    let one_perfect = ScoreInfoData {
        perfects: 1,
        greats: score_v2.object_count() as i32 - 1,
        ..ScoreInfoData::default()
    };
    assert_eq!(
        engine.calculate_accuracy(&score_v2, one_perfect).unwrap(),
        1.0,
        "Classic uses 300/60 weights even when ScoreV2 is also present",
    );
}

#[test]
fn irrelevant_mania_mods_do_not_reprocess_native_maps() {
    let engine = LiveLazerEngine;
    let baseline = engine.parse(MANIA_DIFFCALC_MAP).unwrap();
    let baseline_strains = engine
        .strains(&baseline, &DifficultyConfig::default())
        .unwrap();
    let baseline_difficulty = engine
        .difficulty(&baseline, &DifficultyConfig::default())
        .unwrap();

    let mut autoplay = baseline.clone();
    engine
        .apply_mods(
            &mut autoplay,
            &[StructuredMod {
                acronym: "AT".to_owned(),
                settings: BTreeMap::new(),
            }],
        )
        .unwrap();

    assert!(matches!(
        autoplay.debug_game_mods(),
        GameMods::Lazer(mods)
            if mods
                .iter()
                .any(|game_mod| matches!(game_mod, GameMod::AutoplayMania(_)))
    ));
    assert_eq!(
        autoplay.debug_playable_map().hit_objects,
        baseline.debug_playable_map().hit_objects,
    );
    assert_eq!(
        engine
            .strains(&autoplay, &DifficultyConfig::default())
            .unwrap(),
        baseline_strains,
    );
    assert_eq!(
        engine
            .difficulty(&autoplay, &DifficultyConfig::default())
            .unwrap(),
        baseline_difficulty,
    );
}

#[test]
fn existing_gradual_calculators_observe_same_handle_mod_changes_only() {
    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let mut handle = engine.convert(&source, Ruleset::Mania).unwrap();
    let isolated_handle = handle.clone();
    let mut gradual = engine
        .gradual_difficulty(&handle, &DifficultyConfig::default())
        .unwrap();
    let mut isolated_gradual = engine
        .gradual_difficulty(&isolated_handle, &DifficultyConfig::default())
        .unwrap();
    gradual.skip_to_end();
    isolated_gradual.skip_to_end();
    let score = ScoreInfoData {
        perfects: 1,
        greats: handle.object_count() as i32 - 1,
        ..ScoreInfoData::default()
    };
    let modern = gradual.calculate_progressive_accuracy(score).unwrap();
    assert!(modern < 1.0);

    engine
        .apply_mods(
            &mut handle,
            &[
                StructuredMod {
                    acronym: "CL".to_owned(),
                    settings: BTreeMap::new(),
                },
                StructuredMod {
                    acronym: "SV2".to_owned(),
                    settings: BTreeMap::new(),
                },
            ],
        )
        .unwrap();
    assert_eq!(gradual.calculate_progressive_accuracy(score).unwrap(), 1.0);
    assert_eq!(
        isolated_gradual
            .calculate_progressive_accuracy(score)
            .unwrap(),
        modern,
    );
}

#[test]
fn seedless_config_random_mods_are_cached_for_difficulty_and_strains() {
    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let config = DifficultyConfig {
        mods: vec![StructuredMod {
            acronym: "RD".to_owned(),
            settings: BTreeMap::new(),
        }],
        ..DifficultyConfig::default()
    };

    for mode in [Ruleset::Taiko, Ruleset::Mania] {
        let map = engine.convert(&source, mode).unwrap();
        let first_mods = map.debug_resolved_config_mods(&config.mods);
        let second_mods = map.debug_resolved_config_mods(&config.mods);
        assert_eq!(first_mods, second_mods, "{mode:?}");
        assert!(matches!(
            first_mods[0].settings.get("seed"),
            Some(ModValue::Number(seed)) if seed.is_finite()
        ));

        let first = engine.difficulty(&map, &config).unwrap();
        let second = engine.difficulty(&map, &config).unwrap();
        assert_eq!(first, second, "{mode:?}");
        assert_eq!(
            engine.strains(&map, &config).unwrap(),
            engine.strains(&map, &config).unwrap()
        );
    }
}

#[test]
fn osu_config_map_generation_matches_apply_mods_for_full_and_gradual_paths() {
    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();

    for (acronym, seed) in [("RD", 137.0), ("TP", 29.0)] {
        let structured = StructuredMod {
            acronym: acronym.to_owned(),
            settings: BTreeMap::from([("seed".to_owned(), ModValue::Number(seed))]),
        };
        let config = DifficultyConfig {
            mods: vec![structured.clone()],
            ..DifficultyConfig::default()
        };
        let mut applied = source.clone();
        engine
            .apply_mods(&mut applied, std::slice::from_ref(&structured))
            .unwrap();

        assert_eq!(
            engine.difficulty(&source, &config).unwrap(),
            engine
                .difficulty(&applied, &DifficultyConfig::default())
                .unwrap(),
            "{acronym} difficulty",
        );
        assert_eq!(
            engine.strains(&source, &config).unwrap(),
            engine
                .strains(&applied, &DifficultyConfig::default())
                .unwrap(),
            "{acronym} strains",
        );

        let mut configured_gradual = engine.gradual_difficulty(&source, &config).unwrap();
        let mut applied_gradual = engine
            .gradual_difficulty(&applied, &DifficultyConfig::default())
            .unwrap();
        configured_gradual.skip_to_end();
        applied_gradual.skip_to_end();
        assert_eq!(
            configured_gradual.create_difficulty_attrs().unwrap(),
            applied_gradual.create_difficulty_attrs().unwrap(),
            "{acronym} gradual",
        );
    }

    let easy = StructuredMod {
        acronym: "EZ".to_owned(),
        settings: BTreeMap::new(),
    };
    let duplicate_easy = vec![easy.clone(), easy];
    let config = DifficultyConfig {
        mods: duplicate_easy.clone(),
        ..DifficultyConfig::default()
    };
    let mut applied = source.clone();
    engine.apply_mods(&mut applied, &duplicate_easy).unwrap();
    assert_eq!(
        engine.difficulty(&source, &config).unwrap(),
        engine
            .difficulty(&applied, &DifficultyConfig::default())
            .unwrap(),
        "duplicate EZ difficulty",
    );
    assert_eq!(
        engine.strains(&source, &config).unwrap(),
        engine
            .strains(&applied, &DifficultyConfig::default())
            .unwrap(),
        "duplicate EZ strains",
    );
    let mut configured_gradual = engine.gradual_difficulty(&source, &config).unwrap();
    let mut applied_gradual = engine
        .gradual_difficulty(&applied, &DifficultyConfig::default())
        .unwrap();
    configured_gradual.skip_to_end();
    applied_gradual.skip_to_end();
    assert_eq!(
        configured_gradual.create_difficulty_attrs().unwrap(),
        applied_gradual.create_difficulty_attrs().unwrap(),
        "duplicate EZ gradual",
    );
}

#[test]
fn api_mod_settings_are_snake_cased_and_malformed_values_are_tolerated() {
    let malformed: StructuredMod = serde_json::from_value(serde_json::json!({
        "acronym": "DT",
        "settings": {
            "speedChange": {"not": "a number"},
            "initialRate": 1.2,
            "HDFN": [true, null],
            "unknownSetting": {"nested": [1, false]}
        }
    }))
    .unwrap();

    let mods = game_mods(Ruleset::Osu, &[malformed]).unwrap();
    assert_eq!(mods.clock_rate(), 1.5);
    let valid: StructuredMod = serde_json::from_value(serde_json::json!({
        "acronym": "DT",
        "settings": { "speedChange": 1.25 }
    }))
    .unwrap();
    assert_eq!(
        game_mods(Ruleset::Osu, &[valid]).unwrap().clock_rate(),
        1.25
    );
    let coercible: Vec<StructuredMod> = serde_json::from_value(serde_json::json!([
        {
            "acronym": "RD",
            "settings": { "angleSharpness": "5" }
        },
        {
            "acronym": "HD",
            "settings": { "onlyFadeApproachCircles": "true" }
        }
    ]))
    .unwrap();
    let GameMods::Lazer(mods) = game_mods(Ruleset::Osu, &coercible).unwrap() else {
        panic!("structured mods must remain lazer mods");
    };
    assert!(mods.iter().any(|game_mod| matches!(
        game_mod,
        GameMod::RandomOsu(settings) if settings.angle_sharpness == Some(5.0)
    )));
    assert!(mods.iter().any(|game_mod| matches!(
        game_mod,
        GameMod::HiddenOsu(settings)
            if settings.only_fade_approach_circles == Some(true)
    )));
}

#[test]
fn conversion_time_mania_mods_match_applied_playable_and_gradual_paths() {
    let engine = LiveLazerEngine;
    let osu = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let mania = engine.convert(&osu, Ruleset::Mania).unwrap();

    for acronym in ["7K", "DS"] {
        let game_mod = StructuredMod {
            acronym: acronym.to_owned(),
            settings: BTreeMap::new(),
        };
        let config = DifficultyConfig {
            mods: vec![game_mod.clone()],
            ..DifficultyConfig::default()
        };
        let configured = engine.difficulty_data(&mania, &config).unwrap();

        let mut applied = mania.clone();
        engine.apply_mods(&mut applied, &[game_mod]).unwrap();
        let applied_attrs = engine
            .difficulty_data(&applied, &DifficultyConfig::default())
            .unwrap();
        assert_eq!(
            serde_json::to_value(configured).unwrap(),
            serde_json::to_value(applied_attrs).unwrap(),
            "{acronym}",
        );

        let mut configured_gradual = engine.gradual_difficulty(&mania, &config).unwrap();
        let mut applied_gradual = engine
            .gradual_difficulty(&applied, &DifficultyConfig::default())
            .unwrap();
        configured_gradual.skip_to_end();
        applied_gradual.skip_to_end();
        assert_eq!(
            serde_json::to_value(configured_gradual.create_difficulty_attrs().unwrap()).unwrap(),
            serde_json::to_value(applied_gradual.create_difficulty_attrs().unwrap()).unwrap(),
            "{acronym}",
        );
    }
}
