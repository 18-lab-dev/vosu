use crate::{
    GameMods,
    mania::{ManiaDifficultyAttributes, ManiaPerformanceAttributes, ManiaScoreState},
};

pub(super) struct ManiaPerformanceCalculator<'mods> {
    attrs: ManiaDifficultyAttributes,
    mods: &'mods GameMods,
    state: ManiaScoreState,
}

impl<'a> ManiaPerformanceCalculator<'a> {
    pub const fn new(
        attrs: ManiaDifficultyAttributes,
        mods: &'a GameMods,
        state: ManiaScoreState,
    ) -> Self {
        Self { attrs, mods, state }
    }
}

impl ManiaPerformanceCalculator<'_> {
    pub fn calculate(self) -> ManiaPerformanceAttributes {
        let mut multiplier = 1.0;

        if self.mods.nf() {
            multiplier *= 0.75;
        }

        if self.mods.ez() {
            multiplier *= 0.5;
        }

        let difficulty_value = self.compute_difficulty_value();
        let pp = difficulty_value * multiplier;

        ManiaPerformanceAttributes {
            difficulty: self.attrs,
            pp,
            pp_difficulty: difficulty_value,
        }
    }

    fn compute_difficulty_value(&self) -> f64 {
        // * Star rating to pp curve
        8.0 * f64::powf(f64::max(self.attrs.stars - 0.15, 0.05), 2.2)
             // * From 80% accuracy, 1/20th of total pp is awarded per additional 1% accuracy
             * f64::max(0.0, 5.0 * self.calculate_custom_accuracy() - 4.0)
             // * Length bonus, capped at 1500 notes
             * (1.0 + 0.1 * f64::min(1.0, self.total_hits() / 1500.0))
    }

    const fn total_hits(&self) -> f64 {
        self.state.total_hits() as f64
    }

    fn calculate_custom_accuracy(&self) -> f64 {
        let ManiaScoreState {
            n320,
            n300,
            n200,
            n100,
            n50,
            misses: _,
        } = &self.state;

        let total_hits = self.state.total_hits();

        if total_hits == 0 {
            return 0.0;
        }

        custom_accuracy(*n320, *n300, *n200, *n100, *n50, total_hits)
    }
}

pub(super) fn custom_accuracy(
    n320: u32,
    n300: u32,
    n200: u32,
    n100: u32,
    n50: u32,
    total_hits: u32,
) -> f64 {
    let numerator = n320 * 32 + n300 * 30 + n200 * 20 + n100 * 10 + n50 * 5;
    let denominator = total_hits * 32;

    f64::from(numerator) / f64::from(denominator)
}

#[cfg(test)]
mod tests {
    use crate::{
        GameMods,
        mania::{ManiaDifficultyAttributes, ManiaPerformanceAttributes, ManiaScoreState},
        model::mods::rosu_mods::{
            GameMod, GameMods as GameModsLazer,
            generated_mods::{ClassicMania, ScoreV2Mania},
        },
    };

    use super::ManiaPerformanceCalculator;

    fn attrs() -> ManiaDifficultyAttributes {
        ManiaDifficultyAttributes {
            stars: 2.0,
            n_objects: 24,
            n_hold_notes: 3,
            max_combo: 27,
            is_convert: false,
        }
    }

    fn calculate(mods: &GameMods, state: ManiaScoreState) -> ManiaPerformanceAttributes {
        ManiaPerformanceCalculator::new(attrs(), mods, state).calculate()
    }

    fn calculate_score_info_projection(
        mods: &GameMods,
        state: ManiaScoreState,
        _total_score: i64,
    ) -> ManiaPerformanceAttributes {
        // `ScoreInfo.TotalScore` intentionally does not enter the pinned
        // ManiaPerformanceCalculator; only hit statistics, mods, and attrs do.
        calculate(mods, state)
    }

    #[test]
    fn discriminator_performance_nonzero_full_precision_projection() {
        let state = ManiaScoreState {
            n320: 24,
            ..Default::default()
        };
        let result = calculate(&GameMods::default(), state);

        assert!(result.pp.is_normal());
        assert_eq!(result.pp, result.pp_difficulty);
        eprintln!(
            "mania.performance pp={:?} pp_difficulty={:?}",
            result.pp, result.pp_difficulty
        );
    }

    #[test]
    fn discriminator_difficulty_pp_accuracy_gate() {
        let above_gate = calculate(
            &GameMods::default(),
            ManiaScoreState {
                n320: 21,
                n300: 3,
                ..Default::default()
            },
        );
        let below_gate = calculate(
            &GameMods::default(),
            ManiaScoreState {
                n50: 24,
                ..Default::default()
            },
        );

        assert!(above_gate.pp_difficulty.is_normal());
        assert_eq!(below_gate.pp_difficulty, 0.0);
        eprintln!(
            "mania.performance.difficulty-pp above={:?} below={:?}",
            above_gate.pp_difficulty, below_gate.pp_difficulty
        );
    }

    #[test]
    fn discriminator_total_hits_length_bonus() {
        let short = calculate(
            &GameMods::default(),
            ManiaScoreState {
                n320: 24,
                ..Default::default()
            },
        );
        let capped = calculate(
            &GameMods::default(),
            ManiaScoreState {
                n320: 1500,
                ..Default::default()
            },
        );

        assert!(capped.pp_difficulty > short.pp_difficulty);
        let expected_ratio = 1.1 / (1.0 + 0.1 * 24.0 / 1500.0);
        assert!((capped.pp_difficulty / short.pp_difficulty - expected_ratio).abs() < 1e-12);
        eprintln!(
            "mania.performance.total-score-scaling short={:?} capped={:?}",
            short.pp_difficulty, capped.pp_difficulty
        );
    }

    #[test]
    fn discriminator_total_score_is_negative_invariance_pair() {
        let state = ManiaScoreState {
            n320: 21,
            n300: 3,
            ..Default::default()
        };
        let low = calculate_score_info_projection(&GameMods::default(), state.clone(), 500_000);
        let high = calculate_score_info_projection(&GameMods::default(), state, 1_000_000);

        assert_eq!(low, high);
        eprintln!(
            "mania.performance.total-score-invariance low={:?} high={:?}",
            low.pp_difficulty, high.pp_difficulty
        );
    }

    #[test]
    fn discriminator_score_v2_is_negative_performance_invariance_pair() {
        let score_v2 = GameMods::from(
            [GameMod::ScoreV2Mania(ScoreV2Mania::default())]
                .into_iter()
                .collect::<GameModsLazer>(),
        );
        let state = ManiaScoreState {
            n320: 21,
            n300: 3,
            ..Default::default()
        };
        let classic = calculate_score_info_projection(&GameMods::default(), state.clone(), 500_000);
        let score_v2 = calculate_score_info_projection(&score_v2, state, 1_000_000);

        assert_eq!(classic, score_v2);
        eprintln!(
            "mania.performance.score-v2-normalisation classic={:?} score_v2={:?}",
            classic.pp_difficulty, score_v2.pp_difficulty
        );
    }

    #[test]
    fn discriminator_classic_is_negative_performance_invariance_pair() {
        let classic_mod = GameMods::from(
            [GameMod::ClassicMania(ClassicMania::default())]
                .into_iter()
                .collect::<GameModsLazer>(),
        );
        let state = ManiaScoreState {
            n320: 2,
            n300: 1,
            n100: 1,
            ..Default::default()
        };
        let lazer = calculate(&GameMods::default(), state.clone());
        let classic = calculate(&classic_mod, state);

        assert_eq!(classic, lazer);
    }
}
