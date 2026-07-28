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

fn backend_request(
    backend: &Arc<LiveStateCalculationBackend>,
    root: &Path,
    beatmap_path: PathBuf,
) -> StateCalculationRequest {
    let snapshot =
        crate::state::StateBundle::new(ClientType::Stable, 1, backend.clone()).snapshot();
    let mods = crate::calculation::default_calculated_mods();
    StateCalculationRequest {
        input: crate::state::CalculationInput {
            game_folder: root.to_owned(),
            songs_folder: root.to_owned(),
            menu_folder: Some("set".to_owned()),
            filename: Some("map.osu".to_owned()),
            beatmap_path,
            checksum: "memory-identity".to_owned(),
            mode: 0,
            mods: mods.clone(),
            client_scoring: ClientType::Stable,
        },
        key: crate::state::StateCalculationKey {
            instance_generation: 1,
            reset_epoch: 0,
            checksum: "memory-identity".to_owned(),
            mode: 0,
            mods_checksum: mods.checksum.clone(),
        },
        phase: CalculationPhase::Menu,
        entry: Arc::clone(&snapshot),
        current: snapshot,
        mods,
        calculate_pp: true,
        play_time: 500.0,
        calculation_reset_generation: 0,
        gameplay: None,
        result: None,
    }
}

fn backend_test_paths(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "vosu-state-backend-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time is after the Unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("create backend test directory");
    let first = root.join("map.osu");
    let second = root.join("other.osu");
    std::fs::write(&first, OSU).expect("write first backend test beatmap");
    std::fs::write(&second, OSU).expect("write second backend test beatmap");
    (root, first, second)
}

fn preparation_counts(backend: &LiveStateCalculationBackend) -> (u64, u64, u64) {
    let state = backend.state.lock().expect("backend state lock");
    (
        state.mods_preparation_count,
        state.map_preparation_count,
        state.cache.parse_count(),
    )
}

