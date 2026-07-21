use rosu_mods::GameMod;
use rosu_pp::GameMods;

use std::collections::BTreeMap;

use vosu_lazer_pp::{
    BeatmapDifficultyData, CatchSpatialWindow, DifficultyConfig, HitResult, HitWindows,
    LiveLazerEngine, LiveScoreState, ModValue, Ruleset, ScoreInfoData, ScoreTotals, StructuredMod,
    all_hit_windows, calculate_total_score, catch_spatial_window_for_map, game_mods,
    great_hit_window, hit_window, hit_window_for_map, hit_window_numeric, hit_windows_for_map,
};

const MAP: &[u8] = include_bytes!("fixture.osu");
const OSU_TARGET_DIFFCALC_MAP: &[u8] = include_bytes!("osu-diffcalc-test.osu");
const MANIA_DIFFCALC_MAP: &[u8] =
    include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/mania-diffcalc-test.osu");

#[test]
fn osu_target_difficulty_matches_pinned_source_vectors() {
    let engine = LiveLazerEngine;
    let map = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let no_mod = engine
        .difficulty(&map, &DifficultyConfig::default())
        .unwrap()
        .stars();
    assert!(
        (no_mod - 6.524_317_026_548_358).abs() <= 0.000_01,
        "{no_mod}"
    );

    let dt = DifficultyConfig {
        mods: vec![StructuredMod {
            acronym: "DT".to_owned(),
            settings: BTreeMap::new(),
        }],
        ..DifficultyConfig::default()
    };
    let double_time = engine.difficulty(&map, &dt).unwrap().stars();
    assert!(
        (double_time - 9.467_760_790_064_63).abs() <= 0.000_01,
        "{double_time}"
    );
}

fn full_state(mode: Ruleset) -> LiveScoreState {
    match mode {
        Ruleset::Osu => LiveScoreState {
            accuracy: 0.95,
            max_combo: 4,
            n300: 3,
            n100: 1,
            slider_end_hits: 1,
            large_tick_hits: 1,
            ..Default::default()
        },
        Ruleset::Taiko => LiveScoreState {
            accuracy: 0.95,
            max_combo: 4,
            n300: 3,
            n100: 1,
            ..Default::default()
        },
        Ruleset::Catch => LiveScoreState {
            accuracy: 0.95,
            max_combo: 4,
            n300: 2,
            n100: 1,
            n50: 1,
            large_tick_hits: 1,
            small_tick_hits: 1,
            ..Default::default()
        },
        Ruleset::Mania => LiveScoreState {
            accuracy: 0.95,
            max_combo: 4,
            n_geki: 3,
            n300: 1,
            ..Default::default()
        },
    }
}

#[test]
fn full_and_gradual_performance_cover_every_ruleset() {
    let engine = LiveLazerEngine;
    let osu = engine.parse(MAP).unwrap();

    for mode in [Ruleset::Osu, Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania] {
        let map = engine.convert(&osu, mode).unwrap();
        let full = engine
            .performance(&map, &DifficultyConfig::default(), full_state(mode))
            .unwrap();
        assert!(full.pp().is_finite(), "{mode:?}");

        let mut gradual = engine
            .gradual_difficulty(&map, &DifficultyConfig::default())
            .unwrap();
        gradual.skip_to_end();
        let first = gradual.performance(full_state(mode)).unwrap().pp();
        gradual.rewind();
        assert_eq!(gradual.processed_objects(), 0);
        assert!(gradual.current().is_none());
        gradual.skip_to_end();
        let replay = gradual.performance(full_state(mode)).unwrap().pp();
        assert_eq!(first, replay, "{mode:?}");
    }
}

#[test]
fn non_osu_performance_ignores_independent_accuracy_field() {
    let engine = LiveLazerEngine;
    let osu = engine.parse(MAP).unwrap();

    for mode in [Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania] {
        let map = engine.convert(&osu, mode).unwrap();
        let base = full_state(mode);
        let low = engine
            .performance(
                &map,
                &DifficultyConfig::default(),
                LiveScoreState {
                    accuracy: 0.85,
                    ..base
                },
            )
            .unwrap()
            .pp();
        let high = engine
            .performance(
                &map,
                &DifficultyConfig::default(),
                LiveScoreState {
                    accuracy: 0.99,
                    ..base
                },
            )
            .unwrap()
            .pp();

        assert_eq!(low, high, "{mode:?}");
    }
}

