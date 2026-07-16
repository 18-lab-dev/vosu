// Copyright (c) ppy Pty Ltd <contact@ppy.sh>. Licensed under the MIT Licence.
//
// Signed-score counterpart of osu!'s pinned OsuPerformanceCalculator. The
// public wrapper stores hit statistics as C# `int`; generated or projected
// scores may therefore contain negative results which cannot be represented by
// rosu-pp's unsigned compatibility state. Keep this calculation source-shaped
// instead of clamping or wrapping those values.

use std::f64::consts::PI;

use rosu_pp::{
    GameMods,
    osu::OsuDifficultyAttributes,
    util::difficulty::{erf, erf_inv, logistic, norm, reverse_lerp, smoothstep},
};

use crate::{PerformanceAttrsData, ScoreInfoData};

const PERFORMANCE_BASE_MULTIPLIER: f64 = 1.12;
const PERFORMANCE_NORM_EXPONENT: f64 = 1.1;

pub(crate) fn calculate(
    attrs: &OsuDifficultyAttributes,
    mods: &GameMods,
    using_classic_slider_accuracy: bool,
    score: &ScoreInfoData,
) -> PerformanceAttrsData {
    let calc = SignedOsuPerformanceCalculator {
        attrs,
        mods,
        score,
        using_classic_slider_accuracy,
    };

    calc.calculate()
}

struct SignedOsuPerformanceCalculator<'a> {
    attrs: &'a OsuDifficultyAttributes,
    mods: &'a GameMods,
    score: &'a ScoreInfoData,
    using_classic_slider_accuracy: bool,
}

