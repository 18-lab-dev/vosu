use super::*;

const OSU: &str = r#"osu file format v14

[General]
Mode:0

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
SliderMultiplier:1.4
SliderTickRate:1

[TimingPoints]
0,500,4,2,1,80,1,0

[HitObjects]
64,64,1000,1,0,0:0:0:0:
448,64,1200,1,0,0:0:0:0:
64,320,1400,1,0,0:0:0:0:
"#;

const MANIA: &str = r#"osu file format v14

[General]
Mode:3

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9

[TimingPoints]
0,500,4,2,1,80,1,0

[HitObjects]
64,192,1000,1,0,0:0:0:0:
192,192,1200,128,0,1200:0:0:0:0:
"#;

#[test]
fn stable_readiness_distinguishes_missing_filename_from_missing_folder() {
    let game = Path::new("/game");
    let map = Path::new("/game/Songs/set/map.osu");

    assert_eq!(
        beatmap_readiness(
            Path::new(""),
            map,
            Some("set"),
            Some("map.osu"),
            ClientType::Stable
        ),
        BeatmapReadiness::RetainEventAndRetry
    );
    assert_eq!(
        beatmap_readiness(game, Path::new(""), None, None, ClientType::Stable),
        BeatmapReadiness::RetainEventAndRetry
    );
    assert_eq!(
        beatmap_readiness(
            game,
            Path::new(""),
            None,
            Some("map.osu"),
            ClientType::Stable
        ),
        BeatmapReadiness::Retry
    );
    assert_eq!(
        beatmap_readiness(game, map, Some(""), Some("map.osu"), ClientType::Stable),
        BeatmapReadiness::Retry
    );
    assert_eq!(
        beatmap_readiness(
            game,
            Path::new(""),
            None,
            Some("map.osz"),
            ClientType::Stable
        ),
        BeatmapReadiness::RetainEventAndRetry
    );
    assert_eq!(
        beatmap_readiness(game, map, Some("set"), Some("map.OSU"), ClientType::Stable),
        BeatmapReadiness::RetainEventAndRetry
    );
    assert_eq!(
        beatmap_readiness(
            game,
            Path::new("/game/Songs/theme.osu"),
            Some("."),
            Some("theme.osu"),
            ClientType::Stable
        ),
        BeatmapReadiness::RetainTheme
    );
    assert_eq!(
        beatmap_readiness(game, map, Some("set"), Some("map.osu"), ClientType::Stable),
        BeatmapReadiness::Load
    );
}

#[test]
fn lazer_bypasses_stable_filename_and_theme_gates_but_not_missing_metadata() {
    let game = Path::new("/game");
    let map = Path::new("/game/Songs/set/map.osz");

    assert_eq!(
        beatmap_readiness(
            Path::new(""),
            map,
            Some("set"),
            Some("map.osz"),
            ClientType::Lazer
        ),
        BeatmapReadiness::Retry
    );
    assert_eq!(
        beatmap_readiness(game, Path::new(""), Some("set"), None, ClientType::Lazer),
        BeatmapReadiness::RetainTheme
    );
    assert_eq!(
        beatmap_readiness(
            game,
            Path::new(""),
            None,
            Some("map.osz"),
            ClientType::Lazer
        ),
        BeatmapReadiness::Retry
    );
    assert_eq!(
        beatmap_readiness(game, map, Some(""), Some("map.osz"), ClientType::Lazer),
        BeatmapReadiness::Retry
    );
    assert_eq!(
        beatmap_readiness(game, map, Some("set"), Some(""), ClientType::Lazer),
        BeatmapReadiness::RetainTheme
    );
    assert_eq!(
        beatmap_readiness(
            game,
            Path::new("/game/Songs/theme.osu"),
            Some("."),
            Some("theme.osu"),
            ClientType::Lazer
        ),
        BeatmapReadiness::Load
    );
    assert_eq!(
        beatmap_readiness(game, map, Some("set"), Some("map.osz"), ClientType::Lazer),
        BeatmapReadiness::Load
    );
}