#[test]
fn catch_large_tick_misses_are_performance_misses() {
    let engine = LiveLazerEngine;
    let osu = engine.parse(MAP).unwrap();
    let map = engine.convert(&osu, Ruleset::Catch).unwrap();
    let state = full_state(Ruleset::Catch);
    let clean = engine
        .performance(&map, &DifficultyConfig::default(), state)
        .unwrap()
        .pp();
    let missed = engine
        .performance(
            &map,
            &DifficultyConfig::default(),
            LiveScoreState {
                large_tick_misses: 1,
                accuracy: 0.8,
                ..state
            },
        )
        .unwrap()
        .pp();
    assert!(missed < clean);
}

#[test]
fn conversion_drops_previously_applied_mods() {
    let engine = LiveLazerEngine;
    let map = engine.parse(MAP).unwrap();
    let dt = StructuredMod {
        acronym: "DT".to_owned(),
        settings: BTreeMap::from([("speedChange".to_owned(), ModValue::Number(1.2))]),
    };
    let mut modded = map.clone();
    engine.apply_mods(&mut modded, &[dt]).unwrap();
    let converted_modded = engine.convert(&modded, Ruleset::Taiko).unwrap();
    let converted_plain = engine.convert(&map, Ruleset::Taiko).unwrap();
    assert_eq!(
        engine
            .difficulty(&converted_modded, &DifficultyConfig::default())
            .unwrap()
            .stars(),
        engine
            .difficulty(&converted_plain, &DifficultyConfig::default())
            .unwrap()
            .stars(),
    );
}

#[test]
fn pinned_hit_window_boundaries() {
    assert_eq!(
        hit_window(Ruleset::Osu, 5.0, HitResult::Great).unwrap(),
        Some(49.5)
    );
    assert_eq!(
        hit_window(Ruleset::Taiko, 10.0, HitResult::Ok).unwrap(),
        Some(49.5)
    );
    assert_eq!(
        hit_window(Ruleset::Mania, 0.0, HitResult::Perfect).unwrap(),
        Some(22.5)
    );
    assert_eq!(
        hit_window(Ruleset::Catch, 5.0, HitResult::Great).unwrap(),
        None
    );
    assert!(hit_window(Ruleset::Osu, 5.0, HitResult::Perfect).is_err());
}

#[test]
fn hit_window_enum_and_full_order_match_wrapper() {
    assert_eq!(HitResult::None as i32, 0);
    assert_eq!(HitResult::SliderTailHit as i32, 16);
    assert_eq!(HitResult::LegacyComboIncrease as i32, 99);
    assert_eq!(
        all_hit_windows(Ruleset::Osu, 5.0),
        vec![
            (HitResult::Miss, 400.0),
            (HitResult::Meh, 149.5),
            (HitResult::Ok, 99.5),
            (HitResult::Great, 49.5),
        ]
    );
    assert!(all_hit_windows(Ruleset::Catch, 5.0).is_empty());
    assert_eq!(great_hit_window(Ruleset::Catch, 5.0), 0.0);
}

#[test]
fn osu_score_simulation_clamps_only_osu_accuracy() {
    let engine = LiveLazerEngine;
    let map = engine.parse(MAP).unwrap();
    let zero = engine.create_score(&map, -5.0).unwrap();
    let perfect = engine.create_score(&map, 5.0).unwrap();

    assert_eq!(zero.greats, 0);
    assert_eq!(zero.misses, map.object_count() as i32);
    assert_eq!(perfect.greats, map.object_count() as i32);
    assert_eq!(perfect.misses, 0);

    let taiko = engine.convert(&map, Ruleset::Taiko).unwrap();
    let infeasible = engine.create_score(&taiko, 0.2).unwrap();
    assert!(infeasible.greats < 0 || infeasible.oks < 0);
}

