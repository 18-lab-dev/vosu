#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authoritative_accuracy_vectors() {
        let osu = LiveScoreState {
            n300: 1,
            n100: 1,
            n50: 1,
            misses: 1,
            slider_end_hits: 1,
            large_tick_hits: 1,
            ..Default::default()
        };
        assert!((accuracy(Ruleset::Osu, osu, false, 2, 2) - 13.2 / 31.2).abs() < 1e-12);

        let mania = LiveScoreState {
            n_geki: 1,
            n300: 1,
            n_katu: 1,
            n100: 1,
            n50: 1,
            misses: 1,
            ..Default::default()
        };
        assert!((accuracy(Ruleset::Mania, mania, false, 0, 0) - 955.0 / 1830.0).abs() < 1e-12);
        assert!((accuracy(Ruleset::Mania, mania, true, 0, 0) - 950.0 / 1800.0).abs() < 1e-12);
    }

    #[test]
    fn score_generation_preserves_wrapper_signed_results() {
        let taiko = simulate_score(
            Ruleset::Taiko,
            ScoreTotals {
                max_combo: 100,
                ..Default::default()
            },
            0.2,
            false,
        )
        .unwrap();
        assert_eq!(taiko.greats, -60);
        assert_eq!(taiko.oks, 160);
        assert!((taiko.accuracy - 0.2).abs() < 1e-12);

        let catch = simulate_score(
            Ruleset::Catch,
            ScoreTotals {
                max_combo: 100,
                fruits: 40,
                droplets: 30,
                tiny_droplets: 30,
                ..Default::default()
            },
            0.95,
            false,
        )
        .unwrap();
        assert_eq!(catch.greats, 40);
        assert_eq!(catch.large_tick_hits, 30);
        assert_eq!(catch.small_tick_hits, 54);
        assert_eq!(catch.small_tick_misses, -24);
        assert!((catch.accuracy - 1.24).abs() < 1e-12);

        assert!(matches!(
            generate_score(
                Ruleset::Catch,
                ScoreTotals {
                    max_combo: 100,
                    fruits: 40,
                    droplets: 30,
                    tiny_droplets: 30,
                    ..Default::default()
                },
                0.95,
                false,
            ),
            Err(Error::ScoreOutOfRange(_))
        ));

        let nonclassic_osu = simulate_score(
            Ruleset::Osu,
            ScoreTotals {
                objects: 4,
                sliders: 2,
                slider_ends: 2,
                large_ticks: 3,
                max_combo: 9,
                ..Default::default()
            },
            0.9,
            false,
        )
        .unwrap();
        assert_eq!(nonclassic_osu.slider_end_hits, 2);
        assert_eq!(nonclassic_osu.large_tick_hits, 0);
        assert_eq!(nonclassic_osu.large_tick_misses, 0);
        assert_eq!(
            nonclassic_osu.accuracy,
            calculate_accuracy(
                Ruleset::Osu,
                &nonclassic_osu,
                false,
                ScoreTotals {
                    objects: 4,
                    sliders: 2,
                    slider_ends: 2,
                    large_ticks: 3,
                    max_combo: 9,
                    ..Default::default()
                },
            ),
        );
    }

    #[test]
    fn empty_accuracy_is_nan_for_every_ruleset() {
        for mode in [Ruleset::Osu, Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania] {
            assert!(
                calculate_accuracy(
                    mode,
                    &ScoreInfoData::default(),
                    false,
                    ScoreTotals::default()
                )
                .is_nan()
            );
        }
    }

    #[test]
    fn gradual_waits_for_object_end_time() {
        let engine = LiveLazerEngine;
        let map = engine
            .parse(include_bytes!("../tests/fixture.osu"))
            .unwrap();
        let mut gradual = engine
            .gradual_difficulty(&map, &DifficultyConfig::default())
            .unwrap();
        gradual.skip_to_time(1_500.0);
        assert_eq!(gradual.processed_objects(), 1);
        gradual.skip_to_time(2_000.0);
        assert_eq!(gradual.processed_objects(), 2);
    }

    #[test]
    fn gradual_empty_and_pre_first_calls_are_stable() {
        let engine = LiveLazerEngine;
        let empty = engine
            .parse(
                br#"osu file format v14

[General]
Mode:0

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:5
ApproachRate:5

[HitObjects]
"#,
            )
            .unwrap();
        let mut gradual = engine
            .gradual_difficulty(&empty, &DifficultyConfig::default())
            .unwrap();

        assert_eq!(gradual.processed_objects(), 0);
        assert!(!gradual.advance());
        assert_eq!(gradual.skip_to_time(f64::NAN), 0);
        gradual.skip(1);
        gradual.skip_to_end();
        assert_eq!(gradual.create_difficulty_attrs().unwrap().max_combo, 0);
        assert_eq!(
            gradual.create_difficulty_attrs().unwrap(),
            gradual.create_difficulty_attrs().unwrap()
        );

        let map = engine
            .parse(include_bytes!("../tests/fixture.osu"))
            .unwrap();
        let mut gradual = engine
            .gradual_difficulty(&map, &DifficultyConfig::default())
            .unwrap();
        let before = gradual.create_difficulty_attrs().unwrap();

        assert_eq!(before.max_combo, 0);
        gradual.skip(-1);
        assert_eq!(gradual.processed_objects(), 0);
        assert_eq!(gradual.skip_to_time(999.0), 0);
        assert_eq!(gradual.processed_objects(), 0);
        assert_eq!(before, gradual.create_difficulty_attrs().unwrap());
        assert!(matches!(
            gradual.get_current_strains(),
            Err(Error::IncompleteOsuStrains)
        ));
    }

    #[test]
    fn gradual_osu_batch_uses_only_final_appended_end_cutoff() {
        let engine = LiveLazerEngine;
        let map = engine
            .parse(
                br#"osu file format v14

[General]
Mode:0

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
SliderMultiplier:1
SliderTickRate:1

[TimingPoints]
0,500,4,2,1,100,1,0

[HitObjects]
64,192,1000,1,0,0:0:0:0:
64,192,1200,2,0,L|512:192,1,600
448,192,1500,1,0,0:0:0:0:
"#,
            )
            .unwrap();
        let config = DifficultyConfig::default();
        let mut advanced = engine.gradual_difficulty(&map, &config).unwrap();
        let mut batched = engine.gradual_difficulty(&map, &config).unwrap();

        while advanced.advance() {}
        batched.skip_to_end();

        assert_eq!(advanced.processed_objects(), 3);
        assert_eq!(batched.processed_objects(), 3);
        let advanced_attrs = advanced.create_difficulty_attrs().unwrap();
        let batched_attrs = batched.create_difficulty_attrs().unwrap();
        assert_eq!(advanced_attrs.max_combo, batched_attrs.max_combo);
        assert_ne!(advanced_attrs.stars, batched_attrs.stars);
    }

    #[test]
    fn gradual_config_difficulty_mods_match_same_handle_playable_fold() {
        let engine = LiveLazerEngine;
        let map = engine
            .parse(include_bytes!("../tests/fixture.osu"))
            .unwrap();
        let hard_rock = StructuredMod {
            acronym: "HR".to_owned(),
            settings: BTreeMap::new(),
        };
        let config = DifficultyConfig {
            mods: vec![hard_rock.clone()],
            ..DifficultyConfig::default()
        };
        let mut handle = map.clone();
        engine.apply_mods(&mut handle, &[hard_rock]).unwrap();
        let mut config_gradual = engine.gradual_difficulty(&map, &config).unwrap();
        let mut handle_gradual = engine
            .gradual_difficulty(&handle, &DifficultyConfig::default())
            .unwrap();

        while config_gradual.advance() {
            assert!(handle_gradual.advance());
            assert_eq!(
                config_gradual.create_difficulty_attrs().unwrap(),
                handle_gradual.create_difficulty_attrs().unwrap()
            );
        }
        assert!(!handle_gradual.advance());

        let mut unmodded = engine
            .gradual_difficulty(&map, &DifficultyConfig::default())
            .unwrap();
        unmodded.skip_to_end();
        assert_ne!(
            config_gradual.create_difficulty_attrs().unwrap().stars,
            unmodded.create_difficulty_attrs().unwrap().stars
        );
    }

    #[test]
    fn gradual_repeated_attrs_and_strains_are_idempotent() {
        let engine = LiveLazerEngine;
        let map = engine
            .parse(include_bytes!("../tests/fixture.osu"))
            .unwrap();
        let mut gradual = engine
            .gradual_difficulty(&map, &DifficultyConfig::default())
            .unwrap();
        gradual.skip_to_end();

        let first = gradual.create_difficulty_attrs().unwrap();
        let second = gradual.create_difficulty_attrs().unwrap();
        assert_eq!(first, second);

        let first_strains = gradual.get_current_strains().unwrap();
        let second_strains = gradual.get_current_strains().unwrap();
        assert_eq!(first_strains, second_strains);
    }

    #[test]
    fn performance_uses_target_accuracy_without_post_scaling() {
        let engine = LiveLazerEngine;
        let map = engine
            .parse(
                br#"osu file format v14

[General]
Mode:0

[Metadata]
Title:accuracy
Artist:vosu
Creator:vosu
Version:test

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
SliderMultiplier:1.4
SliderTickRate:1

[TimingPoints]
0,500,4,2,1,100,1,0

[HitObjects]
64,64,1000,1,0,0:0:0:0:
448,64,1200,1,0,0:0:0:0:
64,320,1400,1,0,0:0:0:0:
448,320,1600,1,0,0:0:0:0:
"#,
            )
            .unwrap();
        let state = LiveScoreState {
            accuracy: 0.9,
            n300: 2,
            n100: 1,
            n50: 1,
            ..Default::default()
        };

        let actual = engine
            .performance(&map, &DifficultyConfig::default(), state)
            .unwrap();
        let expected = Performance::new(&map.map)
            .difficulty(build_difficulty(&map, &DifficultyConfig::default()).unwrap())
            .state(performance_score_state(Ruleset::Osu, state))
            .lazer(true)
            .accuracy(90.0)
            .calculate();

        assert_eq!(actual.pp(), expected.pp());
        let lower_accuracy = Performance::new(&map.map)
            .difficulty(build_difficulty(&map, &DifficultyConfig::default()).unwrap())
            .state(performance_score_state(Ruleset::Osu, state))
            .lazer(true)
            .accuracy(50.0)
            .calculate();
        assert!(
            expected.pp() > lower_accuracy.pp(),
            "{lower_accuracy:?} !< {expected:?}"
        );

        let mania = engine.convert(&map, Ruleset::Mania).unwrap();
        let low = engine
            .performance(
                &mania,
                &DifficultyConfig::default(),
                LiveScoreState {
                    accuracy: 0.5,
                    ..state
                },
            )
            .unwrap()
            .pp();
        let high = engine
            .performance(
                &mania,
                &DifficultyConfig::default(),
                LiveScoreState {
                    accuracy: 0.9,
                    ..state
                },
            )
            .unwrap()
            .pp();

        assert_eq!(low, high);
    }

    #[test]
    fn wrapper_score_metadata_is_preserved_then_deliberately_ignored() {
        let live = LiveScoreState {
            total_score: 987_654,
            is_legacy_score: false,
            accuracy: 0.95,
            max_combo: 2,
            combo_breaks: 3,
            ignore_hits: 4,
            ignore_misses: 5,
            large_bonuses: 6,
            small_bonuses: 7,
            small_tick_misses: 8,
            n300: 2,
            ..Default::default()
        };
        let boundary = ScoreState::from(live);
        assert_eq!(boundary.total_score, 987_654);
        assert!(!boundary.is_legacy_score);
        assert_eq!(boundary.combo_breaks, 3);
        assert_eq!(boundary.ignore_hits, 4);
        assert_eq!(boundary.ignore_misses, 5);
        assert_eq!(boundary.large_bonuses, 6);
        assert_eq!(boundary.small_bonuses, 7);
        assert_eq!(boundary.osu_small_tick_misses, 8);

        let dto = ScoreInfoData::from(live);
        let json = serde_json::to_string(&dto).unwrap();
        let decoded: ScoreInfoData = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, dto);
        assert_eq!(LiveScoreState::try_from(decoded).unwrap(), live);

        let engine = LiveLazerEngine;
        let map = engine
            .parse(
                br#"osu file format v14

[General]
Mode:0

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9

[TimingPoints]
0,500,4,2,1,100,1,0

[HitObjects]
64,64,1000,1,0,0:0:0:0:
448,320,1500,1,0,0:0:0:0:
"#,
            )
            .unwrap();
        let config = DifficultyConfig::default();
        let base = ScoreInfoData {
            accuracy: 1.0,
            max_combo: 2,
            greats: 2,
            ..Default::default()
        };
        let metadata = ScoreInfoData {
            total_score: 987_654,
            combo_breaks: 3,
            ignore_hits: 4,
            ignore_misses: 5,
            large_bonuses: 6,
            small_bonuses: 7,
            ..base
        };
        let base_pp = engine.performance_data(&map, &config, base).unwrap().pp;
        let metadata_pp = engine.performance_data(&map, &config, metadata).unwrap().pp;
        assert_eq!(base_pp, metadata_pp);

        let negative_nonlegacy = ScoreInfoData {
            total_score: -1,
            ..base
        };
        let negative_pp = engine
            .performance_data(&map, &config, negative_nonlegacy)
            .unwrap()
            .pp;
        assert_eq!(base_pp, negative_pp);
    }

    #[test]
    fn structured_setting_names_preserve_acronym_boundaries() {
        assert_eq!(camel_to_snake("AR"), "ar");
        assert_eq!(camel_to_snake("HDFN"), "hdfn");
        assert_eq!(camel_to_snake("initialRate"), "initial_rate");
        assert_eq!(camel_to_snake("XMLParser"), "xml_parser");
    }
}
