#[path = "live_contracts/gradual.rs"]
mod gradual;
#[path = "live_contracts/hit_windows.rs"]
mod hit_windows;
#[path = "live_contracts/mods.rs"]
mod mods;
use rosu_mods::GameMod;
use rosu_pp::GameMods;

use std::collections::BTreeMap;

use vosu_pp::{
    BeatmapDifficultyData, CatchSpatialWindow, DifficultyConfig, HitResult, HitWindows,
    LiveLazerEngine, LiveScoreState, ModValue, Ruleset, ScoreInfoData, ScoreTotals, StructuredMod,
    all_hit_windows, calculate_total_score, catch_spatial_window_for_map, game_mods,
    great_hit_window, hit_window, hit_window_for_map, hit_window_numeric, hit_windows_for_map,
    hit_windows_for_map_without_speed_adjustment,
};

const MAP: &[u8] = include_bytes!("fixture.osu");
const OSU_TARGET_DIFFCALC_MAP: &[u8] = include_bytes!("osu-diffcalc-test.osu");
const MANIA_DIFFCALC_MAP: &[u8] =
    include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/mania-diffcalc-test.osu");
const MANIA_SIMULTANEOUS_CHORD_MAP: &[u8] =
    include_bytes!("../../../tools/oracle/inputs/osu-97e94bab/mania-branches/overall-chord.osu");

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
fn passed_objects_limit_full_difficulty_and_strains() {
    let engine = LiveLazerEngine;
    let map = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let full_config = DifficultyConfig::default();
    let prefix_config = DifficultyConfig {
        passed_objects: Some(10),
        ..DifficultyConfig::default()
    };
    let full = engine.difficulty_data(&map, &full_config).unwrap();
    let prefix = engine.difficulty_data(&map, &prefix_config).unwrap();
    assert!(prefix.max_combo < full.max_combo);
    assert_ne!(prefix.stars, full.stars);
    let full_direct = engine.difficulty(&map, &full_config).unwrap();
    let prefix_direct = engine.difficulty(&map, &prefix_config).unwrap();
    assert_eq!(full_direct.stars(), full.stars);
    assert_eq!(prefix_direct.stars(), prefix.stars);
    assert_eq!(full_direct.max_combo(), full.max_combo as u32);
    assert_eq!(prefix_direct.max_combo(), prefix.max_combo as u32);

    let full_strains = engine.strains_data(&map, &full_config).unwrap();
    let prefix_strains = engine.strains_data(&map, &prefix_config).unwrap();
    assert_ne!(prefix_strains, full_strains);

    let oversized_config = DifficultyConfig {
        passed_objects: Some(u32::MAX),
        ..DifficultyConfig::default()
    };
    let oversized = engine.difficulty_data(&map, &oversized_config).unwrap();
    assert_eq!(oversized.stars, full.stars);
    assert_eq!(oversized.max_combo, full.max_combo);
}