#[test]
fn signed_projection_and_retained_attrs_reach_performance() {
    let engine = LiveLazerEngine;
    let map = engine.parse(MAP).unwrap();
    let attrs = engine
        .difficulty_data(&map, &DifficultyConfig::default())
        .unwrap();
    let score = ScoreInfoData {
        accuracy: 0.5,
        max_combo: 1,
        greats: -2,
        misses: 5,
        ..ScoreInfoData::default()
    };
    let performance = engine
        .performance_data_from_attrs(&map, &DifficultyConfig::default(), &attrs, score)
        .unwrap();

    assert!(performance.pp.is_finite());

    let serialized = serde_json::to_value(&attrs).unwrap();
    assert!(serialized.get("inner").is_none());
}

#[test]
fn retained_attrs_use_current_playable_difficulty_context_in_all_affected_modes() {
    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();

    for (mode, setting) in [
        (Ruleset::Osu, "overallDifficulty"),
        (Ruleset::Taiko, "overallDifficulty"),
        (Ruleset::Catch, "approachRate"),
    ] {
        let mut map = engine.convert(&source, mode).unwrap();
        let attrs = engine
            .difficulty_data(&map, &DifficultyConfig::default())
            .unwrap();
        let score = ScoreInfoData::from(full_state(mode));
        let before = engine
            .performance_data_from_attrs(&map, &DifficultyConfig::default(), &attrs, score)
            .unwrap();
        engine
            .apply_mods(
                &mut map,
                &[StructuredMod {
                    acronym: "DA".to_owned(),
                    settings: BTreeMap::from([(setting.to_owned(), ModValue::Number(1.0))]),
                }],
            )
            .unwrap();
        let after = engine
            .performance_data_from_attrs(&map, &DifficultyConfig::default(), &attrs, score)
            .unwrap();

        assert_ne!(after, before, "{mode:?}");
        engine
            .apply_mods(
                &mut map,
                &[StructuredMod {
                    acronym: "HR".to_owned(),
                    settings: BTreeMap::new(),
                }],
            )
            .unwrap();
        let hard_rock = engine
            .performance_data_from_attrs(&map, &DifficultyConfig::default(), &attrs, score)
            .unwrap();
        assert_ne!(hard_rock, before, "{mode:?} HR");
    }
}

#[test]
fn retained_attrs_reject_every_cross_mode_pair() {
    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let modes = [Ruleset::Osu, Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania];
    let maps = modes
        .iter()
        .map(|&mode| engine.convert(&source, mode).unwrap())
        .collect::<Vec<_>>();
    let attrs = maps
        .iter()
        .map(|map| {
            engine
                .difficulty_data(map, &DifficultyConfig::default())
                .unwrap()
        })
        .collect::<Vec<_>>();

    for (map_index, map) in maps.iter().enumerate() {
        for (attrs_index, attrs) in attrs.iter().enumerate() {
            if map_index == attrs_index {
                continue;
            }

            assert!(matches!(
                engine.performance_data_from_attrs(
                    map,
                    &DifficultyConfig::default(),
                    attrs,
                    ScoreInfoData::from(full_state(modes[map_index])),
                ),
                Err(vosu_lazer_pp::Error::DifficultyModeMismatch {
                    expected,
                    actual,
                }) if expected == modes[map_index] && actual == modes[attrs_index]
            ));
        }
    }
}

#[test]
fn structured_autopilot_is_preserved_as_api_mod() {
    let mods = game_mods(
        Ruleset::Osu,
        &[StructuredMod {
            acronym: "AP".to_owned(),
            settings: BTreeMap::new(),
        }],
    )
    .unwrap();

    assert!(mods.ap());
}

#[test]
fn freeze_frame_is_typed_retained_and_reaches_full_calculation_paths() {
    let structured = StructuredMod {
        acronym: "FR".to_owned(),
        settings: BTreeMap::new(),
    };
    let typed = game_mods(Ruleset::Osu, std::slice::from_ref(&structured)).unwrap();
    assert!(matches!(
        &typed,
        GameMods::Lazer(mods)
            if mods.iter().any(|game_mod| matches!(game_mod, GameMod::FreezeFrameOsu(_)))
    ));

    let engine = LiveLazerEngine;
    let original = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let baseline = engine
        .difficulty(&original, &DifficultyConfig::default())
        .unwrap();
    let mut freeze_frame = original;
    engine
        .apply_mods(&mut freeze_frame, std::slice::from_ref(&structured))
        .unwrap();
    assert_eq!(freeze_frame.structured_mods(), [structured]);
    assert!(matches!(
        freeze_frame.debug_game_mods(),
        GameMods::Lazer(mods)
            if mods.iter().any(|game_mod| matches!(game_mod, GameMod::FreezeFrameOsu(_)))
    ));

    let attrs = engine
        .difficulty(&freeze_frame, &DifficultyConfig::default())
        .unwrap();
    assert_ne!(attrs, baseline);
    let strains = engine
        .strains(&freeze_frame, &DifficultyConfig::default())
        .unwrap();
    assert!(matches!(
        &strains,
        rosu_pp::any::Strains::Osu(strains) if !strains.aim.is_empty()
    ));
    let performance = engine
        .performance_data(
            &freeze_frame,
            &DifficultyConfig::default(),
            ScoreInfoData::from(full_state(Ruleset::Osu)),
        )
        .unwrap();
    assert!(performance.pp.is_finite());
}

