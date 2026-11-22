    use std::sync::atomic::{AtomicUsize, Ordering};

    const OSU: &str = r#"osu file format v14

[General]
AudioFilename: audio.mp3
PreviewTime: 750
Mode: 0

[Metadata]
Title:Title
TitleUnicode:題
Artist:Artist
ArtistUnicode:藝術家
Creator:Mapper
Version:Hard
Source:Source
Tags:a b
BeatmapID:12
BeatmapSetID:34

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
SliderMultiplier:1.4
SliderTickRate:1

[Events]
0,0,"bg.jpg",0,0
2,900,2100
2,1200,1500

[TimingPoints]
0,500,4,2,1,80,1,0
1000,400,4,2,1,80,1,1
1600,300,4,2,1,80,1,0
3000,600,4,2,1,80,1,1

[HitObjects]
64,192,1000,1,0,0:0:0:0:
128,192,1400,2,0,B|256:192,1,140
256,192,1900,8,0,2300
"#;

    const TAIKO: &str = r#"osu file format v14
[General]
Mode:1
[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
[TimingPoints]
0,500,4,2,1,80,1,0
[HitObjects]
64,192,1000,1,0,0:0:0:0:
"#;

    #[derive(Default)]
    struct RecordingEngine {
        calls: AtomicUsize,
    }

    impl BeatmapEngine for RecordingEngine {
        fn calculate(&self, request: EngineRequest<'_>) -> Result<EngineOutput, BeatmapError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            assert!(!request.content.is_empty());
            let values = DifficultyValues {
                approach_rate: 9.0,
                circle_size: 4.0,
                drain_rate: 5.0,
                overall_difficulty: 8.0,
            };
            let mut strains = BTreeMap::new();
            for name in strain_names(request.effective_mode) {
                strains.insert(
                    (*name).to_owned(),
                    PeakStrains {
                        section_length: 400.0,
                        values: vec![1.0, 2.0],
                    },
                );
            }
            Ok(EngineOutput {
                original_difficulty: values.clone(),
                converted_difficulty: values,
                difficulty: DifficultyAttributes {
                    stars: 5.5,
                    max_combo: 7,
                    ..Default::default()
                },
                maximum_score: ScoreData {
                    max_combo: 7,
                    maximum_score: 1_000_000,
                },
                pp_by_accuracy: (90..=100).map(|acc| (acc, f64::from(acc))).collect(),
                strains,
                ..Default::default()
            })
        }
    }

    struct EmptyStrainEngine;

    impl BeatmapEngine for EmptyStrainEngine {
        fn calculate(&self, request: EngineRequest<'_>) -> Result<EngineOutput, BeatmapError> {
            let mut strains = BTreeMap::new();
            for name in strain_names(request.effective_mode) {
                strains.insert(
                    (*name).to_owned(),
                    PeakStrains {
                        section_length: 400.0,
                        values: Vec::new(),
                    },
                );
            }
            Ok(EngineOutput {
                strains,
                ..Default::default()
            })
        }
    }

    struct SingleStrainEngine;

    impl BeatmapEngine for SingleStrainEngine {
        fn calculate(&self, request: EngineRequest<'_>) -> Result<EngineOutput, BeatmapError> {
            let mut strains = BTreeMap::new();
            for name in strain_names(request.effective_mode) {
                strains.insert(
                    (*name).to_owned(),
                    PeakStrains {
                        section_length: 400.0,
                        values: vec![1.0],
                    },
                );
            }
            Ok(EngineOutput {
                strains,
                ..Default::default()
            })
        }
    }

    fn mods(checksum: &str, rate: f64) -> ModSet {
        ModSet::new(
            checksum,
            vec![AppliedMod {
                acronym: "HD".into(),
                settings_json: None,
            }],
            rate,
        )
        .unwrap()
    }

