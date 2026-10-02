    #[test]
    fn graph_uses_source_padding_and_boundaries() {
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
        let graph = map.build_graph(Some(2200.0), 1.0);
        assert_eq!(map.times.last_visible_object, 1400.0);
        assert_eq!(graph.primary, vec![0.0, 0.0, 1.0, 2.0]);
        assert_eq!(graph.series[0].data, vec![-100.0, -100.0, 1.0, 2.0]);
        assert_eq!(graph.x_axis, vec![0.0, 400.0, 1000.0, 1400.0]);
        let no_audio_length = map.build_graph(None, 1.0);
        assert_eq!(no_audio_length.primary, vec![0.0, 0.0, 1.0, 2.0]);
        assert_eq!(
            no_audio_length.series[0].data,
            vec![-100.0, -100.0, 1.0, 2.0]
        );
        assert_eq!(no_audio_length.x_axis, vec![0.0, 400.0, 1000.0, 1400.0]);
        // The trailing spinner spans 1900..2300, while the last visible
        // object starts at 1400. Its graph gap is distinct from audio padding.
        let extended = map.build_graph(Some(2700.0), 1.0);
        assert_eq!(extended.series[0].data,
            vec![-100.0, -100.0, 1.0, 2.0, -50.0, -50.0, -50.0, -100.0]);
        assert_eq!(extended.x_axis,
            vec![0.0, 400.0, 1000.0, 1400.0, 1400.0, 1800.0, 2200.0, 2600.0]);
        let leading_spinner = OSU.replace(
            "64,192,1000,1,0,0:0:0:0:", "64,192,1000,8,0,1300");
        let leading = cache.load(
            BeatmapInput::Content { content: &leading_spinner, identity_path: None },
            Ruleset::Osu, &mods("leading-spinner", 1.0), ScoringMode::Stable,
        ).unwrap().build_graph(Some(2700.0), 1.0);
        assert_eq!(leading.series[0].data,
            vec![-100.0, -100.0, -50.0, 1.0, 2.0, -50.0, -50.0, -50.0, -100.0]);
        assert_eq!(leading.x_axis,
            vec![0.0, 400.0, 800.0, 1400.0, 1800.0, 1400.0, 1800.0, 2200.0, 2600.0]);
        let empty = map.build_graph(Some(2200.0), 0.0);
        assert_eq!(
            empty
                .series
                .iter()
                .map(|series| series.name.as_str())
                .collect::<Vec<_>>(),
            strain_names(Ruleset::Osu)
        );
        assert!(empty.series.iter().all(|series| series.data.is_empty()));
        let negative_content = TAIKO.replace("64,192,1000", "64,192,-100");
        let mut negative_cache = BeatmapCache::new(SingleStrainEngine);
        let negative_map = negative_cache
            .load(
                BeatmapInput::Content {
                    content: &negative_content,
                    identity_path: None,
                },
                Ruleset::Taiko,
                &mods("negative-left-offset", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        let negative_graph = negative_map.build_graph(None, 1.0);
        assert!(negative_graph.series[0].data.is_empty());
        assert!(negative_graph.x_axis.is_empty());
    }

    #[test]
    fn empty_strains_still_pad_all_named_graph_series_when_audio_is_known() {
        let content = OSU.split("[HitObjects]").next().unwrap();
        for mode in [Ruleset::Taiko, Ruleset::Catch, Ruleset::Mania] {
            let mut cache = BeatmapCache::new(EmptyStrainEngine);
            let map = cache
                .load(
                    BeatmapInput::Content {
                        content,
                        identity_path: None,
                    },
                    mode,
                    &mods(&format!("empty-{}", mode.id()), 1.0),
                    ScoringMode::Stable,
                )
                .unwrap();
            let padded = map.build_graph(Some(1000.0), 1.0);
            assert_eq!(padded.primary, vec![0.0; 3]);
            assert_eq!(padded.x_axis, vec![0.0, 400.0, 800.0]);
            assert_eq!(padded.series.len(), strain_names(mode).len());
            assert!(
                padded
                    .series
                    .iter()
                    .all(|series| series.data == vec![-100.0; 3])
            );
            let unpadded = map.build_graph(None, 1.0);
            assert!(unpadded.primary.is_empty());
            assert!(unpadded.x_axis.is_empty());
            assert_eq!(unpadded.series.len(), strain_names(mode).len());
            assert!(unpadded.series.iter().all(|series| series.data.is_empty()));
        }
    }

    #[test]
    fn path_input_uses_canonical_identity_and_file_content_checksum() {
        let path = std::env::temp_dir().join(format!("vosu-beatmap-{}.osu", std::process::id()));
        fs::write(&path, OSU).unwrap();
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let first = cache
            .load(
                BeatmapInput::Path(&path),
                Ruleset::Osu,
                &mods("m", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(
            first.cache_key().canonical_path.as_deref(),
            Some(fs::canonicalize(&path).unwrap().as_path())
        );
        fs::write(&path, OSU.replace("Title:Title", "Title:Changed")).unwrap();
        let changed = cache
            .load(
                BeatmapInput::Path(&path),
                Ruleset::Osu,
                &mods("m", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_ne!(
            first.cache_key().source_checksum,
            changed.cache_key().source_checksum
        );
        assert_eq!(changed.metadata.title, "Changed");
        let mut invalid_utf8 = OSU.replace("Title:Title", "Title:Bad").into_bytes();
        let bad = invalid_utf8
            .windows(3)
            .position(|window| window == b"Bad")
            .unwrap();
        invalid_utf8[bad + 1] = 0xff;
        fs::write(&path, invalid_utf8).unwrap();
        let replaced = cache
            .load(
                BeatmapInput::Path(&path),
                Ruleset::Osu,
                &mods("m", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert_eq!(replaced.metadata.title, "B\u{fffd}d");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn caller_identity_skips_all_path_io_until_generation_changes() {
        let path =
            std::env::temp_dir().join(format!("vosu-beatmap-identity-{}.osu", std::process::id()));
        fs::write(&path, OSU).unwrap();
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        let first = cache
            .load(
                BeatmapInput::PathWithIdentity {
                    path: &path,
                    memory_identity: "checksum-a",
                },
                Ruleset::Osu,
                &mods("m", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        fs::remove_file(&path).unwrap();
        let cached = cache
            .load(
                BeatmapInput::PathWithIdentity {
                    path: &path,
                    memory_identity: "checksum-a",
                },
                Ruleset::Osu,
                &mods("m", 1.0),
                ScoringMode::Stable,
            )
            .unwrap();
        assert!(
            Arc::ptr_eq(&first, &cached),
            "deleted file proves the cache returned before stat/canonicalize/read/hash"
        );
        assert_eq!(cache.parse_count(), 1);
        assert!(
            matches!(
                cache.load(
                    BeatmapInput::PathWithIdentity {
                        path: &path,
                        memory_identity: "checksum-a"
                    },
                    Ruleset::Osu,
                    &mods("m", 1.25),
                    ScoringMode::Stable,
                ),
                Err(BeatmapError::NotAFile(_))
            ),
            "clock rate is an independent PathWithIdentity cache dimension"
        );
        assert!(matches!(
            cache.load(
                BeatmapInput::PathWithIdentity {
                    path: &path,
                    memory_identity: "checksum-b"
                },
                Ruleset::Osu,
                &mods("m", 1.0),
                ScoringMode::Stable,
            ),
            Err(BeatmapError::NotAFile(_))
        ));
    }

    #[test]
    fn missing_files_are_explicit_errors() {
        let mut cache = BeatmapCache::new(RecordingEngine::default());
        assert!(matches!(
            cache.load(
                BeatmapInput::Path(Path::new("/definitely/missing/map.osu")),
                Ruleset::Osu,
                &mods("m", 1.0),
                ScoringMode::Stable
            ),
            Err(BeatmapError::NotAFile(_))
        ));
        assert_eq!(cache.parse_count(), 0);
    }

    #[test]
    fn range_index_handles_adversarial_nested_intervals() {
        let ranges = (0..100_000)
            .map(|i| TimeRange {
                start: f64::from(i),
                end: if i == 0 {
                    200_000.0
                } else {
                    f64::from(i) + 0.25
                },
            })
            .collect();
        let index = RangeIndex::new(ranges);
        assert!(index.contains(150_000.0));
        assert!(!index.contains(200_000.001));
    }