#[test]
fn mania_to_taiko_groups_simultaneous_objects() {
    const MANIA: &[u8] = br#"osu file format v14

[General]
Mode:3

[Difficulty]
CircleSize:4
OverallDifficulty:5

[TimingPoints]
0,500,4,2,1,80,1,0

[HitObjects]
64,64,1000,1,0,0:0:0:0:
192,64,1000,1,0,0:0:0:0:
320,64,1000,1,0,0:0:0:0:
448,64,1500,1,0,0:0:0:0:
"#;
    let engine = LiveLazerEngine;
    let mania = engine.parse(MANIA).unwrap();
    let taiko = engine.convert(&mania, Ruleset::Taiko).unwrap();
    let attrs = engine
        .difficulty(&taiko, &DifficultyConfig::default())
        .unwrap();

    assert_eq!(attrs.max_combo(), 2);
}

#[test]
fn gradual_outputs_once_per_top_level_object_in_every_mode() {
    const BASE: &str = r#"osu file format v14

[General]
Mode:{mode}

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:5
ApproachRate:5
SliderMultiplier:1.4
SliderTickRate:1

[TimingPoints]
0,500,4,2,1,80,1,0

[HitObjects]
64,64,1000,1,0,0:0:0:0:
256,192,1500,8,0,2200
448,320,2500,1,0,0:0:0:0:
"#;
    let engine = LiveLazerEngine;

    for mode in [Ruleset::Taiko, Ruleset::Catch] {
        let mode_id = match mode {
            Ruleset::Taiko => 1,
            Ruleset::Catch => 2,
            _ => unreachable!(),
        };
        let source = BASE.replace("{mode}", &mode_id.to_string());
        let map = engine.parse(source.as_bytes()).unwrap();
        let mut gradual = engine
            .gradual_difficulty(&map, &DifficultyConfig::default())
            .unwrap();
        let mut count = 0;

        while gradual.advance() {
            let attrs = gradual.create_difficulty_attrs().unwrap();
            assert!(attrs.stars.is_finite());
            count += 1;
        }

        assert_eq!(count, 3, "{mode:?}");
    }
}

#[test]
fn mania_gradual_uses_end_time_gate_for_overlapping_holds() {
    const MANIA: &[u8] = br#"osu file format v14

[General]
Mode:3

[Difficulty]
CircleSize:4
OverallDifficulty:5

[TimingPoints]
0,500,4,2,1,80,1,0

[HitObjects]
64,64,1000,128,0,3000:0:0:0:0:
192,64,1500,1,0,0:0:0:0:
320,64,3500,1,0,0:0:0:0:
"#;
    let engine = LiveLazerEngine;
    let map = engine.parse(MANIA).unwrap();
    let mut gradual = engine
        .gradual_difficulty(&map, &DifficultyConfig::default())
        .unwrap();

    assert_eq!(gradual.skip_to_time(2000.0), 0);
    assert_eq!(gradual.skip_to_time(3000.0), 2);
    let first = gradual.create_difficulty_attrs().unwrap();
    assert_eq!(gradual.processed_objects(), 2);
    assert!(first.max_combo > gradual.processed_objects() as i32);
    assert_eq!(gradual.skip_to_time(3500.0), 1);
    assert!(!gradual.advance());
}