#[test]
fn parsed_background_overrides_memory_only_for_stable() {
    assert_eq!(
        parsed_background_filename(ClientType::Stable, r#" images\bg.jpg "#),
        Some(safe_join([r#" images\bg.jpg "#]))
    );
    assert_eq!(
        parsed_background_filename(ClientType::Stable, ""),
        Some(".".to_owned())
    );
    assert_eq!(
        parsed_background_filename(ClientType::Lazer, "events-bg.jpg"),
        None
    );
}

#[test]
fn live_beatmap_engine_populates_full_projection() {
    let output = LiveBeatmapEngine
        .calculate(EngineRequest {
            content: OSU,
            original_mode: BeatmapRuleset::Osu,
            effective_mode: BeatmapRuleset::Osu,
            mods: &[],
            clock_rate: 1.0,
            scoring_mode: ScoringMode::Lazer,
        })
        .unwrap();
    assert!(output.difficulty.stars > 0.0);
    assert_eq!(output.difficulty.max_combo, 3);
    assert_eq!(output.maximum_score.max_combo, 3);
    assert_eq!(output.pp_by_accuracy.len(), 11);
    assert!(output.pp_by_accuracy[&100] >= output.pp_by_accuracy[&90]);
    assert!(!output.strains.is_empty());
    assert!(output.difficulty.hit_window.unwrap() > 0.0);
    assert_eq!(output.strains.len(), 10);
    for name in [
        "aim",
        "aimNoSliders",
        "flashlight",
        "speed",
        "reading",
        "color",
        "rhythm",
        "stamina",
        "movement",
        "strains",
    ] {
        assert!(output.strains.contains_key(name), "missing strain `{name}`");
    }
}

#[test]
fn mania_api_combo_uses_scoring_maximum_instead_of_difficulty_combo() {
    let output = LiveBeatmapEngine
        .calculate(EngineRequest {
            content: MANIA,
            original_mode: BeatmapRuleset::Mania,
            effective_mode: BeatmapRuleset::Mania,
            mods: &[],
            clock_rate: 1.0,
            scoring_mode: ScoringMode::Lazer,
        })
        .unwrap();

    assert!(
        output.maximum_score.max_combo > output.difficulty.max_combo,
        "lazer scoring counts hold-note judgements beyond difficulty combo"
    );
}

#[test]
fn source_number_scores_use_napi_long_conversion_at_calculator_boundary() {
    assert_eq!(calculator_long(42.9), 42);
    assert_eq!(calculator_long(-42.9), -42);
    // The source has already normalized this odd integer through a
    // JavaScript Number before it reaches the calculator boundary.
    let normalized_above_js_safe_integer = 9_007_199_254_740_993_u64 as f64;
    assert_eq!(
        calculator_long(normalized_above_js_safe_integer),
        9_007_199_254_740_992
    );
    assert_eq!(calculator_long(f64::NAN), 0);
    assert_eq!(calculator_long(f64::INFINITY), i64::MAX);
    assert_eq!(calculator_long(f64::NEG_INFINITY), i64::MIN);
    assert_eq!(calculator_long(i64::MAX as f64), i64::MAX);
    assert_eq!(calculator_long(i64::MIN as f64), i64::MIN);
}

#[test]
fn calculation_mods_follow_source_sanitation_and_stable_classic_insertion() {
    let input = ["RX", "AP", "v2", "HD"]
        .into_iter()
        .map(|acronym| Mod {
            acronym: acronym.to_owned(),
            settings: None,
        })
        .collect::<Vec<_>>();

    let stable = mods_for_calculation(&input, false)
        .into_iter()
        .map(|value| value.acronym)
        .collect::<Vec<_>>();
    assert_eq!(stable, ["SV2", "HD", "CL"]);

    let lazer = mods_for_calculation(&input, true)
        .into_iter()
        .map(|value| value.acronym)
        .collect::<Vec<_>>();
    assert_eq!(lazer, ["SV2", "HD"]);
}

#[test]
fn native_ruleset_is_used_for_progressive_calculation() {
    assert_eq!(progressive_ruleset(BeatmapRuleset::Taiko), Ruleset::Taiko);
    assert_eq!(progressive_ruleset(BeatmapRuleset::Catch), Ruleset::Catch);
    assert_eq!(progressive_ruleset(BeatmapRuleset::Mania), Ruleset::Mania);
}

#[test]
fn concrete_progressive_adapter_preserves_accuracy_and_fc_score_fields() {
    let map = LiveProgressiveBeatmap::new(OSU.as_bytes(), Ruleset::Osu, &[]).unwrap();
    let key = GameplayCacheKey {
        beatmap_checksum: "map".into(),
        mode: 0,
        mods_checksum: "mods".into(),
        mp3_length: 2_000.0,
    };
    let mut gameplay = GameplayCalculator::default();
    let outcome = gameplay.update(
        Some(&map),
        GameplayFrame {
            gate: GameplayGate::Active,
            reset_fc_pp: 0.0,
            key: &key,
            play_time: 1_500.0,
            mode: Ruleset::Osu,
            client: ClientScoring::Lazer,
            score: 900_000,
            combo: 2,
            max_combo: 2,
            statistics: HitStatistics {
                great: 2,
                miss: 1,
                ..HitStatistics::default()
            },
        },
    );
    assert_eq!(outcome, CalculationOutcome::Updated);
    assert!(gameplay.attributes().pp.is_finite());
    assert!(gameplay.attributes().fc_pp >= gameplay.attributes().pp);

    let mut result = ResultCalculator::default();
    let outcome = result.update(
        Some(&map),
        &ResultFrame {
            beatmap_checksum: "map",
            mods_checksum: "mods",
            mode: Ruleset::Osu,
            player_name: "player",
            tournament_rank: None,
            client: ClientScoring::Lazer,
            score: 900_000,
            max_combo: 2,
            map_max_combo: 3,
            displayed_accuracy_percent: 66.6666666667,
            statistics: HitStatistics {
                great: 2,
                miss: 1,
                ..HitStatistics::default()
            },
        },
    );
    assert_eq!(outcome, CalculationOutcome::Updated);
    assert!(result.performance().pp.is_finite());
    assert!(result.performance().fc_pp >= result.performance().pp);
}
