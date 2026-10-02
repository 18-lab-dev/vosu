use super::*;

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
    assert_eq!(
        hit_windows_for_map(&catch).unwrap(),
        HitWindows {
            miss: Some(0.0),
            meh: Some(0.0),
            ok: Some(0.0),
            good: Some(0.0),
            great: Some(0.0),
            perfect: Some(0.0),
        }
    );
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
fn map_windows_apply_official_difficulty_and_fixed_rate_modifiers() {
    const NATIVE_MAP: &str = r#"osu file format v14

[General]
Mode:{mode}

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

    fn map_for_mode(engine: &LiveLazerEngine, mode: u8) -> vosu_pp::ParsedBeatmap {
        engine
            .parse_str(&NATIVE_MAP.replace("{mode}", &mode.to_string()))
            .unwrap()
    }

    fn game_mod(
        acronym: &str,
        settings: impl IntoIterator<Item = (&'static str, ModValue)>,
    ) -> StructuredMod {
        StructuredMod {
            acronym: acronym.to_owned(),
            settings: settings
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value))
                .collect(),
        }
    }

    fn apply(engine: &LiveLazerEngine, mode: u8, mods: &[StructuredMod]) -> vosu_pp::ParsedBeatmap {
        let mut map = map_for_mode(engine, mode);
        engine.apply_mods(&mut map, mods).unwrap();
        map
    }

    let engine = LiveLazerEngine;
    let easy = game_mod("EZ", []);
    let hard_rock = game_mod("HR", []);
    let difficulty_adjust = game_mod(
        "DA",
        [
            ("overall_difficulty", ModValue::Number(8.0)),
            ("circle_size", ModValue::Number(7.0)),
        ],
    );

    let osu_easy = apply(&engine, 0, std::slice::from_ref(&easy));
    assert_eq!(
        hit_windows_for_map(&osu_easy).unwrap(),
        HitWindows {
            miss: Some(400.0),
            meh: Some(174.5),
            ok: Some(119.5),
            great: Some(64.5),
            ..HitWindows::default()
        },
    );
    let osu_hard_rock = apply(&engine, 0, std::slice::from_ref(&hard_rock));
    assert_eq!(
        hit_windows_for_map(&osu_hard_rock).unwrap(),
        HitWindows {
            miss: Some(400.0),
            meh: Some(129.5),
            ok: Some(83.5),
            great: Some(37.5),
            ..HitWindows::default()
        },
    );
    let osu_difficulty_adjust = apply(&engine, 0, std::slice::from_ref(&difficulty_adjust));
    assert_eq!(
        hit_windows_for_map(&osu_difficulty_adjust).unwrap(),
        HitWindows {
            miss: Some(400.0),
            meh: Some(119.5),
            ok: Some(75.5),
            great: Some(31.5),
            ..HitWindows::default()
        },
    );

    let taiko_easy = apply(&engine, 1, std::slice::from_ref(&easy));
    assert_eq!(
        hit_windows_for_map(&taiko_easy).unwrap(),
        HitWindows {
            miss: Some(114.5),
            ok: Some(99.5),
            great: Some(41.5),
            ..HitWindows::default()
        },
    );
    let taiko_hard_rock = apply(&engine, 1, std::slice::from_ref(&hard_rock));
    assert_eq!(
        hit_windows_for_map(&taiko_hard_rock).unwrap(),
        HitWindows {
            miss: Some(84.5),
            ok: Some(67.5),
            great: Some(28.5),
            ..HitWindows::default()
        },
    );
    let taiko_difficulty_adjust = apply(&engine, 1, std::slice::from_ref(&difficulty_adjust));
    assert_eq!(
        hit_windows_for_map(&taiko_difficulty_adjust).unwrap(),
        HitWindows {
            miss: Some(79.5),
            ok: Some(61.5),
            great: Some(25.5),
            ..HitWindows::default()
        },
    );

    for (game_mod, expected) in [
        (&easy, 57.645_000_457_763_67),
        (&hard_rock, 33.733_001_708_984_375),
        (&difficulty_adjust, 30.744_001_388_549_805),
    ] {
        let catch = apply(&engine, 2, std::slice::from_ref(game_mod));
        assert_eq!(
            catch_spatial_window_for_map(&catch),
            Some(CatchSpatialWindow {
                half_width: expected,
            }),
        );
    }

    let mania_easy = apply(&engine, 3, std::slice::from_ref(&easy));
    assert_eq!(
        hit_windows_for_map(&mania_easy).unwrap(),
        HitWindows {
            miss: Some(242.5),
            meh: Some(190.5),
            ok: Some(156.5),
            good: Some(114.5),
            great: Some(68.5),
            perfect: Some(27.5),
        },
    );
    let mania_hard_rock = apply(&engine, 3, std::slice::from_ref(&hard_rock));
    assert_eq!(
        hit_windows_for_map(&mania_hard_rock).unwrap(),
        HitWindows {
            miss: Some(123.5),
            meh: Some(97.5),
            ok: Some(80.5),
            good: Some(58.5),
            great: Some(35.5),
            perfect: Some(13.5),
        },
    );
    let mania_difficulty_adjust = apply(&engine, 3, std::slice::from_ref(&difficulty_adjust));
    assert_eq!(
        hit_windows_for_map(&mania_difficulty_adjust).unwrap(),
        HitWindows {
            miss: Some(164.5),
            meh: Some(127.5),
            ok: Some(103.5),
            good: Some(73.5),
            great: Some(40.5),
            perfect: Some(16.5),
        },
    );

    let faster = HitWindows {
        miss: Some(259.5),
        meh: Some(204.5),
        ok: Some(168.5),
        good: Some(123.5),
        great: Some(73.5),
        perfect: Some(29.5),
    };
    for acronym in ["DT", "NC"] {
        let mania = apply(&engine, 3, &[game_mod(acronym, [])]);
        assert_eq!(hit_windows_for_map(&mania).unwrap(), faster, "{acronym}");
    }

    let slower = HitWindows {
        miss: Some(129.5),
        meh: Some(102.5),
        ok: Some(84.5),
        good: Some(61.5),
        great: Some(36.5),
        perfect: Some(14.5),
    };
    for acronym in ["HT", "DC"] {
        let mania = apply(&engine, 3, &[game_mod(acronym, [])]);
        assert_eq!(hit_windows_for_map(&mania).unwrap(), slower, "{acronym}");
    }

    let custom_rate = apply(
        &engine,
        3,
        &[game_mod("DT", [("speed_change", ModValue::Number(1.234))])],
    );
    assert_eq!(
        hit_windows_for_map(&custom_rate).unwrap(),
        HitWindows {
            miss: Some(212.5),
            meh: Some(167.5),
            ok: Some(137.5),
            good: Some(100.5),
            great: Some(60.5),
            perfect: Some(23.5),
        },
        "fixed rates clamp and round to two decimal places before Mania source rounding",
    );

    let baseline_mania = HitWindows {
        miss: Some(173.5),
        meh: Some(136.5),
        ok: Some(112.5),
        good: Some(82.5),
        great: Some(49.5),
        perfect: Some(19.5),
    };
    for acronym in ["AS", "WU", "WD"] {
        let mania = apply(
            &engine,
            3,
            &[game_mod(
                acronym,
                [("initial_rate", ModValue::Number(1.75))],
            )],
        );
        assert_eq!(
            hit_windows_for_map(&mania).unwrap(),
            baseline_mania,
            "{acronym} does not set ManiaHitWindows.SpeedMultiplier",
        );
    }

    let mut native_classic = map_for_mode(&engine, 3);
    engine
        .apply_mods(&mut native_classic, &[game_mod("CL", []), easy])
        .unwrap();
    assert_eq!(
        hit_windows_for_map(&native_classic).unwrap(),
        HitWindows {
            miss: Some(242.5),
            meh: Some(190.5),
            ok: Some(156.5),
            good: Some(114.5),
            great: Some(68.5),
            perfect: Some(22.5),
        },
        "native Classic applies Easy before floor-plus-half",
    );

    let mut converted_classic = map_for_mode(&engine, 0).convert(Ruleset::Mania).unwrap();
    engine
        .apply_mods(
            &mut converted_classic,
            &[game_mod("CL", []), game_mod("DT", [])],
        )
        .unwrap();
    assert_eq!(
        hit_windows_for_map(&converted_classic).unwrap(),
        HitWindows {
            miss: Some(237.5),
            meh: Some(181.5),
            ok: Some(145.5),
            good: Some(100.5),
            great: Some(51.5),
            perfect: Some(24.5),
        },
        "converted Classic applies fixed speed before floor-plus-half",
    );

    let classic_score_v2 = apply(
        &engine,
        3,
        &[game_mod("CL", []), game_mod("SV2", []), game_mod("HR", [])],
    );
    assert_eq!(
        hit_windows_for_map(&classic_score_v2).unwrap(),
        HitWindows {
            miss: Some(123.5),
            meh: Some(97.5),
            ok: Some(80.5),
            good: Some(58.5),
            great: Some(35.5),
            perfect: Some(13.5),
        },
        "Classic ScoreV2 uses generic Mania raw windows with Hard Rock",
    );
}

#[test]
fn mania_hit_windows_without_speed_adjustment_ignore_custom_dt() {
    const NATIVE_MANIA_OD5: &str = r#"osu file format v14

[General]
Mode:3

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

    let engine = LiveLazerEngine;
    let mut map = engine.parse_str(NATIVE_MANIA_OD5).unwrap();
    engine
        .apply_mods(
            &mut map,
            &[StructuredMod {
                acronym: "DT".to_owned(),
                settings: BTreeMap::from([("speed_change".to_owned(), ModValue::Number(1.3))]),
            }],
        )
        .unwrap();

    assert_eq!(
        hit_windows_for_map_without_speed_adjustment(&map).unwrap(),
        HitWindows {
            miss: Some(173.5),
            meh: Some(136.5),
            ok: Some(112.5),
            good: Some(82.5),
            great: Some(49.5),
            perfect: Some(19.5),
        },
    );
}
