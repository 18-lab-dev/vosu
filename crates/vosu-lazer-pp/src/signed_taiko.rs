// Copyright (c) ppy Pty Ltd <contact@ppy.sh>. Licensed under the MIT Licence.
//
// Source-shaped port of osu.Game.Rulesets.Taiko/Difficulty/
// TaikoPerformanceCalculator.cs at osu! revision 97e94bab. This separate path
// preserves the wrapper's signed statistics for infeasible simulated scores;
// the normal rosu-pp score state is intentionally unsigned.

use rosu_pp::{
    GameMods,
    taiko::TaikoDifficultyAttributes,
    util::difficulty::{erf, erf_inv, logistic, reverse_lerp},
};

use crate::{PerformanceAttrsData, ScoreInfoData};

pub(crate) fn calculate(
    attrs: &TaikoDifficultyAttributes,
    mods: &GameMods,
    classic: bool,
    score: &ScoreInfoData,
) -> PerformanceAttrsData {
    let calculator = Calculator {
        attrs,
        mods,
        classic,
        score,
    };

    let estimated_unstable_rate = if score.greats == 0 || attrs.great_hit_window <= 0.0 {
        None
    } else {
        Some(
            calculator
                .compute_deviation_upper_bound(f64::from(score.greats) / calculator.total_hits())
                * 10.0,
        )
    };

    let total_difficult_hits = calculator.total_hits() * attrs.consistency_factor;
    let difficulty =
        calculator.compute_difficulty_value(total_difficult_hits, estimated_unstable_rate) * 1.08;
    let accuracy =
        calculator.compute_accuracy_value(total_difficult_hits, estimated_unstable_rate) * 1.1;

    PerformanceAttrsData {
        pp: difficulty + accuracy,
        // Preserve PerformanceAttrsData.FromAttributes' pinned field-assignment quirk.
        effective_miss_count: estimated_unstable_rate.unwrap_or(-1.0),
        pp_difficulty: difficulty,
        ..PerformanceAttrsData::default()
    }
}

struct Calculator<'a> {
    attrs: &'a TaikoDifficultyAttributes,
    mods: &'a GameMods,
    classic: bool,
    score: &'a ScoreInfoData,
}

impl Calculator<'_> {
    fn compute_difficulty_value(
        &self,
        total_difficult_hits: f64,
        estimated_unstable_rate: Option<f64>,
    ) -> f64 {
        let Some(estimated_unstable_rate) = estimated_unstable_rate else {
            return 0.0;
        };

        if total_difficult_hits == 0.0 {
            return 0.0;
        }

        let rhythm_expected_unstable_rate = self.compute_deviation_upper_bound(1.0) * 10.0;
        let rhythm_maximum_unstable_rate = self.compute_deviation_upper_bound(0.8) * 10.0;
        let rhythm_factor = reverse_lerp(self.attrs.rhythm / self.attrs.stars, 0.15, 0.4);
        let rhythm_penalty = 1.0
            - logistic(
                estimated_unstable_rate,
                (rhythm_expected_unstable_rate + rhythm_maximum_unstable_rate) / 2.0,
                10.0 / (rhythm_maximum_unstable_rate - rhythm_expected_unstable_rate),
                Some(0.25 * rhythm_factor.powi(3)),
            );

        let base_difficulty = 5.0 * (self.attrs.stars * rhythm_penalty / 0.110).max(1.0) - 4.0;
        let mut value =
            (base_difficulty.powi(3) / 69_052.51).min(base_difficulty.powf(2.25) / 1250.0);

        value *= 1.0 + 0.10 * (self.attrs.stars - 10.0).max(0.0);

        let length_bonus = 1.0 + 0.25 * total_difficult_hits / (total_difficult_hits + 4000.0);
        value *= length_bonus;

        let miss_penalty = 0.97 + 0.03 * total_difficult_hits / (total_difficult_hits + 1500.0);
        value *= miss_penalty.powf(f64::from(self.score.misses));

        if self.mods.hd() {
            let mut hidden_bonus = if self.attrs.is_convert { 0.025 } else { 0.1 };

            if !self.mods.fl() {
                if !self.classic {
                    hidden_bonus *= 0.2;
                }

                if self.mods.ez() && self.classic {
                    hidden_bonus *= 0.5;
                }
            }

            value *= 1.0 + hidden_bonus;
        }

        if self.mods.fl() {
            value *=
                (1.05 - (self.attrs.mono_stamina_factor / 50.0).min(1.0) * length_bonus).max(1.0);
        }

        let mono_acc_scaling_exponent = 2.0 + self.attrs.mono_stamina_factor;
        let mono_acc_scaling_shift = 500.0 - 100.0 * (self.attrs.mono_stamina_factor * 3.0);

        value
            * erf(mono_acc_scaling_shift / (f64::sqrt(2.0) * estimated_unstable_rate))
                .powf(mono_acc_scaling_exponent)
    }

    fn compute_accuracy_value(
        &self,
        total_difficult_hits: f64,
        estimated_unstable_rate: Option<f64>,
    ) -> f64 {
        let Some(estimated_unstable_rate) = estimated_unstable_rate else {
            return 0.0;
        };

        if self.attrs.great_hit_window <= 0.0 {
            return 0.0;
        }

        let mut value = 470.0 * 0.9885_f64.powf(estimated_unstable_rate);
        value *=
            1.0 + (50.0 / estimated_unstable_rate).powi(2) * self.attrs.stars.powf(2.8) / 600.0;

        if self.mods.hd() && !self.attrs.is_convert {
            value *= 1.075;
        }

        value *= 1.0 + 0.3 * total_difficult_hits / (total_difficult_hits + 4000.0);

        let memory_length_bonus = (self.total_hits() / 1500.0).powf(0.3).min(1.15);

        if self.mods.fl() && self.mods.hd() && !self.attrs.is_convert {
            value *= (1.05 * memory_length_bonus).max(1.0);
        }

        value
    }

    fn compute_deviation_upper_bound(&self, accuracy: f64) -> f64 {
        const Z: f64 = 2.32634787404;

        let n = self.total_hits();
        let p_lower_bound = (n * accuracy + Z * Z / 2.0) / (n + Z * Z)
            - Z / (n + Z * Z) * (n * accuracy * (1.0 - accuracy) + Z * Z / 4.0).sqrt();

        self.attrs.great_hit_window / (f64::sqrt(2.0) * erf_inv(p_lower_bound))
    }

    fn total_hits(&self) -> f64 {
        f64::from(
            self.score
                .greats
                .wrapping_add(self.score.oks)
                .wrapping_add(self.score.mehs)
                .wrapping_add(self.score.misses),
        )
    }
}
