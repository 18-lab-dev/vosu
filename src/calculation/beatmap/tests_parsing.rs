    #[test]
    fn extracts_metadata_objects_timings_and_engine_outputs() {
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let map = cache
            .load(
                BeatmapInput::Content {
                    content: OSU,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("hd", 1.5),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(map.metadata.title, "Title");
        assert_eq!(map.metadata.background_file, "bg.jpg");
        assert_eq!(
            map.object_counts,
            ObjectCounts {
                circles: 1,
                sliders: 1,
                spinners: 1,
                holds: 0
            }
        );
        assert_eq!(map.times.preview, 750);
        assert_eq!(map.times.first_object, 1000);
        assert_eq!(map.times.full, 2300.0);
        assert_eq!(map.engine.difficulty.max_combo, 7);
        assert_eq!(map.engine.maximum_score.maximum_score, 1_000_000);
        assert_eq!(map.engine.pp_by_accuracy.len(), 11);
        assert_eq!(map.clock_rate(), 1.5);
        assert_eq!(map.bpm.common, 300.0);
        assert_eq!(map.bpm.minimum, 150.0);
        assert_eq!(map.bpm.maximum, 300.0);
        assert_eq!(map.breaks.len(), 2);
        assert!(map.breaks.iter().any(|range| range.has_effect));
        assert!(map.breaks.iter().any(|range| !range.has_effect));
    }

    #[test]
    fn forwards_sanitized_structured_mods_to_the_injected_engine() {
        let mods = ModSet::new(
            "sanitized-checksum",
            vec![AppliedMod {
                acronym: "DT".into(),
                settings_json: Some("{\"speed_change\":1.25}".into()),
            }],
            1.25,
        )
        .unwrap();
        BeatmapCache::new(ContractEngine)
            .load(
                BeatmapInput::Content {
                    content: OSU,
                    identity_path: None,
                },
                Ruleset::Catch,
                &mods,
                ScoringMode::Lazer,
            )
            .unwrap();
    }

    #[test]
    fn cache_identity_covers_content_mode_mods_scoring_and_explicit_invalidation() {
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let base = mods("base", 1.0);
        let input = || BeatmapInput::Content {
            content: OSU,
            identity_path: Some(Path::new("virtual/map.osu")),
        };
        let first = cache
            .load(input(), Ruleset::Osu, &base, ScoringMode::Stable)
            .unwrap();
        let same = cache
            .load(input(), Ruleset::Osu, &base, ScoringMode::Stable)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        assert_eq!(cache.parse_count(), 1);
        cache
            .load(input(), Ruleset::Taiko, &base, ScoringMode::Stable)
            .unwrap();
        cache
            .load(
                input(),
                Ruleset::Taiko,
                &mods("changed", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        cache
            .load(
                input(),
                Ruleset::Taiko,
                &mods("changed", 1.0),
                ScoringMode::Lazer,
            )
            .unwrap();
        let changed_content = OSU.replace("Title:Title", "Title:Other");
        cache
            .load(
                BeatmapInput::Content {
                    content: &changed_content,
                    identity_path: Some(Path::new("virtual/map.osu")),
                },
                Ruleset::Taiko,
                &mods("changed", 1.0),
                ScoringMode::Lazer,
            )
            .unwrap();
        assert_eq!(cache.parse_count(), 5);
        cache.invalidate();
        cache
            .load(input(), Ruleset::Osu, &base, ScoringMode::Stable)
            .unwrap();
        assert_eq!(cache.parse_count(), 6);

        let rate_one = cache
            .load(input(), Ruleset::Osu, &base, ScoringMode::Stable)
            .unwrap();
        let rate_changed = cache
            .load(
                input(),
                Ruleset::Osu,
                &mods("base", 1.25),
                ScoringMode::Stable,
            )
            .unwrap();
        assert!(!Arc::ptr_eq(&rate_one, &rate_changed));
        assert_eq!(rate_changed.clock_rate(), 1.25);
    }

    #[test]
    fn identityless_content_reuses_only_exact_source_and_dimensions() {
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let base = mods("base", 1.0);
        fn input(content: &str) -> BeatmapInput<'_> {
            BeatmapInput::Content {
                content,
                identity_path: None,
            }
        }
        let first = cache
            .load(input(OSU), Ruleset::Osu, &base, ScoringMode::Stable)
            .unwrap();
        let same = cache
            .load(input(OSU), Ruleset::Osu, &base, ScoringMode::Stable)
            .unwrap();
        assert!(Arc::ptr_eq(&first, &same));
        assert_eq!(cache.parse_count(), 1);

        let changed_mode = cache
            .load(input(OSU), Ruleset::Taiko, &base, ScoringMode::Stable)
            .unwrap();
        assert!(!Arc::ptr_eq(&same, &changed_mode));
        let changed_content = OSU.replace("Title:Title", "Title:Other");
        let changed = cache
            .load(
                input(&changed_content),
                Ruleset::Taiko,
                &base,
                ScoringMode::Stable,
            )
            .unwrap();
        assert!(!Arc::ptr_eq(&changed_mode, &changed));
        assert_eq!(cache.parse_count(), 3);
    }

    #[test]
    fn only_osu_maps_are_converted_and_all_requested_modes_are_supported() {
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        for mode in [Ruleset::Osu, Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania] {
            let converted = cache
                .load(
                    BeatmapInput::Content {
                        content: OSU,
                        identity_path: None,
                    },
                    mode,
                    &mods(&format!("m{}", mode.id()), 1.0),
                    ScoringMode::Lazer,
                )
                .unwrap();
            assert_eq!(
                (converted.original_mode, converted.effective_mode),
                (Ruleset::Osu, mode)
            );
        }
        let native = cache
            .load(
                BeatmapInput::Content {
                    content: TAIKO,
                    identity_path: None,
                },
                Ruleset::Catch,
                &mods("native", 1.0),
                ScoringMode::Lazer,
            )
            .unwrap();
        assert_eq!(
            (native.original_mode, native.effective_mode),
            (Ruleset::Taiko, Ruleset::Taiko)
        );
    }

    #[test]
    fn exposed_integer_metadata_uses_javascript_parse_int_prefixes() {
        let content = OSU
            .replace("PreviewTime: 750", "PreviewTime: 1500.9tail")
            .replace("BeatmapID:12", "BeatmapID:123tail")
            .replace("BeatmapSetID:34", "BeatmapSetID:0x10tail");
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let map = cache
            .load(
                BeatmapInput::Content {
                    content: &content,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("numeric-prefixes", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(map.times.preview, 1500);
        assert_eq!(map.metadata.beatmap_id, 123);
        assert_eq!(map.metadata.beatmap_set_id, 16);

        let mode_prefix = content.replace("Mode: 0", "Mode:1tail");
        let mode_map = cache
            .load(
                BeatmapInput::Content {
                    content: &mode_prefix,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("mode-prefix", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            mode_map.original_mode,
            Ruleset::Osu,
            "strict engine mode remains unchanged"
        );
        assert_eq!(mode_map.effective_mode, Ruleset::Osu);
        assert_eq!(
            mode_map.reporting_mode, 1,
            "raw reporting mode follows JavaScript parseInt"
        );

        let duplicates = "\
[General]\n\
PreviewTime:123\n\
PreviewTime:oops\n\
Mode:2\n\
Mode:oops\n\
[Metadata]\n\
BeatmapID:456\n\
BeatmapID:oops\n\
BeatmapSetID:789\n\
BeatmapSetID:oops\n";
        assert_eq!(
            section_integer_value(duplicates, "General", "PreviewTime"),
            Some(123)
        );
        assert_eq!(
            section_integer_value(duplicates, "General", "Mode"),
            Some(2)
        );
        assert_eq!(
            section_integer_value(duplicates, "Metadata", "BeatmapID"),
            Some(456)
        );
        assert_eq!(
            section_integer_value(duplicates, "Metadata", "BeatmapSetID"),
            Some(789)
        );
    }

    #[test]
    fn bpm_grouping_rounds_beat_lengths_counts_from_zero_preserves_ties_and_scales() {
        let points = [
            BeatLengthPoint {
                start: 100.0,
                beat_len: 500.0004,
            },
            BeatLengthPoint {
                start: 600.0,
                beat_len: 500.00049,
            },
            BeatLengthPoint {
                start: 1000.0,
                beat_len: 400.0,
            },
        ];
        let summary = bpm_summary(&points, 2000.0, 1.25);
        assert_eq!(summary.common, 150.0);
        assert!((summary.minimum - (60_000.0 / 500.00049 * 1.25)).abs() < 1e-12);
        assert_eq!(summary.maximum, 187.5);
        assert_eq!((500.0005_f64 * 1000.0).round_ties_even() / 1000.0, 500.0);
        assert_eq!((500.0015_f64 * 1000.0).round_ties_even() / 1000.0, 500.002);
    }

    #[test]
    fn bpm_defaults_clamps_extrema_and_spinner_decimal_end_uses_parse_int() {
        assert_eq!(
            bpm_summary(&[], 0.0, 1.5),
            BpmSummary {
                common: 90.0,
                minimum: 90.0,
                maximum: 90.0
            }
        );
        let clamped = bpm_summary(
            &[
                BeatLengthPoint {
                    start: 0.0,
                    beat_len: 5.0,
                },
                BeatLengthPoint {
                    start: 10.0,
                    beat_len: 60_001.0,
                },
            ],
            20.0,
            1.0,
        );
        assert_eq!(clamped.minimum, 1.0);
        assert_eq!(clamped.maximum, 10_000.0);

        let content = r#"osu file format v14
[General]
Mode:0
[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
[TimingPoints]
0,500,4,2,1,80,1,0
1000,400,4,2,1,80,1,0
[HitObjects]
64,192,0,1,0,0:0:0:0:
64,192,1000,8,0,2000.4
"#;
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let map = cache
            .load(
                BeatmapInput::Content {
                    content,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("m", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(map.times.full, 2000.0);
        assert_eq!(
            map.bpm.common, 120.0,
            "spinner decimal end truncates to an exact duration tie and the first group wins"
        );
        assert_eq!(map.timing_points[0].beat_length, 500.0);
        assert_eq!(map.timing_points[1].beat_length, 400.0);

        let boundary_content = content
            .replace("0,500,4,2,1,80,1,0", "0,5,4,2,1,80,1,0")
            .replace("1000,400,4,2,1,80,1,0", "1000,60001,4,2,1,80,1,0");
        let boundary_map = cache
            .load(
                BeatmapInput::Content {
                    content: &boundary_content,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("boundaries", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(boundary_map.timing_points[0].beat_length, 6.0);
        assert_eq!(boundary_map.timing_points[1].beat_length, 60_000.0);
    }

    #[test]
    fn total_length_uses_last_start_sorted_object_and_unicode_falls_back() {
        let content = r#"osu file format v14
[General]
Mode:0
[Metadata]
Title:Fallback Title
Artist:Fallback Artist
[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
[TimingPoints]
0,500,4,2,1,80,1,0
[HitObjects]
64,192,1000,8,0,10000
64,192,2000,1,0,0:0:0:0:
"#;
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let map = cache
            .load(
                BeatmapInput::Content {
                    content,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("m", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(map.times.full, 2000.0);
        assert_eq!(map.metadata.title_unicode, "Fallback Title");
        assert_eq!(map.metadata.artist_unicode, "Fallback Artist");

        let short_spinner = content.replace(
            "64,192,1000,8,0,10000\n64,192,2000,1,0,0:0:0:0:",
            "64,192,1000,8,0,900",
        );
        let short_map = cache
            .load(
                BeatmapInput::Content {
                    content: &short_spinner,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("short-spinner", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            short_map.times.full, 900.0,
            "pinned spinner end is not clamped to its start"
        );

        let decimal_spinner = short_spinner.replace(",900", ",1500.9");
        let decimal_map = cache
            .load(
                BeatmapInput::Content {
                    content: &decimal_spinner,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("decimal-spinner", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            decimal_map.times.full, 1500.0,
            "pinned spinner endpoint uses parseInt truncation"
        );
    }