#[test]
fn gradual_boundary_waits_for_slider_end_and_rewinds() {
    let engine = LiveLazerEngine;
    let map = engine.parse(MAP).unwrap();
    let mut gradual = engine
        .gradual_difficulty(&map, &DifficultyConfig::default())
        .unwrap();
    gradual.skip_to_time(1499.999);
    assert_eq!(gradual.processed_objects(), 1);
    gradual.skip_to_time(2000.0);
    assert_eq!(gradual.processed_objects(), 2);
    let stars = gradual.create_difficulty_attrs().unwrap().stars;
    gradual.rewind();
    gradual.skip(2);
    assert_eq!(gradual.create_difficulty_attrs().unwrap().stars, stars);
}

#[test]
fn classic_mod_reaches_full_attribute_and_gradual_performance_paths() {
    let engine = LiveLazerEngine;
    let osu = engine.parse(MAP).unwrap();
    let classic = DifficultyConfig {
        mods: vec![StructuredMod {
            acronym: "CL".to_owned(),
            settings: BTreeMap::new(),
        }],
        ..DifficultyConfig::default()
    };

    for mode in [Ruleset::Osu, Ruleset::Mania] {
        let map = engine.convert(&osu, mode).unwrap();
        let score = match mode {
            Ruleset::Osu => ScoreInfoData {
                accuracy: 0.8,
                max_combo: 3,
                greats: 3,
                oks: 1,
                ..ScoreInfoData::default()
            },
            Ruleset::Mania => ScoreInfoData {
                accuracy: 0.8,
                max_combo: 3,
                perfects: 2,
                greats: 1,
                oks: 1,
                ..ScoreInfoData::default()
            },
            _ => unreachable!(),
        };

        let attrs = engine.difficulty_data(&map, &classic).unwrap();
        let direct = engine.performance_data(&map, &classic, score).unwrap();
        let retained = engine
            .performance_data_from_attrs(&map, &classic, &attrs, score)
            .unwrap();
        assert!((direct.pp - retained.pp).abs() <= 1e-12, "{mode:?}");

        let state = LiveScoreState::try_from(score).unwrap();
        let full = engine.performance(&map, &classic, state).unwrap().pp();
        assert!((direct.pp - full).abs() <= 1e-12, "{mode:?}");

        let mut gradual = engine.gradual_difficulty(&map, &classic).unwrap();
        gradual.skip_to_end();
        let gradual_pp = gradual.performance(state).unwrap().pp();
        assert!((full - gradual_pp).abs() <= 1e-12, "{mode:?}");

        let lazer = engine
            .performance_data(&map, &DifficultyConfig::default(), score)
            .unwrap();
        if mode == Ruleset::Osu {
            assert_ne!(direct.pp, lazer.pp, "{mode:?}");
        } else {
            assert_eq!(direct.pp, lazer.pp, "{mode:?}");
        }
    }
}

#[test]
fn every_gradual_prefix_has_exact_strains_and_performance_projection() {
    let engine = LiveLazerEngine;
    let osu = engine.parse(MAP).unwrap();

    for mode in [Ruleset::Osu, Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania] {
        let map = engine.convert(&osu, mode).unwrap();
        let mut gradual_attrs = engine
            .gradual_difficulty(&map, &DifficultyConfig::default())
            .unwrap();
        let mut gradual_performance = engine
            .gradual_difficulty(&map, &DifficultyConfig::default())
            .unwrap();
        let mut processed = 0_u32;

        while gradual_attrs.advance() {
            assert!(gradual_performance.advance());
            processed += 1;

            if mode == Ruleset::Osu && processed < map.object_count() {
                assert!(gradual_attrs.get_current_strains().is_err());
            } else {
                let gradual_strains = gradual_attrs.get_current_strains().unwrap();
                assert_eq!(
                    gradual_strains,
                    gradual_attrs.get_current_strains().unwrap(),
                    "{mode:?} prefix {processed}",
                );
            }

            // CreateDifficultyAttrs and performance each receive the same
            // first evaluation of an independently advanced source enumerator.
            let gradual_attrs = gradual_attrs.create_difficulty_attrs().unwrap();

            let score_data = ScoreInfoData::from(full_state(mode));
            let gradual_data = gradual_performance
                .performance_from_score(score_data)
                .unwrap();
            let retained = engine
                .performance_data_from_attrs(
                    &map,
                    &DifficultyConfig::default(),
                    &gradual_attrs,
                    score_data,
                )
                .unwrap();
            assert_eq!(gradual_data.pp, retained.pp, "{mode:?} prefix {processed}");
        }
    }
}