#[test]
fn mania_gradual_simultaneous_chord_matches_csharp_time_boundary() {
    let engine = LiveLazerEngine;
    let map = engine
        .convert(
            &engine.parse(MANIA_SIMULTANEOUS_CHORD_MAP).unwrap(),
            Ruleset::Mania,
        )
        .unwrap();
    let mut gradual = engine
        .gradual_difficulty(&map, &DifficultyConfig::default())
        .unwrap();

    for processed in 1..=3 {
        assert!(gradual.advance());

        let attrs = gradual.create_difficulty_attrs().unwrap();
        assert!(
            (attrs.stars - 0.108).abs() <= f64::EPSILON,
            "prefix {processed}"
        );
        assert_eq!(attrs.max_combo, processed, "prefix {processed}");
        assert_eq!(attrs.n_objects, processed, "prefix {processed}");
        assert_eq!(gradual.processed_objects(), processed as usize);
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
fn attrs_backed_paths_use_the_supplied_immutable_context() {
    let engine = LiveLazerEngine;
    let mut map = engine.parse(OSU_TARGET_DIFFCALC_MAP).unwrap();
    let config = DifficultyConfig::default();
    let full = engine.difficulty_data(&map, &config).unwrap();
    let score = ScoreInfoData::from(full_state(Ruleset::Osu));
    let performance = engine
        .performance_data_from_attrs_with_context(&map, &config, &full, &full, score)
        .unwrap();
    let accuracy = engine
        .calculate_accuracy_from_attrs(&map, &full, score)
        .unwrap();

    engine
        .apply_mods(
            &mut map,
            &[StructuredMod {
                acronym: "DA".to_owned(),
                settings: BTreeMap::from([("overallDifficulty".to_owned(), ModValue::Number(1.0))]),
            }],
        )
        .unwrap();

    assert_eq!(
        engine
            .performance_data_from_attrs_with_context(&map, &config, &full, &full, score)
            .unwrap(),
        performance,
    );
    assert_eq!(
        engine
            .calculate_accuracy_from_attrs(&map, &full, score)
            .unwrap(),
        accuracy,
    );
    assert_ne!(
        engine
            .performance_data_from_attrs(&map, &config, &full, score)
            .unwrap(),
        performance,
    );
}

#[test]
fn catch_attrs_context_retains_effective_performance_mods() {
    let engine = LiveLazerEngine;
    let source = engine.parse(MAP).unwrap();
    let map = engine.convert(&source, Ruleset::Catch).unwrap();
    let hidden = DifficultyConfig {
        mods: vec![StructuredMod {
            acronym: "HD".to_owned(),
            settings: BTreeMap::new(),
        }],
        ..DifficultyConfig::default()
    };
    let attrs = engine.difficulty_data(&map, &hidden).unwrap();
    let score = ScoreInfoData::from(full_state(Ruleset::Catch));
    let expected = engine.performance_data(&map, &hidden, score).unwrap();
    let retained = engine
        .performance_data_from_attrs_with_context(
            &map,
            &DifficultyConfig::default(),
            &attrs,
            &attrs,
            score,
        )
        .unwrap();

    assert_eq!(retained, expected);
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
                Err(vosu_pp::Error::DifficultyModeMismatch {
                    expected,
                    actual,
                }) if expected == modes[map_index] && actual == modes[attrs_index]
            ));
            assert!(matches!(
                engine.create_score_from_attrs(map, attrs, 1.0),
                Err(vosu_pp::Error::DifficultyModeMismatch {
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
fn catch_performance_uses_configured_hd_fl_and_nf_mods() {
    let engine = LiveLazerEngine;
    let osu = engine.parse(MAP).unwrap();
    let map = engine.convert(&osu, Ruleset::Catch).unwrap();
    let score = ScoreInfoData {
        misses: 2,
        ..ScoreInfoData::from(full_state(Ruleset::Catch))
    };
    let base = engine
        .performance_data(&map, &DifficultyConfig::default(), score)
        .unwrap()
        .pp;

    for acronym in ["HD", "FL", "NF"] {
        let config = DifficultyConfig {
            mods: vec![StructuredMod {
                acronym: acronym.to_owned(),
                settings: BTreeMap::new(),
            }],
            ..DifficultyConfig::default()
        };
        let direct = engine.performance_data(&map, &config, score).unwrap();
        let attrs = engine.difficulty_data(&map, &config).unwrap();
        let retained = engine
            .performance_data_from_attrs(&map, &config, &attrs, score)
            .unwrap();

        assert_ne!(direct.pp, base, "{acronym}");
        assert!((direct.pp - retained.pp).abs() <= 1e-12, "{acronym}");
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
fn score_simulator_matches_checked_nonfinite_and_range_boundaries() {
    let osu = ScoreTotals {
        objects: 10,
        ..ScoreTotals::default()
    };
    assert_eq!(
        vosu_pp::simulate_score(Ruleset::Osu, osu, f64::INFINITY, false)
            .unwrap()
            .greats,
        10
    );
    assert_eq!(
        vosu_pp::simulate_score(Ruleset::Osu, osu, f64::NEG_INFINITY, false)
            .unwrap()
            .misses,
        10
    );
    assert!(vosu_pp::simulate_score(Ruleset::Osu, osu, f64::NAN, false).is_err());
    assert!(vosu_pp::simulate_score(Ruleset::Osu, ScoreTotals::default(), 1.0, false).is_err());

    for mode in [Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania] {
        let totals = ScoreTotals {
            objects: 10,
            max_combo: 10,
            fruits: 5,
            droplets: 5,
            tiny_droplets: 5,
            ..ScoreTotals::default()
        };
        assert!(vosu_pp::simulate_score(mode, totals, f64::NAN, false).is_err());
        assert!(vosu_pp::simulate_score(mode, totals, f64::INFINITY, false).is_err());
        assert!(vosu_pp::simulate_score(mode, totals, f64::MAX, false).is_err());
    }
}

#[test]
fn raw_total_score_uses_checked_calculator_long_boundaries() {
    let score = calculate_total_score(0.9, 0.5, 0.75, 100.0, 1.2).unwrap();
    assert_eq!(score.total_score_without_mods, 446_534);
    assert_eq!(score.total_score, 535_841);
    assert!(calculate_total_score(f64::NAN, 1.0, 1.0, 0.0, 1.0).is_err());
}