impl SignedOsuPerformanceCalculator<'_> {
    fn calculate(&self) -> PerformanceAttrsData {
        let total_hits = self.total_hits();

        if total_hits == 0.0 {
            return PerformanceAttrsData::default();
        }

        let mut effective_miss_count =
            if self.using_classic_slider_accuracy && !self.mods.sv2() && self.score.is_legacy_score
            {
                self.calculate_legacy_score_miss_count()
            } else {
                self.calculate_combo_based_estimated_miss_count()
            };
        effective_miss_count = effective_miss_count.max(f64::from(self.score.misses));
        effective_miss_count = effective_miss_count.min(total_hits);

        let (aim_estimated_slider_breaks, speed_estimated_slider_breaks) =
            if effective_miss_count > 0.0 {
                (
                    self.calculate_estimated_slider_breaks(
                        self.attrs.aim_top_weighted_slider_factor,
                        effective_miss_count,
                    ),
                    self.calculate_estimated_slider_breaks(
                        self.attrs.speed_top_weighted_slider_factor,
                        effective_miss_count,
                    ),
                )
            } else {
                (0.0, 0.0)
            };

        let mut multiplier = PERFORMANCE_BASE_MULTIPLIER;

        if self.mods.nf() {
            multiplier *= (1.0 - 0.02 * effective_miss_count).max(0.9);
        }

        if self.mods.so() && total_hits > 0.0 {
            multiplier *= 1.0 - (f64::from(self.attrs.n_spinners) / total_hits).powf(0.85);
        }

        if self.mods.rx() {
            let od = self.attrs.od();
            let (ok_multiplier, meh_multiplier) = if od > 0.0 {
                (
                    0.75 * (1.0 - od / 13.33).max(0.0),
                    (1.0 - (od / 13.33).powf(5.0)).max(0.0),
                )
            } else {
                (1.0, 1.0)
            };

            effective_miss_count = (effective_miss_count
                + f64::from(self.score.oks) * ok_multiplier
                + f64::from(self.score.mehs) * meh_multiplier)
                .min(total_hits);
        }

        let speed_deviation = self.calculate_speed_deviation();
        let aim = self.compute_aim_value(effective_miss_count, aim_estimated_slider_breaks);
        let speed = self.compute_speed_value(
            speed_deviation,
            effective_miss_count,
            speed_estimated_slider_breaks,
        );
        let accuracy = self.compute_accuracy_value();
        let reading = self.compute_reading_value(effective_miss_count, aim_estimated_slider_breaks);
        let flashlight = self.compute_flashlight_value(effective_miss_count);
        let cognition = sum_cognition_difficulty(reading, flashlight);
        let pp = norm(PERFORMANCE_NORM_EXPONENT, [aim, speed, accuracy, cognition]) * multiplier;

        PerformanceAttrsData {
            pp,
            aim,
            flashlight,
            speed,
            reading,
            accuracy,
            effective_miss_count: if self.score.accuracy.is_nan() {
                f64::NAN
            } else {
                effective_miss_count
            },
            ..PerformanceAttrsData::default()
        }
    }

    fn compute_aim_value(
        &self,
        effective_miss_count: f64,
        aim_estimated_slider_breaks: f64,
    ) -> f64 {
        if self.mods.ap() {
            return 0.0;
        }

        let mut aim_difficulty = self.attrs.aim;

        if self.attrs.n_sliders > 0 && self.attrs.aim_difficult_slider_count > 0.0 {
            let estimate_improperly_followed_difficult_sliders =
                if self.using_classic_slider_accuracy {
                    let maximum_possible_dropped_sliders = self.total_imperfect_hits();
                    maximum_possible_dropped_sliders
                        .min(f64::from(self.attrs.max_combo) - f64::from(self.score.max_combo))
                        .clamp(0.0, self.attrs.aim_difficult_slider_count)
                } else {
                    f64::from(self.n_slider_ends_dropped() + self.score.large_tick_misses)
                        .clamp(0.0, self.attrs.aim_difficult_slider_count)
                };

            let slider_nerf_factor = (1.0 - self.attrs.slider_factor)
                * (1.0
                    - estimate_improperly_followed_difficult_sliders
                        / self.attrs.aim_difficult_slider_count)
                    .powi(3)
                + self.attrs.slider_factor;
            aim_difficulty *= slider_nerf_factor;
        }

        let mut aim_value = 4.0 * aim_difficulty.powi(3);
        let total_hits = self.total_hits();
        let length_bonus = 0.95
            + 0.35 * (total_hits / 2000.0).min(1.0)
            + f64::from(u8::from(total_hits > 2000.0)) * (total_hits / 2000.0).log10() * 0.5;
        aim_value *= length_bonus;

        if effective_miss_count > 0.0 {
            let relevant_miss_count = (effective_miss_count + aim_estimated_slider_breaks)
                .min(self.total_imperfect_hits() + f64::from(self.score.large_tick_misses));
            aim_value *= Self::calculate_miss_penalty(
                relevant_miss_count,
                self.attrs.aim_difficult_strain_count,
            );
        }

        if self.mods.bl() {
            aim_value *= 1.3
                + total_hits
                    * (0.0016 / (1.0 + 2.0 * effective_miss_count))
                    * self.score.accuracy.powf(16.0)
                    * (1.0 - 0.003 * self.attrs.hp * self.attrs.hp);
        } else if self.mods.tc() {
            aim_value *= 1.0 + self.calculate_traceable_bonus(self.attrs.slider_factor);
        }

        aim_value * self.score.accuracy
    }

    fn compute_speed_value(
        &self,
        speed_deviation: Option<f64>,
        effective_miss_count: f64,
        speed_estimated_slider_breaks: f64,
    ) -> f64 {
        let Some(speed_deviation) = speed_deviation.filter(|_| !self.mods.rx()) else {
            return 0.0;
        };

        let mut speed_value = 4.0 * self.attrs.speed.powi(3);

        if effective_miss_count > 0.0 {
            let relevant_miss_count = (effective_miss_count + speed_estimated_slider_breaks)
                .min(self.total_imperfect_hits() + f64::from(self.score.large_tick_misses));
            speed_value *= Self::calculate_miss_penalty(
                relevant_miss_count,
                self.attrs.speed_difficult_strain_count,
            );
        }

        if self.mods.bl() {
            speed_value *= 1.12;
        }

        speed_value *= self.calculate_speed_high_deviation_nerf(speed_deviation);
        let effective_hit_window = 20.0 * (4.0 / self.attrs.speed).powf(0.35);
        let effective_accuracy = erf(effective_hit_window / speed_deviation);

        speed_value * effective_accuracy.powi(2)
    }

    fn compute_accuracy_value(&self) -> f64 {
        if self.mods.rx() {
            return 0.0;
        }

        let mut amount_hit_objects_with_accuracy = self.attrs.n_circles as i32;

        if !self.using_classic_slider_accuracy || self.mods.sv2() {
            amount_hit_objects_with_accuracy += self.attrs.n_sliders as i32;
        }

        let mut better_accuracy_percentage = if amount_hit_objects_with_accuracy > 0 {
            let excess_hits = (self.total_hits() as i32 - amount_hit_objects_with_accuracy).max(0);
            f64::from((self.score.greats - excess_hits) * 6 + self.score.oks * 2 + self.score.mehs)
                / f64::from(amount_hit_objects_with_accuracy * 6)
        } else {
            0.0
        };

        if better_accuracy_percentage < 0.0 {
            better_accuracy_percentage = 0.0;
        }

        let mut accuracy_value =
            1.52163_f64.powf(self.attrs.od()) * better_accuracy_percentage.powf(24.0) * 2.83;
        let object_count_factor = f64::from(amount_hit_objects_with_accuracy) / 1000.0;
        accuracy_value *= if amount_hit_objects_with_accuracy < 1000 {
            object_count_factor.powf(0.3)
        } else {
            object_count_factor.powf(0.1)
        };

        if self.mods.bl() {
            accuracy_value *= 1.14;
        } else if self.mods.tc() {
            accuracy_value *= 1.0 + 0.08 * reverse_lerp(self.attrs.ar, 11.5, 10.0);
        }

        accuracy_value
    }

    fn compute_flashlight_value(&self, effective_miss_count: f64) -> f64 {
        if !self.mods.fl() {
            return 0.0;
        }

        let mut flashlight_value = 25.0 * self.attrs.flashlight.powi(2);
        let total_hits = self.total_hits();

        if effective_miss_count > 0.0 {
            flashlight_value *= 0.97
                * (1.0 - (effective_miss_count / total_hits).powf(0.775))
                    .powf(effective_miss_count.powf(0.875));
        }

        flashlight_value *= self.combo_scaling_factor();
        flashlight_value *= 0.5 + self.score.accuracy / 2.0;

        flashlight_value
    }

    fn compute_reading_value(
        &self,
        effective_miss_count: f64,
        aim_estimated_slider_breaks: f64,
    ) -> f64 {
        let mut reading_value = 4.0 * self.attrs.reading.powi(3);

        if effective_miss_count > 0.0 {
            reading_value *= Self::calculate_miss_penalty(
                effective_miss_count + aim_estimated_slider_breaks,
                self.attrs.reading_difficult_note_count,
            );
        }

        reading_value * self.score.accuracy.powi(3)
    }

    fn calculate_combo_based_estimated_miss_count(&self) -> f64 {
        if self.attrs.n_sliders == 0 {
            return f64::from(self.score.misses);
        }

        let mut miss_count = f64::from(self.score.misses);

        if self.using_classic_slider_accuracy {
            let likely_missed_slider_end_portion =
                0.04 + 0.06 * self.attrs.aim_top_weighted_slider_factor.min(1.0).powi(2);
            let full_combo_threshold = f64::from(self.attrs.max_combo)
                - (4.0 + likely_missed_slider_end_portion * f64::from(self.attrs.n_sliders))
                    .min(f64::from(self.attrs.n_sliders));

            if f64::from(self.score.max_combo) < full_combo_threshold {
                miss_count = full_combo_threshold / f64::from(self.score.max_combo).max(1.0);
            }

            miss_count = miss_count.min(self.total_imperfect_hits());
            let max_possible_slider_breaks = (self.attrs.n_sliders as i32)
                .min((self.attrs.max_combo as i32 - self.score.max_combo) / 2);
            let slider_breaks = miss_count - f64::from(self.score.misses);

            if slider_breaks > f64::from(max_possible_slider_breaks) {
                miss_count = f64::from(self.score.misses + max_possible_slider_breaks);
            }
        } else {
            let full_combo_threshold =
                f64::from(self.attrs.max_combo as i32 - self.n_slider_ends_dropped());

            if f64::from(self.score.max_combo) < full_combo_threshold {
                miss_count = full_combo_threshold / f64::from(self.score.max_combo).max(1.0);
            }

            miss_count =
                miss_count.min(f64::from(self.score.large_tick_misses + self.score.misses));
        }

        miss_count
    }

    fn calculate_legacy_score_miss_count(&self) -> f64 {
        if self.attrs.max_combo == 0 {
            return 0.0;
        }

        let score_v1_multiplier =
            self.attrs.legacy_score_base_multiplier * self.legacy_score_multiplier();
        let relevant_combo_per_object = self.relevant_legacy_combo_per_object();
        let maximum_miss_count = self.maximum_legacy_combo_based_miss_count();
        let score_obtained_during_max_combo = self.legacy_score_at_combo(
            f64::from(self.score.max_combo),
            relevant_combo_per_object,
            score_v1_multiplier,
        );
        let remaining_score = self.score.total_score as f64 - score_obtained_during_max_combo;

        if remaining_score <= 0.0 {
            return maximum_miss_count;
        }

        let remaining_combo = f64::from(self.attrs.max_combo) - f64::from(self.score.max_combo);
        let expected_remaining_score = self.legacy_score_at_combo(
            remaining_combo,
            relevant_combo_per_object,
            score_v1_multiplier,
        );
        let score_based_miss_count = (expected_remaining_score / remaining_score).max(1.0);

        score_based_miss_count.min(maximum_miss_count)
    }

    fn legacy_score_at_combo(
        &self,
        combo: f64,
        relevant_combo_per_object: f64,
        score_v1_multiplier: f64,
    ) -> f64 {
        let total_hits = self
            .score
            .greats
            .wrapping_add(self.score.oks)
            .wrapping_add(self.score.mehs)
            .wrapping_add(self.score.misses);
        let estimated_objects = combo / relevant_combo_per_object - 1.0;
        let mut combo_score = if relevant_combo_per_object > 0.0 {
            (2.0 * (relevant_combo_per_object - 1.0)
                + (estimated_objects - 1.0) * relevant_combo_per_object)
                * estimated_objects
                / 2.0
        } else {
            0.0
        };

        combo_score *= self.score.accuracy * 300.0 / 25.0 * score_v1_multiplier;

        let objects_hit = f64::from(total_hits.wrapping_sub(self.score.misses)) * combo
            / f64::from(self.attrs.max_combo);
        let non_combo_score =
            (300.0 + self.attrs.nested_score_per_object) * self.score.accuracy * objects_hit;

        combo_score + non_combo_score
    }

    fn relevant_legacy_combo_per_object(&self) -> f64 {
        let mut combo_score = self.attrs.maximum_legacy_combo_score;
        combo_score /= 300.0 / 25.0 * self.attrs.legacy_score_base_multiplier;

        let max_combo = self.attrs.max_combo as i32;
        let numerator = max_combo.wrapping_sub(2).wrapping_mul(max_combo);

        f64::from(numerator)
            / (f64::from(self.attrs.max_combo) + 2.0 * (combo_score - 1.0)).max(1.0)
    }

    fn maximum_legacy_combo_based_miss_count(&self) -> f64 {
        if self.attrs.n_sliders == 0 {
            return f64::from(self.score.misses);
        }

        let total_imperfect_hits = self
            .score
            .oks
            .wrapping_add(self.score.mehs)
            .wrapping_add(self.score.misses);
        let likely_missed_slider_end_portion =
            0.04 + 0.06 * self.attrs.aim_top_weighted_slider_factor.min(1.0).powi(2);
        let full_combo_threshold = f64::from(self.attrs.max_combo)
            - (4.0 + likely_missed_slider_end_portion * f64::from(self.attrs.n_sliders))
                .min(f64::from(self.attrs.n_sliders));
        let mut miss_count = 0.0;

        if f64::from(self.score.max_combo) < full_combo_threshold {
            miss_count =
                (full_combo_threshold / f64::from(self.score.max_combo).max(1.0)).powf(2.5);
        }

        miss_count = miss_count.min(f64::from(total_imperfect_hits));

        let max_possible_slider_breaks = (self.attrs.n_sliders as i32)
            .min((self.attrs.max_combo as i32).wrapping_sub(self.score.max_combo) / 2);
        let slider_breaks = miss_count - f64::from(self.score.misses);

        if slider_breaks > f64::from(max_possible_slider_breaks) {
            miss_count = f64::from(self.score.misses.wrapping_add(max_possible_slider_breaks));
        }

        miss_count
    }

    fn legacy_score_multiplier(&self) -> f64 {
        let score_v2 = self.mods.sv2();
        let mut multiplier = 1.0;

        if self.mods.nf() {
            multiplier *= if score_v2 { 1.0 } else { 0.5 };
        }
        if self.mods.ez() {
            multiplier *= 0.5;
        }
        if self.mods.clock_rate() < 1.0 {
            multiplier *= 0.3;
        }
        if self.mods.hd() {
            multiplier *= 1.06;
        }
        if self.mods.hr() {
            multiplier *= if score_v2 { 1.10 } else { 1.06 };
        }
        if self.mods.clock_rate() > 1.0 {
            multiplier *= if score_v2 { 1.20 } else { 1.12 };
        }
        if self.mods.fl() {
            multiplier *= 1.12;
        }
        if self.mods.so() {
            multiplier *= 0.9;
        }
        if self.mods.rx() || self.mods.ap() {
            multiplier = 0.0;
        }

        multiplier
    }

    fn calculate_estimated_slider_breaks(
        &self,
        top_weighted_slider_factor: f64,
        effective_miss_count: f64,
    ) -> f64 {
        let non_miss_mistakes = self.score.oks + self.score.mehs;

        if !self.using_classic_slider_accuracy || non_miss_mistakes == 0 {
            return 0.0;
        }

        let missed_combo_percent =
            1.0 - f64::from(self.score.max_combo) / f64::from(self.attrs.max_combo);
        let mut estimated_slider_breaks =
            (effective_miss_count * top_weighted_slider_factor).min(f64::from(non_miss_mistakes));
        let non_miss_mistake_adjustment = (f64::from(non_miss_mistakes) - estimated_slider_breaks
            + 4.5)
            / (f64::from(non_miss_mistakes) + 4.0);
        estimated_slider_breaks *= smoothstep(effective_miss_count, 1.0, 2.0);

        estimated_slider_breaks
            * non_miss_mistake_adjustment
            * logistic(missed_combo_percent, 0.33, 15.0, None)
    }

    fn calculate_speed_deviation(&self) -> Option<f64> {
        if self.total_successful_hits() == 0 {
            return None;
        }

        let mut speed_note_count = self.attrs.speed_note_count;
        speed_note_count += (self.total_hits() - self.attrs.speed_note_count) * 0.1;
        let relevant_count_miss = f64::from(self.score.misses).min(speed_note_count);
        let relevant_count_meh =
            f64::from(self.score.mehs).min(speed_note_count - relevant_count_miss);
        let relevant_count_ok = f64::from(self.score.oks)
            .min(speed_note_count - relevant_count_miss - relevant_count_meh);
        let relevant_count_great =
            (speed_note_count - relevant_count_miss - relevant_count_meh - relevant_count_ok)
                .max(0.0);

        self.calculate_deviation(relevant_count_great, relevant_count_ok, relevant_count_meh)
    }

    fn calculate_deviation(
        &self,
        relevant_count_great: f64,
        relevant_count_ok: f64,
        relevant_count_meh: f64,
    ) -> Option<f64> {
        if relevant_count_great + relevant_count_ok + relevant_count_meh <= 0.0 {
            return None;
        }

        let n = (relevant_count_great + relevant_count_ok).max(1.0);
        let p = relevant_count_great / n;
        const Z: f64 = 2.326_347_874_04;
        let p_lower_bound = ((n * p + Z * Z / 2.0) / (n + Z * Z)
            - Z / (n + Z * Z) * (n * p * (1.0 - p) + Z * Z / 4.0).sqrt())
        .min(p);
        let mut deviation = if p_lower_bound > 0.01 {
            let mut deviation =
                self.attrs.great_hit_window / (2.0_f64.sqrt() * erf_inv(p_lower_bound));
            let ok_hit_window_tail_amount = (2.0 / PI).sqrt()
                * self.attrs.ok_hit_window
                * (-0.5 * (self.attrs.ok_hit_window / deviation).powi(2)).exp()
                / (deviation * erf(self.attrs.ok_hit_window / (2.0_f64.sqrt() * deviation)));
            deviation *= (1.0 - ok_hit_window_tail_amount).sqrt();
            deviation
        } else {
            self.attrs.ok_hit_window / 3.0_f64.sqrt()
        };
        let meh_variance = (self.attrs.meh_hit_window * self.attrs.meh_hit_window
            + self.attrs.ok_hit_window * self.attrs.meh_hit_window
            + self.attrs.ok_hit_window * self.attrs.ok_hit_window)
            / 3.0;
        deviation = (((relevant_count_great + relevant_count_ok) * deviation.powi(2)
            + relevant_count_meh * meh_variance)
            / (relevant_count_great + relevant_count_ok + relevant_count_meh))
            .sqrt();

        Some(deviation)
    }

    fn calculate_speed_high_deviation_nerf(&self, speed_deviation: f64) -> f64 {
        let speed_value = 4.0 * self.attrs.speed.powi(3);
        let excess_speed_difficulty_cutoff = 100.0 + 220.0 * (22.0 / speed_deviation).powf(6.5);

        if speed_value <= excess_speed_difficulty_cutoff {
            return 1.0;
        }

        const SCALE: f64 = 50.0;
        let adjusted_speed_value = SCALE
            * (((speed_value - excess_speed_difficulty_cutoff) / SCALE + 1.0).ln()
                + excess_speed_difficulty_cutoff / SCALE);
        let amount = 1.0 - reverse_lerp(speed_deviation, 22.0, 27.0);
        let adjusted_speed_value =
            adjusted_speed_value + (speed_value - adjusted_speed_value) * amount;

        adjusted_speed_value / speed_value
    }

    fn calculate_traceable_bonus(&self, slider_factor: f64) -> f64 {
        let slider_factor = slider_factor.powi(6);
        let high_ar_slider_visibility_factor = 0.5 + slider_factor / 2.0;
        let low_ar_slider_visibility_factor = slider_factor;
        let mut bonus =
            0.0275 + 0.025 * (12.0 - self.attrs.ar.max(7.0)) * high_ar_slider_visibility_factor;

        if self.attrs.ar < 7.0 {
            bonus += 0.025 * (7.0 - self.attrs.ar.max(0.0)) * low_ar_slider_visibility_factor;
        }

        if self.attrs.ar < 0.0 {
            bonus += 0.025 * (1.0 - 1.5_f64.powf(self.attrs.ar)) * low_ar_slider_visibility_factor;
        }

        bonus
    }

    fn calculate_miss_penalty(miss_count: f64, difficult_strain_count: f64) -> f64 {
        0.93 / (miss_count / (4.0 * difficult_strain_count.ln()) + 1.0)
    }

    fn combo_scaling_factor(&self) -> f64 {
        if self.attrs.max_combo == 0 {
            1.0
        } else {
            (f64::from(self.score.max_combo).powf(0.8) / f64::from(self.attrs.max_combo).powf(0.8))
                .min(1.0)
        }
    }

    fn total_hits(&self) -> f64 {
        f64::from(self.score.greats + self.score.oks + self.score.mehs + self.score.misses)
    }

    fn total_successful_hits(&self) -> i32 {
        self.score.greats + self.score.oks + self.score.mehs
    }

    fn total_imperfect_hits(&self) -> f64 {
        f64::from(self.score.oks + self.score.mehs + self.score.misses)
    }

    fn n_slider_ends_dropped(&self) -> i32 {
        self.attrs.n_sliders as i32 - self.score.slider_end_hits
    }
}

fn sum_cognition_difficulty(reading: f64, flashlight: f64) -> f64 {
    if reading <= 0.0 {
        return flashlight;
    }

    if flashlight <= 0.0 {
        return reading;
    }

    norm(
        PERFORMANCE_NORM_EXPONENT,
        [
            reading,
            flashlight * (flashlight / reading).clamp(0.25, 1.0),
        ],
    )
}
