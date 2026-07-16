
    #[test]
    fn metadata_defaults_explicit_empty_unicode_sentinels_and_background_event_match_source() {
        let omitted = r#"osu file format v14
[General]
Mode:0
[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
[HitObjects]
64,192,1000,1,0,0:0:0:0:
"#;
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let map = cache
            .load(
                BeatmapInput::Content {
                    content: omitted,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("a", 1.25),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(map.metadata.title, "Unknown Title");
        assert_eq!(map.metadata.title_unicode, "Unknown Title");
        assert_eq!(map.metadata.artist, "Unknown Artist");
        assert_eq!(map.metadata.artist_unicode, "Unknown Artist");
        assert_eq!(map.metadata.creator, "Unknown Creator");
        assert_eq!(map.metadata.difficulty_name, "Normal");
        assert_eq!(map.metadata.source, "");
        assert_eq!(map.metadata.tags, "");
        assert_eq!(
            (map.metadata.beatmap_id, map.metadata.beatmap_set_id),
            (0, 0)
        );
        assert_eq!(
            map.bpm,
            BpmSummary {
                common: 75.0,
                minimum: 75.0,
                maximum: 75.0
            }
        );

        let explicit = r#"osu file format v14
[General]
Mode:0
[Metadata] // trailing comment
Title:
TitleUnicode:
Artist:
ArtistUnicode:
Creator:
Version:
Tags:a  b	c
[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
[Events] // trailing comment
0,0,"actual,bg.jpg",0,0
 0,0,"ignored-leading-space.png",0,0
0,0
1,0,"odd-video.mp4"
Sprite,Background,Centre,"storyboard.png",320,240
[HitObjects]
64,192,1000,1,0,0:0:0:0:
"#;
        let metadata_stuck = "\
[Metadata]\n\
Title:Still Metadata\n\
[General] // inline comment is retained while Metadata is current\n\
PreviewTime:123\n\
[Events] // retained too\n\
0,0,\"not-a-background.jpg\"\n\
[TimingPoints] // retained too\n\
0,500,4,2,1,80,1,1\n\
[HitObjects] // retained too\n\
64,192,1000,1,0,0:0:0:0:\n";
        assert_eq!(
            section_integer_value(metadata_stuck, "General", "PreviewTime"),
            None
        );
        assert_eq!(background_from_events(metadata_stuck), "");
        assert!(source_timing_data(metadata_stuck).beat_lengths.is_empty());
        assert!(source_breaks(metadata_stuck).is_empty());
        assert_eq!(
            source_object_counts(metadata_stuck),
            ObjectCounts::default()
        );
        assert!(source_visible_lengths(metadata_stuck, None).is_none());
        assert!(source_last_slider_geometry(metadata_stuck).is_none());
        assert_eq!(
            section_value(
                "[Metadata]\u{FEFF}\nTitle:\u{FEFF}Name\u{FEFF}",
                "Metadata",
                "Title"
            ),
            Some("Name"),
        );
        assert_eq!(
            background_from_events("[Events]\u{FEFF}\n0,0,\"bom.png\""),
            "bom.png",
        );
        assert_eq!(
            background_from_events("[Events]\u{0085}\n0,0,\"nel.png\""),
            "",
        );
        let explicit_map = cache
            .load(
                BeatmapInput::Content {
                    content: explicit,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("b", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(explicit_map.metadata.title, "");
        assert_eq!(explicit_map.metadata.title_unicode, "");
        assert_eq!(explicit_map.metadata.artist, "");
        assert_eq!(explicit_map.metadata.artist_unicode, "");
        assert_eq!(explicit_map.metadata.creator, "");
        assert_eq!(explicit_map.metadata.difficulty_name, "");
        assert_eq!(explicit_map.metadata.tags, "a  b	c");
        assert_eq!(
            explicit_map.metadata.background_file, "actual",
            "osu-parsers splits event rows on every comma"
        );

        let sentinels = explicit.replace("Title:\nTitleUnicode:\nArtist:\nArtistUnicode:", "Title:Real\nTitleUnicode:Unknown Title\nArtist:Real Artist\nArtistUnicode:Unknown Artist");
        let reopened = "\
[General]\nMode:0\n\
[Metadata]\nTitle:First\n\
[General]\nMode:1\n\
[Events]\n0,0,\"first.png\"\n\
[Metadata]\nTitle:Second\n\
[Events]\n0,0,\"second.png\"\n\
[TimingPoints]\n0,500,4,2,1,80,1,0\n\
[Metadata]\nTitle:Third\n\
[TimingPoints]\n0,250,4,2,1,80,1,1\n\
[HitObjects]\n0,0,1000,1,0\n\
[Metadata]\nTitle:Fourth\n\
[HitObjects]\n0,0,2000,1,0\n";
        let disabled_metadata = "\
[Metadata]\nTitle:First\n\
[Metadata]\nTitle:Ignored\n\
[Events] // retained because the disabled current section is still Metadata\n\
0,0,\"not-background.png\"\n";
        assert_eq!(background_from_events(disabled_metadata), "");
        assert_eq!(section_integer_value(reopened, "General", "Mode"), Some(0));
        assert_eq!(background_from_events(reopened), "first.png");
        assert_eq!(source_timing_data(reopened).beat_lengths[0].beat_len, 500.0);
        assert_eq!(
            source_object_counts(reopened),
            ObjectCounts {
                circles: 1,
                sliders: 0,
                spinners: 0,
                holds: 0
            },
        );
        let sentinel_map = cache
            .load(
                BeatmapInput::Content {
                    content: &sentinels,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("c", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(sentinel_map.metadata.title_unicode, "Real");
        assert_eq!(sentinel_map.metadata.artist_unicode, "Real Artist");
        assert_eq!(
            metadata_value("[Metadata]\nTitle:First\nTitle:Last", "Title"),
            Some("Last")
        );
        assert!(event_is_background("Background   "));
        assert!(event_is_background("0foo"));
        assert!(!event_is_background(" 0"));
        assert!(!event_is_background("_0"));
    }

    #[test]
    fn missing_or_empty_hit_objects_are_valid_and_preserve_audio_and_bpm_fallback() {
        let missing = r#"osu file format v14
[General]
AudioFilename: folder\audio:mix.mp3//ignored
Mode:0
[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
[TimingPoints]
0,500,4,2,1,80,1,0
1000,400,4,2,1,80,1,0
3000,400,4,2,1,80,1,0
"#;
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let missing_map = cache
            .load(
                BeatmapInput::Content {
                    content: missing,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("missing", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(missing_map.metadata.audio_file, r"folder\audio:mix.mp3");
        assert_eq!(
            general_value("[General]\nAudioFilename:foo//bar", "AudioFilename"),
            Some("foo")
        );
        assert_eq!(missing_map.object_counts, ObjectCounts::default());
        assert_eq!(
            missing_map.times,
            BeatmapTimes {
                preview: -1,
                first_object: 0,
                full: 0.0
            }
        );
        assert_eq!(
            missing_map.bpm,
            BpmSummary {
                common: 150.0,
                minimum: 120.0,
                maximum: 150.0
            }
        );

        let empty = format!("{missing}[HitObjects]\n");
        let empty_map = cache
            .load(
                BeatmapInput::Content {
                    content: &empty,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("empty", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(empty_map.object_counts, ObjectCounts::default());
        assert_eq!(empty_map.bpm, missing_map.bpm);
    }

    #[test]
    fn event_boundaries_overlaps_and_open_kiai_match_source() {
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let map = cache
            .load(
                BeatmapInput::Content {
                    content: OSU,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("m", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            map.event_status(1000.0, 1.5),
            EventStatus {
                bpm: 225.0,
                kiai: true,
                break_time: true
            }
        );
        assert!(map.event_status(1500.0, 1.0).break_time);
        assert!(map.event_status(2100.0, 1.0).break_time);
        assert!(!map.event_status(2100.001, 1.0).break_time);
        assert!(map.event_status(1600.0, 1.0).kiai);
        assert!(!map.event_status(1600.001, 1.0).kiai);
        assert!(
            !map.event_status(4000.0, 1.0).kiai,
            "source open kiai end remains -1 and never matches"
        );
    }

    #[test]
    fn fractional_and_reversed_breaks_match_source() {
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let fractional = OSU.replace("2,900,2100", "2,100.5tail,800.5tail\n2,900,800");
        let fractional_map = cache
            .load(
                BeatmapInput::Content {
                    content: &fractional,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("fractional-breaks", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert!(fractional_map.event_status(100.5, 1.0).break_time);
        assert!(fractional_map.event_status(800.5, 1.0).break_time);
        assert!(!fractional_map.event_status(800.501, 1.0).break_time);
        assert_eq!(
            fractional_map.breaks[0],
            BreakRange {
                has_effect: true,
                start: 100.5,
                end: 800.5
            }
        );
        assert_eq!(
            fractional_map.breaks[1],
            BreakRange {
                has_effect: false,
                start: 900.0,
                end: 900.0
            }
        );
    }

    #[test]
    fn equal_time_control_points_keep_first_point_of_each_type() {
        let content = OSU.replace(
            "0,500,4,2,1,80,1,0\n1000,400,4,2,1,80,1,1\n1600,300,4,2,1,80,1,0\n3000,600,4,2,1,80,1,1",
            "0,500,4,2,1,80,1,0\n0,250,4,2,1,80,1,1",
        );
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let map = cache
            .load(
                BeatmapInput::Content {
                    content: &content,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("equal-timing", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            map.timing_points
                .iter()
                .filter(|point| point.start == 0.0)
                .count(),
            1
        );
        assert_eq!(map.timing_points[0].beat_length, 500.0);
        let separated_duplicate = source_timing_data(
            "[TimingPoints]\n0,500,4,2,1,80,1,0\n100,400,4,2,1,80,1,0\n0,-50,4,2,1,80,0,1\n",
        );
        assert_eq!(separated_duplicate.beat_lengths[0].beat_len, 500.0);
        assert_eq!(separated_duplicate.effects[0], (0.0, false));
        assert_eq!(
            source_slider_control_points(
                "[TimingPoints]\n0,500,4,2,1,80,1,0\n100,400,4,2,1,80,1,0\n0,-50,4,2,1,80,0,1\n",
                0.0,
            ),
            (500.0, 1.0),
        );
        assert_eq!(map.bpm.common, 120.0);
        assert_eq!(map.bpm.maximum, 120.0);
        assert!(!map.event_status(0.0, 1.0).kiai);
        let inherited = source_timing_data(
            "[TimingPoints]\n0,500,4,2,1,80,1,0\n0,-100,4,2,1,80,0,0\n0,-50,4,2,1,80,0,1\n",
        );
        assert_eq!(
            inherited.effects,
            vec![(0.0, true)],
            "last inherited effect point wins at an equal timestamp"
        );
        let signed_zero =
            source_timing_data("[TimingPoints]\n0,500,4,2,1,80,1,0\n-0,600,4,2,1,80,1,1\n");
        assert_eq!(signed_zero.beat_lengths[0].beat_len, 500.0);
        assert_eq!(signed_zero.effects, vec![(0.0, false)]);
    }

    #[test]
    fn raw_prefix_tokens_drive_reported_objects_timing_kiai_and_breaks() {
        let content = OSU
            .replace("64,192,1000,1,0", "64,192,1000tail,1,0")
            .replace("1000,400,4,2,1,80,1,1", "1000tail,400tail,4,2,1,80,1,1")
            .replace("1600,300,4,2,1,80,1,0", "1600tail,300tail,4,2,1,80,1,0")
            .replace("2,900,2100", "2tail,900tail,2100tail");
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let map = cache
            .load(
                BeatmapInput::Content {
                    content: &content,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("raw-prefix-projections", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(map.times.first_object, 1000);
        assert!(
            map.timing_points
                .iter()
                .any(|point| point.start == 1000.0 && point.beat_length == 400.0)
        );
        assert_eq!(map.event_status(1000.0, 1.0).bpm, 150.0);
        assert!(map.event_status(1000.0, 1.0).kiai);
        assert!(map.event_status(1600.0, 1.0).kiai);
        assert!(!map.event_status(1600.001, 1.0).kiai);
        assert!(map.event_status(900.0, 1.0).break_time);
        assert!(map.event_status(2100.0, 1.0).break_time);
        assert!(!map.event_status(2100.001, 1.0).break_time);
    }

    #[test]
    fn first_object_uses_javascript_round_for_negative_half() {
        let content = r#"osu file format v14
[General]
Mode:0
[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
[HitObjects]
64,192,-.5,1,0,0:0:0:0:
"#;
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let map = cache
            .load(
                BeatmapInput::Content {
                    content,
                    identity_path: None,
                },
                Ruleset::Osu,
                &mods("negative-half", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(map.times.first_object, 0);
        assert_eq!(map.times.full, 0.0);
    }

