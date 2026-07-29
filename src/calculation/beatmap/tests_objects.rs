    #[test]
    fn raw_total_length_matches_slider_clamps_multiplier_and_hit_type_priority() {
        let slider = r#"osu file format v14
[General]
Mode:0
[Difficulty]
SliderMultiplier:5
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
[TimingPoints]
0,5,4,2,1,80,1,0
500,-50,4,2,1,80,0,0
[HitObjects]
64,192,1000.5,2,0,B|164:192,1,100
"#;
        assert_eq!(parse_integer_prefix("1e3", MAX_PARSE_VALUE), Some(1));
        assert_eq!(parse_integer_prefix("-0x10", MAX_PARSE_VALUE), Some(-16));
        assert_eq!(parse_float_prefix("1e3", MAX_PARSE_VALUE), Some(1000.0));
        assert_eq!(parse_float_prefix(".5", MAX_PARSE_VALUE), Some(0.5));
        assert_eq!(parse_float_prefix("-.5", MAX_PARSE_VALUE), Some(-0.5));
        assert_eq!(parse_integer_prefix("\u{FEFF}1", MAX_PARSE_VALUE), Some(1));
        assert_eq!(
            parse_float_prefix("\u{FEFF}1.5", MAX_PARSE_VALUE),
            Some(1.5)
        );
        assert_eq!(parse_integer_prefix("\u{0085}1", MAX_PARSE_VALUE), None);
        assert_eq!(parse_float_prefix("\u{0085}1.5", MAX_PARSE_VALUE), None);
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let short_beat = cache
            .load(
                BeatmapInput::Content {
                    content: slider,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("slider-short-beat", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            short_beat.times.full, 1004.0,
            "beatLength 5 clamps to 6, inherited SV applies, and the pinned partial decoder keeps SliderMultiplier at default 1"
        );

        let long_timing = slider
            .replace("0,5,4,2,1,80,1,0", "0,60001,4,2,1,80,1,0")
            .replace("500,-50,4,2,1,80,0,0\n", "");
        let long_beat = cache
            .load(
                BeatmapInput::Content {
                    content: &long_timing,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("slider-long-beat", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            long_beat.times.full, 61_000.0,
            "beatLength 60001 clamps to 60000 before slider duration"
        );

        let normal_priority = slider.replace(
            "64,192,1000.5,2,0,B|164:192,1,100",
            "64,192,1234.6,9,0,2000:0:0:0:",
        );
        let normal = cache
            .load(
                BeatmapInput::Content {
                    content: &normal_priority,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("normal-priority", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            normal.times.full, 2000.0,
            "valid Normal+Spinner extras dynamically expose endTime"
        );
        assert_eq!(
            normal.object_counts,
            ObjectCounts {
                circles: 1,
                sliders: 0,
                spinners: 1,
                holds: 0
            },
            "raw combined hit-type bits are counted independently even though object construction uses Normal priority",
        );
        let malformed_normal_spinner = normal_priority.replace("2000:0:0:0:", "2000");
        let malformed = cache
            .load(
                BeatmapInput::Content {
                    content: &malformed_normal_spinner,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("malformed-normal-spinner", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(malformed.object_counts, ObjectCounts::default());
        assert_eq!(malformed.times.full, 0.0);
        let normal_hold = normal_priority.replace(",9,0,2000:0:0:0:", ",129,0,2100:0:0:0:0:");
        let hold = cache
            .load(
                BeatmapInput::Content {
                    content: &normal_hold,
                    identity_path: None,
                },
                Ruleset::Mania,
                &mods("normal-hold", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            hold.object_counts,
            ObjectCounts {
                circles: 1,
                sliders: 0,
                spinners: 0,
                holds: 1
            }
        );
        assert_eq!(hold.times.full, 2100.0);
        let invalid_normal_slider = "[HitObjects]\n64,192,2000,3,0\n";
        assert_eq!(
            source_object_counts(invalid_normal_slider),
            ObjectCounts::default()
        );
        assert!(source_visible_lengths(invalid_normal_slider, None).is_none());

        for malformed_slider in [
            "[HitObjects]\n64,192,2000,2,0,B|bad,1,100\n",
            "[HitObjects]\n64,192,2000,2,0,B|164:192,1,100,0,bad\n",
            "[HitObjects]\n64,192,2000,2,0,B|164:192,1,100,0,0,bad\n",
        ] {
            assert_eq!(
                source_object_counts(malformed_slider),
                ObjectCounts::default()
            );
            assert!(source_visible_lengths(malformed_slider, None).is_none());
        }
        let explicit_empty_distance = "[HitObjects]\n64,192,2000,2,0,B|164:192,1,\n";
        assert_eq!(
            source_object_counts(explicit_empty_distance),
            ObjectCounts::default()
        );
        let one_coordinate_path = "[HitObjects]\n64,192,2000,2,0,B|164,1,100\n";
        assert_eq!(
            source_object_counts(one_coordinate_path),
            ObjectCounts {
                circles: 0,
                sliders: 1,
                spinners: 0,
                holds: 0
            },
        );
        let invalid_extra_coordinate = "[HitObjects]\n64,192,2000,2,0,B|164:192:bad,1,100\n";
        assert_eq!(
            source_object_counts(invalid_extra_coordinate),
            ObjectCounts::default()
        );
        let bare_marker_path = "[HitObjects]\n64,192,2000,2,0,B,1,100\n";
        assert_eq!(
            source_object_counts(bare_marker_path),
            ObjectCounts {
                circles: 0,
                sliders: 1,
                spinners: 0,
                holds: 0
            },
        );
        let unicode_marker_path = "[HitObjects]\n0,0,1000,2,0,B|100:0|é|200:0,1,100\n";
        assert_eq!(
            source_object_counts(unicode_marker_path),
            ObjectCounts {
                circles: 0,
                sliders: 1,
                spinners: 0,
                holds: 0
            },
            "JavaScript marker tests count UTF-16 code units, not UTF-8 bytes",
        );
        let later_empty_segment = "[HitObjects]\n64,192,2000,2,0,B|L,1,100\n";
        assert_eq!(
            source_object_counts(later_empty_segment),
            ObjectCounts::default()
        );
        let invalid_edge_volume = "[HitObjects]\n64,192,2000,2,0,B|164:192,1,100,0,0:0:0:bad\n";
        assert_eq!(
            source_object_counts(invalid_edge_volume),
            ObjectCounts::default()
        );
        let permissive_slider_fields =
            "[HitObjects]\n64,192,2000,2,0,B|164:192|B|264:192,1,100,bad,0:0|0:0|bad\n";
        assert_eq!(
            source_object_counts(permissive_slider_fields),
            ObjectCounts {
                circles: 0,
                sliders: 1,
                spinners: 0,
                holds: 0
            },
            "edge sounds use permissive global parseInt and node sets beyond the slider node count are ignored",
        );
        let excessive_repeats = "[HitObjects]\n64,192,2000,2,0,B|164:192,9001,100\n";
        assert_eq!(
            source_object_counts(excessive_repeats),
            ObjectCounts {
                circles: 0,
                sliders: 1,
                spinners: 0,
                holds: 0
            },
            "the pinned parser's pre-assignment repeat guard is inert",
        );
        let extreme_geometry = slider
            .replace("SliderMultiplier:5", "SliderMultiplier:0.001")
            .replace("0,5,4,2,1,80,1,0", "0,60000,4,2,1,80,1,0")
            .replace("500,-50,4,2,1,80,0,0\n", "")
            .replace(",1,100", ",2147483647,0");
        let geometry = source_last_slider_geometry(&extreme_geometry);
        assert!(geometry.is_some_and(|distance| distance > 0.0));
        let extreme_times = source_visible_lengths(&extreme_geometry, geometry).unwrap();
        assert!(extreme_times.total > f64::from(i32::MAX));
        let extreme_map = cache
            .load(
                BeatmapInput::Content {
                    content: &extreme_geometry,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("extreme-full-time", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert!(extreme_map.times.full > f64::from(i32::MAX));
        let explicit_one = slider.replace("SliderMultiplier:5", "SliderMultiplier:1");
        let explicit_two = slider.replace("SliderMultiplier:5", "SliderMultiplier:2");
        let missing_multiplier = slider.replace("SliderMultiplier:5\n", "");
        let visible_one = source_visible_lengths(&explicit_one, None).unwrap().total;
        assert_eq!(
            source_visible_lengths(&explicit_two, None).unwrap().total,
            visible_one
        );
        assert_eq!(
            source_visible_lengths(&missing_multiplier, None)
                .unwrap()
                .total,
            visible_one
        );
        let nan_duration = slider
            .replace("SliderMultiplier:5", "SliderMultiplier:0")
            .replace("B|164:192", "B")
            .replace(",1,100", ",1,0");
        let zero_geometry = source_last_slider_geometry(&nan_duration);
        assert_eq!(zero_geometry, Some(0.0));
        assert_eq!(
            source_visible_lengths(&nan_duration, zero_geometry)
                .unwrap()
                .total,
            1000.5,
            "JavaScript `NaN || start` falls back to the slider start while Infinity remains truthy",
        );
        let fractional_path = "[HitObjects]\n0,0,1000,2,0,L|100.9:0,1,0\n";
        assert_eq!(
            source_last_slider_geometry(fractional_path),
            Some(100.0),
            "pinned readPoint truncates every coordinate toward zero",
        );
        let modern_catmull =
            "osu file format v128\n[HitObjects]\n0,0,1000,2,0,C|100:0|100:0|200:100,1,0\n";
        assert_eq!(
            source_last_slider_geometry(&format!(
                "\nignored osu file format v1\n  {}foo  \n{}",
                modern_catmull.lines().next().unwrap(),
                modern_catmull
                    .lines()
                    .skip(1)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            source_last_slider_geometry(modern_catmull),
            "leading empty lines and header whitespace preserve the first format header",
        );
        assert_eq!(source_format_version("\njunk\nosu file format v128"), 14);
        assert_eq!(
            source_format_version("\nignored osu file format v1\n  osu file format v128foo  "),
            128,
        );
        assert_eq!(source_format_version("   \nosu file format v128"), 14);
        assert_eq!(
            source_format_version("  // comment\nosu file format v128"),
            14
        );
        for malformed_timed in [
            "[HitObjects]\n64,192,1000,8,0,2000,1\n",
            "[HitObjects]\n64,192,1000,128,0,2000:1\n",
        ] {
            assert_eq!(
                source_object_counts(malformed_timed),
                ObjectCounts::default()
            );
            assert!(source_visible_lengths(malformed_timed, None).is_none());
        }
        let empty_end_hold = "[HitObjects]\n64,192,1000,128,0,:1:2\n";
        assert_eq!(
            source_object_counts(empty_end_hold),
            ObjectCounts::default()
        );

        let combined_spinner_hold = "[HitObjects]\n64,192,1000,137,0,2000:0:0:0:,0:0\n";
        assert!(matches!(
            parse_raw_hit_object(combined_spinner_hold.lines().nth(1).unwrap())
                .map(|value| value.0),
            Some(RawLastObject::Spinner {
                start: 1000.0,
                end: 2000.0
            }),
        ));
        let combined_missing_spinner_end = "[HitObjects]\n64,192,1000,9,0\n";
        assert_eq!(
            source_object_counts(combined_missing_spinner_end),
            ObjectCounts::default()
        );
        let combined_bad_spinner_sample = "[HitObjects]\n64,192,1000,9,0,2000:0,1\n";
        assert_eq!(
            source_object_counts(combined_bad_spinner_sample),
            ObjectCounts::default()
        );
        let combined_bad_hold_suffix = "[HitObjects]\n64,192,1000,129,0,2000:1\n";
        assert_eq!(
            source_object_counts(combined_bad_hold_suffix),
            ObjectCounts::default()
        );
        let zero_spinner = normal_priority.replace(",9,0,2000:0:0:0:", ",8,0,0");
        let spinner = cache
            .load(
                BeatmapInput::Content {
                    content: &zero_spinner,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("zero-spinner", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            spinner.times.full, 1235.0,
            "a zero spinner end falls back to its decimal start time"
        );

        let zero_end_bpm = zero_spinner
            .replace(
                "0,5,4,2,1,80,1,0",
                "0,500,4,2,1,80,1,0\n500,250,4,2,1,80,1,0",
            )
            .replace("500,-50,4,2,1,80,0,0\n", "");
        let zero_end_bpm_map = cache
            .load(
                BeatmapInput::Content {
                    content: &zero_end_bpm,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("zero-spinner-bpm", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            zero_end_bpm_map.times.full, 1235.0,
            "totalLength uses endTime || startTime"
        );
        assert_eq!(
            zero_end_bpm_map.bpm.common, 120.0,
            "common BPM weighting uses explicit endTime 0 through nullish fallback"
        );

        let actual_distance = slider
            .replace("SliderMultiplier:5", "SliderMultiplier:1")
            .replace("0,5,4,2,1,80,1,0", "0,500,4,2,1,80,1,0")
            .replace("500,-50,4,2,1,80,0,0\n", "")
            .replace(",1,100", ",1,0");
        let actual_distance_map = cache
            .load(
                BeatmapInput::Content {
                    content: &actual_distance,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("slider-actual-distance", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            actual_distance_map.times.full, 1501.0,
            "zero expected distance uses the actual SliderPath geometry"
        );

        let missing_distance = actual_distance.replace(",1,0", ",1");
        let missing_distance_map = cache
            .load(
                BeatmapInput::Content {
                    content: &missing_distance,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("slider-missing-distance", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            missing_distance_map.times.full, 1501.0,
            "missing expected distance uses the actual SliderPath geometry"
        );

        let hold_without_extras =
            actual_distance.replace("64,192,1000.5,2,0,B|164:192,1,0", "64,192,1234.6,128,0");
        let hold = cache
            .load(
                BeatmapInput::Content {
                    content: &hold_without_extras,
                    identity_path: None,
                },
                Ruleset::Mania,
                &mods("hold-without-extras", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            hold.times.full, 1235.0,
            "missing hold extras fall back to the start time"
        );
    }

    #[test]
    fn native_mania_objects_expose_ordered_press_and_release_judgements() {
        let source = r#"osu file format v14
[General]
Mode:3
[Difficulty]
CircleSize:4
[HitObjects]
0,192,100,1,0,0:0:0:0:
128,192,200,128,0,400:0:0:0:0:
511,192,300,1,0,0:0:0:0:
"#;

        assert_eq!(
            source_mania_judgement_events(source).unwrap(),
            [
                ManiaJudgementEvent {
                    target: 100,
                    lane: 0,
                    object_index: 0,
                    kind: ManiaJudgementKind::Press,
                },
                ManiaJudgementEvent {
                    target: 200,
                    lane: 1,
                    object_index: 1,
                    kind: ManiaJudgementKind::Press,
                },
                ManiaJudgementEvent {
                    target: 400,
                    lane: 1,
                    object_index: 1,
                    kind: ManiaJudgementKind::Release,
                },
                ManiaJudgementEvent {
                    target: 300,
                    lane: 3,
                    object_index: 2,
                    kind: ManiaJudgementKind::Press,
                },
            ]
        );
    }
