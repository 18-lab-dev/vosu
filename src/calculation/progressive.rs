//! Source-compatible progressive gameplay and result-screen calculations.
//!
//! This module deliberately owns only calculation state. Beatmap parsing, concrete
//! performance engines, and reducer scheduling are supplied through the narrow
//! [`ProgressiveBeatmap`] and [`GradualProgression`] traits.

mod gameplay;
mod live_adapter;
mod models;
mod result;
mod score;

pub use gameplay::GameplayCalculator;
pub use live_adapter::{LiveAdapterError, LiveGradualProgression, LiveProgressiveBeatmap};
pub use models::{
    CalculationOutcome, ClientScoring, DifficultyAttributes, GameplayCacheKey, GameplayFrame,
    GameplayGate, GradualProgression, HitStatistics, PerformanceAttributes, ProgressiveAttributes,
    ProgressiveBeatmap, Ruleset, ScoreInput, SkipReason,
};
pub use result::{ResultCalculator, ResultFrame, ResultPerformance};

#[cfg(test)]
use std::fmt;
#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    #[derive(Debug, Clone, Copy)]
    struct TestError;

    impl fmt::Display for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("fake engine failure")
        }
    }

    #[derive(Default)]
    struct Calls {
        gradual_creations: usize,
        skipped_times: Vec<f64>,
        accuracy: Vec<ScoreInput>,
        performance: Vec<(DifficultyAttributes, ScoreInput)>,
        fail_performance: bool,
        skip_returns_zero: bool,
    }

    struct FakeGradual {
        calls: Rc<RefCell<Calls>>,
        diff: DifficultyAttributes,
    }

    impl GradualProgression for FakeGradual {
        type Error = TestError;
        type Difficulty = DifficultyAttributes;

        fn skip_to_time(&mut self, play_time: f64) -> Result<usize, Self::Error> {
            let mut calls = self.calls.borrow_mut();
            calls.skipped_times.push(play_time);
            Ok(usize::from(play_time > 0.0 && !calls.skip_returns_zero))
        }

        fn difficulty(&mut self) -> Result<DifficultyAttributes, Self::Error> {
            Ok(self.diff)
        }

        fn progressive_accuracy(&mut self, score: &ScoreInput) -> Result<f64, Self::Error> {
            Ok((score.greats * 6 + score.oks * 2 + score.mehs) as f64 / 600.0)
        }
        fn calculate_performance(
            &mut self,
            score: &ScoreInput,
        ) -> Result<PerformanceAttributes, Self::Error> {
            let mut calls = self.calls.borrow_mut();
            if calls.fail_performance {
                return Err(TestError);
            }
            calls.performance.push((self.diff, *score));
            Ok(PerformanceAttributes {
                pp: self.diff.stars * 100.0
                    + score.accuracy * 10.0
                    + score.max_combo as f64 / 100.0,
                accuracy: score.accuracy,
                aim: score.greats as f64,
                difficulty: score.misses as f64,
                flashlight: score.slider_end_hits as f64,
                speed: score.large_tick_hits as f64,
                reading: 0.0,
            })
        }
    }

    struct FakeMap {
        calls: Rc<RefCell<Calls>>,
        full: DifficultyAttributes,
        current: DifficultyAttributes,
        maximum: ScoreInput,
    }

    impl ProgressiveBeatmap for FakeMap {
        type Error = TestError;
        type Difficulty = DifficultyAttributes;
        type Gradual = FakeGradual;

        fn create_gradual(&self) -> Result<Self::Gradual, Self::Error> {
            self.calls.borrow_mut().gradual_creations += 1;
            Ok(FakeGradual {
                calls: self.calls.clone(),
                diff: self.current,
            })
        }

        fn full_difficulty(&self) -> &DifficultyAttributes {
            &self.full
        }
        fn difficulty_summary(&self, difficulty: &Self::Difficulty) -> DifficultyAttributes {
            *difficulty
        }
        fn max_score(&self) -> &ScoreInput {
            &self.maximum
        }

        fn calculate_accuracy(&self, score: &ScoreInput) -> Result<f64, Self::Error> {
            self.calls.borrow_mut().accuracy.push(*score);
            Ok((score.perfects * 7
                + score.greats * 5
                + score.goods * 3
                + score.oks * 2
                + score.mehs) as f64
                / 1000.0)
        }

        fn calculate_performance(
            &self,
            difficulty: &DifficultyAttributes,
            score: &ScoreInput,
        ) -> Result<PerformanceAttributes, Self::Error> {
            let mut calls = self.calls.borrow_mut();
            if calls.fail_performance {
                return Err(TestError);
            }
            calls.performance.push((*difficulty, *score));
            Ok(PerformanceAttributes {
                pp: difficulty.stars * 100.0
                    + score.accuracy * 10.0
                    + score.max_combo as f64 / 100.0,
                accuracy: score.accuracy,
                aim: score.greats as f64,
                difficulty: score.misses as f64,
                flashlight: score.slider_end_hits as f64,
                speed: score.large_tick_hits as f64,
                reading: 0.0,
            })
        }
    }

    fn fixture() -> (FakeMap, GameplayCacheKey, HitStatistics) {
        let calls = Rc::new(RefCell::new(Calls::default()));
        let map = FakeMap {
            calls,
            full: DifficultyAttributes {
                stars: 6.0,
                max_combo: 1000,
                n_sliders: 300,
                n_objects: 1000,
                nested_score_per_object: 0.0,
                maximum_legacy_combo_score: 300_000.0,
            },
            current: DifficultyAttributes {
                stars: 4.0,
                max_combo: 400,
                n_sliders: 120,
                n_objects: 400,
                nested_score_per_object: 0.0,
                maximum_legacy_combo_score: 120_000.0,
            },
            maximum: ScoreInput {
                max_combo: 1000,
                perfects: 1000,
                greats: 1000,
                slider_end_hits: 300,
                small_tick_hits: 500,
                large_tick_hits: 400,
                ..ScoreInput::default()
            },
        };
        let key = GameplayCacheKey {
            beatmap_checksum: "map".into(),
            mode: 0,
            mods_checksum: "mods".into(),
            mp3_length: 123_456.0,
        };
        let stats = HitStatistics {
            great: 350,
            ok: 20,
            meh: 5,
            miss: 3,
            small_tick_miss: 7,
            large_tick_miss: 11,
            large_tick_hit: 100,
            slider_tail_hit: 90,
            combo_break: 2,
            ..HitStatistics::default()
        };
        (map, key, stats)
    }

    fn active<'a>(key: &'a GameplayCacheKey, stats: HitStatistics) -> GameplayFrame<'a> {
        GameplayFrame {
            gate: GameplayGate::Active,
            reset_fc_pp: 0.0,
            key,
            play_time: 50_000.0,
            mode: Ruleset::Osu,
            client: ClientScoring::Stable,
            score: 765_432,
            combo: 370,
            max_combo: 380,
            statistics: stats,
        }
    }

    #[test]
    fn source_gameplay_golden_vector_assembles_current_achievable_and_fc_scores() {
        let (map, key, stats) = fixture();
        let mut calc = GameplayCalculator::default();
        assert_eq!(
            calc.update(Some(&map), active(&key, stats)),
            CalculationOutcome::Updated
        );
        let calls = map.calls.borrow();
        assert_eq!(calls.performance.len(), 3);
        let current = calls.performance[0].1;
        assert_eq!(current.total_score, 765_432);
        assert!(current.is_legacy_score);
        assert_eq!(
            (current.greats, current.oks, current.mehs, current.misses),
            (350, 20, 5, 3)
        );
        let achievable = calls.performance[1].1;
        assert_eq!(achievable.max_combo, 970); // 1000 - (400 - 370)
        assert_eq!(achievable.greats, 972);
        assert_eq!(achievable.large_tick_hits, 389);
        assert_eq!(achievable.slider_end_hits, 270);
        assert_eq!(achievable.small_tick_hits, 493);
        let fc = calls.performance[2].1;
        assert_eq!(fc.max_combo, 1000);
        assert_eq!(fc.greats, 977);
        assert_eq!((fc.misses, fc.combo_breaks), (0, 0));
        assert_eq!((fc.large_tick_hits, fc.large_tick_misses), (400, 0));
        assert_eq!(fc.slider_end_hits, 300);
        assert_eq!(calc.attributes().max_achieved, calc.attributes().pp);
    }

    #[test]
    fn zero_passed_object_window_retains_values_and_does_not_commit_play_time() {
        let (map, key, stats) = fixture();
        map.calls.borrow_mut().skip_returns_zero = true;
        let mut calc = GameplayCalculator::default();
        assert_eq!(
            calc.update(Some(&map), active(&key, stats)),
            CalculationOutcome::Unchanged
        );
        assert_eq!(*calc.attributes(), ProgressiveAttributes::default());
        map.calls.borrow_mut().skip_returns_zero = false;
        // Since the zero-object window did not commit previousPlayTime, the same
        // time is retried once an object enters the inclusive gradual prefix.
        assert_eq!(
            calc.update(Some(&map), active(&key, stats)),
            CalculationOutcome::Updated
        );
        assert_eq!(map.calls.borrow().skipped_times, vec![50_000.0, 50_000.0]);
    }

    #[test]
    fn cache_key_windows_and_rewind_rebuild_match_source() {
        let (map, mut key, stats) = fixture();
        let mut calc = GameplayCalculator::default();
        assert_eq!(key.source_key(), "map:0:mods:123456");
        calc.update(Some(&map), active(&key, stats));
        assert_eq!(
            calc.update(Some(&map), active(&key, stats)),
            CalculationOutcome::Unchanged
        );
        key.mp3_length += 1.0;
        // Source rebuilds the cache but returns early when play time is unchanged.
        assert_eq!(
            calc.update(Some(&map), active(&key, stats)),
            CalculationOutcome::Unchanged
        );
        let mut rewind = active(&key, stats);
        rewind.play_time = 40_000.0;
        assert_eq!(calc.update(Some(&map), rewind), CalculationOutcome::Updated);
        assert_eq!(map.calls.borrow().gradual_creations, 3);
        assert_eq!(map.calls.borrow().skipped_times, vec![50_000.0, 40_000.0]);
        // lazerInstance applies resetQuick after an observed combo rewind. It
        // retains published PP but forces the same time window to be rebuilt on
        // the next update.
        let stale = *calc.attributes();
        calc.reset_quick();
        assert_eq!(*calc.attributes(), stale);
        assert_eq!(calc.update(Some(&map), rewind), CalculationOutcome::Updated);
        assert_eq!(map.calls.borrow().gradual_creations, 4);
        assert_eq!(
            map.calls.borrow().skipped_times,
            vec![50_000.0, 40_000.0, 40_000.0]
        );
    }

    #[test]
    fn status_gates_preserve_or_reset_progressive_values_in_source_order() {
        let (map, key, stats) = fixture();
        let mut calc = GameplayCalculator::default();
        calc.update(Some(&map), active(&key, stats));
        let stale = *calc.attributes();
        for (gate, reason) in [
            (GameplayGate::NotPlaying, SkipReason::NotPlaying),
            (GameplayGate::Tournament, SkipReason::Tournament),
            (
                GameplayGate::CalculationDisabled,
                SkipReason::CalculationDisabled,
            ),
            (
                GameplayGate::MissingGameFolder,
                SkipReason::MissingGameFolder,
            ),
            (
                GameplayGate::BeatmapUnavailable,
                SkipReason::BeatmapUnavailable,
            ),
        ] {
            let mut frame = active(&key, stats);
            frame.gate = gate;
            assert_eq!(
                calc.update(Some(&map), frame),
                CalculationOutcome::Skipped(reason)
            );
            assert_eq!(*calc.attributes(), stale);
        }

        map.calls.borrow_mut().skip_returns_zero = true;
        let mut before = active(&key, stats);
        before.gate = GameplayGate::BeforeFirstObject;
        assert_eq!(
            calc.update(Some(&map), before),
            CalculationOutcome::Unchanged
        );
        assert_eq!(*calc.attributes(), stale);

        let mut retry = active(&key, stats);
        retry.gate = GameplayGate::Retry;
        retry.reset_fc_pp = 321.5;
        assert_eq!(
            calc.update(Some(&map), retry),
            CalculationOutcome::Unchanged
        );
        assert_eq!(
            *calc.attributes(),
            ProgressiveAttributes {
                fc_pp: 321.5,
                ..ProgressiveAttributes::default()
            }
        );

        let mut selection = active(&key, stats);
        selection.gate = GameplayGate::SongSelectionReset;
        selection.reset_fc_pp = 654.25;
        assert_eq!(
            calc.update(Some(&map), selection),
            CalculationOutcome::Skipped(SkipReason::SongSelectionReset)
        );
        assert_eq!(
            *calc.attributes(),
            ProgressiveAttributes {
                fc_pp: 654.25,
                ..ProgressiveAttributes::default()
            }
        );
    }

    #[test]
    fn mania_achievable_and_fc_move_misses_to_perfects() {
        let (map, key, mut stats) = fixture();
        stats.perfect = 100;
        stats.good = 12;
        let mut frame = active(&key, stats);
        frame.mode = Ruleset::Mania;
        let mut calc = GameplayCalculator::default();
        calc.update(Some(&map), frame);
        let calls = map.calls.borrow();
        let achievable = calls.performance[1].1;
        assert_eq!(achievable.perfects, 960);
        assert_eq!(achievable.greats, 350);
        let fc = calls.performance[2].1;
        assert_eq!(fc.perfects, 965);
        assert_eq!((fc.misses, fc.combo_breaks), (0, 0));
    }

    #[test]
    fn result_cache_omits_score_and_tournament_rank() {
        let (map, _, stats) = fixture();
        let mut calc = ResultCalculator::default();
        let frame = ResultFrame {
            beatmap_checksum: "ab",
            mods_checksum: "c",
            mode: Ruleset::Osu,
            player_name: "12bob",
            tournament_rank: Some(9),
            client: ClientScoring::Stable,
            score: 900_000,
            max_combo: 800,
            statistics: stats,
        };
        assert_eq!(frame.source_cache_key(), "abc012bob");
        assert_eq!(calc.update(Some(&map), &frame), CalculationOutcome::Updated);
        let mut changed_but_omitted = frame.clone();
        changed_but_omitted.score += 1;
        changed_but_omitted.tournament_rank = Some(1);
        assert_eq!(
            calc.update(Some(&map), &changed_but_omitted),
            CalculationOutcome::Unchanged
        );
        assert_eq!(map.calls.borrow().performance.len(), 2);
    }

    #[test]
    fn engine_errors_retain_gameplay_and_result_outputs_and_do_not_cache_result_failure() {
        let (map, mut key, stats) = fixture();
        let mut gameplay = GameplayCalculator::default();
        assert_eq!(
            gameplay.update(Some(&map), active(&key, stats)),
            CalculationOutcome::Updated
        );
        let stale_gameplay = *gameplay.attributes();
        key.mp3_length += 1.0;
        map.calls.borrow_mut().fail_performance = true;
        let mut changed = active(&key, stats);
        changed.play_time += 1.0;
        assert_eq!(
            gameplay.update(Some(&map), changed),
            CalculationOutcome::RetainedError("fake engine failure".into())
        );
        assert_eq!(*gameplay.attributes(), stale_gameplay);

        let frame = ResultFrame {
            beatmap_checksum: "result",
            mods_checksum: "mods",
            mode: Ruleset::Taiko,
            player_name: "player",
            tournament_rank: None,
            client: ClientScoring::Lazer,
            score: 10,
            max_combo: 2,
            statistics: stats,
        };
        let mut result = ResultCalculator::default();
        assert_eq!(
            result.update(Some(&map), &frame),
            CalculationOutcome::RetainedError("fake engine failure".into())
        );
        assert_eq!(result.performance(), ResultPerformance::default());
        map.calls.borrow_mut().fail_performance = false;
        assert_eq!(
            result.update(Some(&map), &frame),
            CalculationOutcome::Updated
        );
    }
    const LIVE_OSU: &str = r#"osu file format v14

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

    #[test]
    fn production_live_adapter_runs_progressive_and_result_fc_paths() {
        let map = LiveProgressiveBeatmap::new(LIVE_OSU.as_bytes(), Ruleset::Osu, &[]).unwrap();
        assert_eq!(map.max_score().max_combo, 3);
        let key = GameplayCacheKey {
            beatmap_checksum: "map".into(),
            mode: 0,
            mods_checksum: "mods".into(),
            mp3_length: 2_000.0,
        };
        let statistics = HitStatistics {
            great: 2,
            miss: 1,
            combo_break: 1,
            ..HitStatistics::default()
        };
        let mut gameplay = GameplayCalculator::default();
        assert_eq!(
            gameplay.update(
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
                    statistics,
                },
            ),
            CalculationOutcome::Updated
        );
        assert!(gameplay.attributes().pp.is_finite());
        assert!(gameplay.attributes().max_achievable.is_finite());
        assert!(gameplay.attributes().fc_pp >= gameplay.attributes().pp);

        let mut result = ResultCalculator::default();
        assert_eq!(
            result.update(
                Some(&map),
                &ResultFrame {
                    beatmap_checksum: "map",
                    mods_checksum: "mods",
                    mode: Ruleset::Osu,
                    player_name: "player",
                    tournament_rank: Some(1),
                    client: ClientScoring::Lazer,
                    score: 900_000,
                    max_combo: 2,
                    statistics,
                },
            ),
            CalculationOutcome::Updated
        );
        assert!(result.performance().pp.is_finite());
        assert!(result.performance().fc_pp >= result.performance().pp);
    }

    #[test]
    fn production_live_adapter_accepts_signed_projected_results() {
        let map = LiveProgressiveBeatmap::new(LIVE_OSU.as_bytes(), Ruleset::Osu, &[]).unwrap();
        let projected = ScoreInput {
            accuracy: 0.5,
            max_combo: 1,
            greats: -2,
            misses: 5,
            ..ScoreInput::default()
        };

        let performance = map
            .calculate_performance(map.full_difficulty(), &projected)
            .unwrap();

        assert!(performance.pp.is_finite());
    }
}