#[test]
fn steady_requests_reuse_prepared_mods_map_and_live_metadata() {
    let (root, first, second) = backend_test_paths("steady");
    let backend = Arc::new(LiveStateCalculationBackend::new());
    let request = backend_request(&backend, &root, first);

    let initial =
        LiveStateCalculationBackend::calculate_sync(&backend.state, request.clone()).unwrap();
    let repeated =
        LiveStateCalculationBackend::calculate_sync(&backend.state, request.clone()).unwrap();
    assert_eq!(preparation_counts(&backend), (1, 1, 1));
    let mut redundant_mods_field_changed = request.clone();
    redundant_mods_field_changed.mods.rate = 2.0;
    LiveStateCalculationBackend::calculate_sync(&backend.state, redundant_mods_field_changed)
        .unwrap();
    assert_eq!(preparation_counts(&backend), (1, 1, 1));
    {
        let state = backend.state.lock().expect("backend state lock");
        let prepared = state.prepared_mods.as_ref().expect("prepared mods");
        let mut settings_changed = request.input.mods.clone();
        settings_changed.array.push(Mod {
            acronym: "HD".to_owned(),
            settings: Some(indexmap::IndexMap::from([(
                "setting".to_owned(),
                serde_json::json!(1),
            )])),
        });
        assert!(!prepared.matches(&settings_changed, ClientType::Stable));
        let mut rate_changed = request.input.mods.clone();
        rate_changed.rate = 1.25;
        assert!(!prepared.matches(&rate_changed, ClientType::Stable));
        assert!(!prepared.matches(&request.input.mods, ClientType::Lazer));
    }

    let initial = initial.beatmap.expect("initial beatmap update");
    let repeated = repeated.beatmap.expect("repeated beatmap update");
    assert_eq!(initial.original_mode, repeated.original_mode);
    assert_eq!(initial.background_filename, repeated.background_filename);
    assert_eq!(initial.first_object_time, repeated.first_object_time);
    assert_eq!(initial.full_pp, repeated.full_pp);
    assert_eq!(initial.source_key, repeated.source_key);
    assert_eq!(initial.pp_acc, repeated.pp_acc);
    assert_eq!(initial.event_status, repeated.event_status);
    assert_eq!(initial.graph, repeated.graph);
    assert!(Arc::ptr_eq(
        initial.parsed.as_ref().expect("initial parsed map"),
        repeated.parsed.as_ref().expect("repeated parsed map")
    ));

    let mut changed = request.clone();
    changed.input.checksum = "other-identity".to_owned();
    LiveStateCalculationBackend::calculate_sync(&backend.state, changed).unwrap();
    assert_eq!(preparation_counts(&backend), (1, 2, 2));
    let live_before_rate = {
        let state = backend.state.lock().expect("backend state lock");
        Arc::clone(state.live.as_ref().expect("prepared live beatmap"))
    };

    let mut changed = request.clone();
    changed.input.mods.rate = 1.25;
    LiveStateCalculationBackend::calculate_sync(&backend.state, changed).unwrap();
    assert_eq!(preparation_counts(&backend), (2, 3, 3));
    {
        let state = backend.state.lock().expect("backend state lock");
        assert!(!Arc::ptr_eq(
            &live_before_rate,
            state.live.as_ref().expect("rate-adjusted live beatmap")
        ));
    }

    let mut changed = request.clone();
    changed.input.mods.array.push(Mod {
        acronym: "HD".to_owned(),
        settings: None,
    });
    LiveStateCalculationBackend::calculate_sync(&backend.state, changed).unwrap();
    assert_eq!(preparation_counts(&backend), (3, 4, 4));

    let mut changed = request.clone();
    changed.input.mode = 1;
    LiveStateCalculationBackend::calculate_sync(&backend.state, changed).unwrap();
    assert_eq!(preparation_counts(&backend), (4, 5, 5));

    let mut changed = request.clone();
    changed.input.client_scoring = ClientType::Lazer;
    LiveStateCalculationBackend::calculate_sync(&backend.state, changed).unwrap();
    assert_eq!(preparation_counts(&backend), (5, 6, 6));

    let mut changed = request;
    changed.input.beatmap_path = second;
    LiveStateCalculationBackend::calculate_sync(&backend.state, changed).unwrap();
    assert_eq!(preparation_counts(&backend), (6, 7, 7));
    std::fs::remove_dir_all(root).expect("remove backend test directory");
}

#[test]
fn reset_and_errors_do_not_leave_stale_progression_or_replace_valid_preparation() {
    let (root, first, _) = backend_test_paths("reset-errors");
    let backend = Arc::new(LiveStateCalculationBackend::new());
    let request = backend_request(&backend, &root, first);
    LiveStateCalculationBackend::calculate_sync(&backend.state, request.clone()).unwrap();

    let mut reset = request.clone();
    reset.calculation_reset_generation = 1;
    LiveStateCalculationBackend::calculate_sync(&backend.state, reset.clone()).unwrap();
    assert_eq!(preparation_counts(&backend), (1, 2, 1));

    let mut invalid_rate = reset.clone();
    invalid_rate.input.mods.rate = 0.0;
    assert!(LiveStateCalculationBackend::calculate_sync(&backend.state, invalid_rate).is_err());
    assert_eq!(preparation_counts(&backend), (1, 2, 1));

    let mut missing = reset.clone();
    missing.input.beatmap_path = root.join("missing.osu");
    missing.input.checksum = "missing".to_owned();
    missing.input.mods.array.push(Mod {
        acronym: "HD".to_owned(),
        settings: None,
    });
    assert!(LiveStateCalculationBackend::calculate_sync(&backend.state, missing).is_err());
    assert_eq!(preparation_counts(&backend), (1, 2, 1));

    LiveStateCalculationBackend::calculate_sync(&backend.state, reset).unwrap();
    assert_eq!(preparation_counts(&backend), (1, 2, 1));
    std::fs::remove_dir_all(root).expect("remove backend test directory");
}