#[test]
fn public_numeric_surface_and_nan_difficulty_range_match_wrapper() {
    let engine = LiveLazerEngine;
    let map = engine.parse_str(std::str::from_utf8(MAP).unwrap()).unwrap();
    assert_eq!(map.mode_id(), 0);
    assert_eq!(Ruleset::from_mode_id(3), Some(Ruleset::Mania));
    assert_eq!(
        HitResult::ALL.map(|result| result as i32),
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 99],
    );
    assert_eq!(serde_json::to_value(HitResult::SliderTailHit).unwrap(), 16);
    assert_eq!(
        serde_json::from_value::<HitResult>(serde_json::json!(99)).unwrap(),
        HitResult::LegacyComboIncrease
    );
    assert!(serde_json::from_value::<HitResult>(serde_json::json!(17)).is_err());
    assert_eq!(hit_window_numeric(2, 5.0, 5).unwrap(), None);
    assert!(hit_window_numeric(0, 5.0, 6).is_err());
    assert_eq!(hit_window_numeric(0, f64::NAN, 5).unwrap(), Some(49.5));
}

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

    fn typed_mods(map: &vosu_lazer_pp::ParsedBeatmap) -> Vec<&GameMod> {
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
    assert_ne!(
        swapped.debug_playable_map().hit_sounds,
        taiko.debug_playable_map().hit_sounds
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
        "ordered duplicate SW applications must cancel",
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
fn difficulty_adjust_extended_od_reaches_playable_map_hit_windows() {
    const NATIVE_TAIKO: &str = r#"osu file format v14

[General]
Mode:1

[Difficulty]
HPDrainRate:5
CircleSize:5
OverallDifficulty:5
ApproachRate:5

[TimingPoints]
0,500,4,2,1,50,1,0

[HitObjects]
64,192,1000,1,0,0:0:0:0:
"#;
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
"#;

    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let difficulty_adjust = StructuredMod {
        acronym: "DA".to_owned(),
        settings: BTreeMap::from([
            ("overall_difficulty".to_owned(), ModValue::Number(11.0)),
            ("extended_limits".to_owned(), ModValue::Bool(true)),
        ]),
    };

    let mut osu = source.clone();
    engine
        .apply_mods(&mut osu, std::slice::from_ref(&difficulty_adjust))
        .unwrap();

    assert_eq!(osu.debug_playable_map().od, 11.0);
    assert_eq!(
        hit_windows_for_map(&osu).unwrap(),
        HitWindows {
            miss: Some(400.0),
            meh: Some(89.5),
            ok: Some(51.5),
            great: Some(13.5),
            ..HitWindows::default()
        },
    );

    let mut taiko = engine.parse_str(NATIVE_TAIKO).unwrap();
    engine
        .apply_mods(&mut taiko, std::slice::from_ref(&difficulty_adjust))
        .unwrap();
    assert_eq!(taiko.debug_playable_map().od, 11.0);
    assert_eq!(
        hit_windows_for_map(&taiko).unwrap(),
        HitWindows {
            miss: Some(64.5),
            ok: Some(43.5),
            great: Some(16.5),
            ..HitWindows::default()
        },
    );

    let mut mania = engine.parse_str(NATIVE_MANIA).unwrap();
    engine
        .apply_mods(&mut mania, std::slice::from_ref(&difficulty_adjust))
        .unwrap();
    assert_eq!(mania.debug_playable_map().od, 11.0);
    assert_eq!(
        hit_windows_for_map(&mania).unwrap(),
        HitWindows {
            miss: Some(155.5),
            meh: Some(118.5),
            ok: Some(94.5),
            good: Some(64.5),
            great: Some(31.5),
            perfect: Some(12.5),
        },
    );
}

#[test]
fn difficulty_adjust_extended_od_uses_classic_mania_windows() {
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
"#;

    let engine = LiveLazerEngine;
    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let classic = StructuredMod {
        acronym: "CL".to_owned(),
        settings: BTreeMap::new(),
    };
    let difficulty_adjust = StructuredMod {
        acronym: "DA".to_owned(),
        settings: BTreeMap::from([
            ("overall_difficulty".to_owned(), ModValue::Number(11.0)),
            ("extended_limits".to_owned(), ModValue::Bool(true)),
        ]),
    };
    let expected = HitWindows {
        miss: Some(158.5),
        meh: Some(121.5),
        ok: Some(97.5),
        good: Some(67.5),
        great: Some(34.5),
        perfect: Some(16.5),
    };

    let mut native = engine.parse_str(NATIVE_MANIA).unwrap();
    engine
        .apply_mods(&mut native, &[classic.clone(), difficulty_adjust.clone()])
        .unwrap();
    assert_eq!(hit_windows_for_map(&native).unwrap(), expected);

    let mut converted = engine.convert(&source, Ruleset::Mania).unwrap();
    engine
        .apply_mods(&mut converted, &[classic, difficulty_adjust])
        .unwrap();
    assert_eq!(hit_windows_for_map(&converted).unwrap(), expected);
}

#[test]
fn map_hit_windows_project_all_official_results_without_clock_rate() {
    const NATIVE_MANIA_OD5: &str = r#"osu file format v14

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
"#;

    let plain = |acronym: &str| StructuredMod {
        acronym: acronym.to_owned(),
        settings: BTreeMap::new(),
    };
    let engine = LiveLazerEngine;

    let source = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    assert_eq!(catch_spatial_window_for_map(&source), None);
    let mut converted_classic = engine.convert(&source, Ruleset::Mania).unwrap();
    engine
        .apply_mods(&mut converted_classic, &[plain("CL")])
        .unwrap();
    assert_eq!(
        hit_windows_for_map(&converted_classic).unwrap(),
        HitWindows {
            miss: Some(158.5),
            meh: Some(121.5),
            ok: Some(97.5),
            good: Some(67.5),
            great: Some(34.5),
            perfect: Some(16.5),
        },
    );

    let mut classic_score_v2 = engine.parse_str(NATIVE_MANIA_OD5).unwrap();
    engine
        .apply_mods(&mut classic_score_v2, &[plain("CL"), plain("SV2")])
        .unwrap();
    assert_eq!(
        hit_windows_for_map(&classic_score_v2).unwrap(),
        HitWindows {
            miss: Some(173.5),
            meh: Some(136.5),
            ok: Some(112.5),
            good: Some(82.5),
            great: Some(49.5),
            perfect: Some(19.5),
        },
    );

    let mut catch = engine.convert(&source, Ruleset::Catch).unwrap();
    assert_eq!(hit_windows_for_map(&catch).unwrap(), HitWindows::default());
    assert_eq!(
        catch_spatial_window_for_map(&catch),
        Some(CatchSpatialWindow {
            half_width: f64::from(48.678_f32),
        }),
    );
    engine.apply_mods(&mut catch, &[plain("EZ")]).unwrap();
    assert_eq!(
        catch_spatial_window_for_map(&catch).map(|window| window.half_width),
        Some(60.633_998_870_849_61),
        "catch uses playable post-mod CS and Catcher f32 conversion order",
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
fn osu_slider_tails_affect_direct_and_progressive_accuracy() {
    let engine = LiveLazerEngine;
    let map = engine.parse(MAP).unwrap();
    let attrs = engine
        .difficulty(&map, &DifficultyConfig::default())
        .unwrap();
    let totals = ScoreTotals::from_difficulty(&attrs);
    assert!(totals.slider_ends > 0);

    let score = engine.create_score(&map, 0.95).unwrap();
    assert_eq!(score.slider_end_hits, totals.slider_ends as i32);
    assert_eq!(score.large_tick_misses, 0);
    assert_eq!(
        engine.calculate_accuracy(&map, score).unwrap(),
        score.accuracy
    );

    let mut gradual = engine
        .gradual_difficulty(&map, &DifficultyConfig::default())
        .unwrap();
    gradual.skip_to_end();
    let progressive = gradual.create_progressive_score(0.95).unwrap();
    assert!(progressive.slider_end_hits > 0);
    assert_eq!(
        gradual.calculate_progressive_accuracy(progressive).unwrap(),
        progressive.accuracy,
    );
}

#[test]
fn irrelevant_negative_fields_are_ignored_per_ruleset() {
    let engine = LiveLazerEngine;
    let osu = engine.parse(MAP).unwrap();

    for mode in [Ruleset::Osu, Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania] {
        let map = engine.convert(&osu, mode).unwrap();
        let mut score = engine.create_score(&map, 0.95).unwrap();
        score.total_score = -1;
        score.is_legacy_score = mode != Ruleset::Osu;
        score.perfects = if mode == Ruleset::Osu {
            -1
        } else {
            score.perfects
        };
        score.goods = if matches!(mode, Ruleset::Osu | Ruleset::Taiko) {
            -1
        } else {
            score.goods
        };
        score.slider_end_hits = if mode != Ruleset::Osu {
            -1
        } else {
            score.slider_end_hits
        };
        score.max_combo = if matches!(mode, Ruleset::Taiko | Ruleset::Mania) {
            -1
        } else {
            score.max_combo
        };

        let result = engine.performance_data(&map, &DifficultyConfig::default(), score);
        assert!(result.is_ok(), "{mode:?}: {result:?}");
    }
}

#[test]
fn gradual_zero_prefix_signed_skip_and_call_order_are_stable() {
    let engine = LiveLazerEngine;
    let osu = engine.parse(MAP).unwrap();

    for mode in [Ruleset::Osu, Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania] {
        let map = engine.convert(&osu, mode).unwrap();
        let mut gradual = engine
            .gradual_difficulty(&map, &DifficultyConfig::default())
            .unwrap();
        let zero_attrs = gradual.create_difficulty_attrs().unwrap();
        assert_eq!(zero_attrs.max_combo, 0, "{mode:?}");
        if mode == Ruleset::Osu {
            assert!(gradual.get_current_strains().is_err());
            assert!(gradual.advance());
            assert!(gradual.get_current_strains().is_err());
            gradual.rewind();
        } else {
            assert_eq!(
                gradual.get_current_strains().unwrap(),
                gradual.get_current_strains().unwrap(),
            );
        }
        gradual.skip(-3);
        assert_eq!(gradual.processed_objects(), 0);
        assert_eq!(
            serde_json::to_value(gradual.create_difficulty_attrs().unwrap()).unwrap(),
            serde_json::to_value(zero_attrs).unwrap(),
        );
        let object_count = map.object_count() as usize;
        assert_eq!(gradual.skip_to_time(f64::NAN), object_count);
        assert_eq!(gradual.processed_objects(), object_count);
        assert!(!gradual.advance());
        let final_strains = gradual.get_current_strains().unwrap();
        assert_eq!(final_strains, gradual.get_current_strains().unwrap());
    }
}

#[test]
fn score_simulator_matches_checked_nonfinite_and_range_boundaries() {
    let osu = ScoreTotals {
        objects: 10,
        ..ScoreTotals::default()
    };
    assert_eq!(
        vosu_lazer_pp::simulate_score(Ruleset::Osu, osu, f64::INFINITY, false)
            .unwrap()
            .greats,
        10
    );
    assert_eq!(
        vosu_lazer_pp::simulate_score(Ruleset::Osu, osu, f64::NEG_INFINITY, false)
            .unwrap()
            .misses,
        10
    );
    assert!(vosu_lazer_pp::simulate_score(Ruleset::Osu, osu, f64::NAN, false).is_err());
    assert!(
        vosu_lazer_pp::simulate_score(Ruleset::Osu, ScoreTotals::default(), 1.0, false).is_err()
    );

    for mode in [Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania] {
        let totals = ScoreTotals {
            objects: 10,
            max_combo: 10,
            fruits: 5,
            droplets: 5,
            tiny_droplets: 5,
            ..ScoreTotals::default()
        };
        assert!(vosu_lazer_pp::simulate_score(mode, totals, f64::NAN, false).is_err());
        assert!(vosu_lazer_pp::simulate_score(mode, totals, f64::INFINITY, false).is_err());
        assert!(vosu_lazer_pp::simulate_score(mode, totals, f64::MAX, false).is_err());
    }
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

#[test]
fn raw_total_score_uses_checked_calculator_long_boundaries() {
    let score = calculate_total_score(0.9, 0.5, 0.75, 100.0, 1.2).unwrap();
    assert_eq!(score.total_score_without_mods, 446_534);
    assert_eq!(score.total_score, 535_841);
    assert!(calculate_total_score(f64::NAN, 1.0, 1.0, 0.0, 1.0).is_err());
}
