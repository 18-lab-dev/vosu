use super::*;

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
