use std::f64::consts::PI;

use crate::{
    any::difficulty::object::{HasStartTime, IDifficultyObject},
    model::mods::GameMods,
    osu::difficulty::object::OsuDifficultyObject,
    util::difficulty::{logistic, norm, reverse_lerp, smootherstep},
};

/// Reading difficulty using lazer's harmonic object-difficulty aggregation.
#[derive(Clone)]
pub struct Reading {
    object_difficulties: Vec<f64>,
    object_start_times: Vec<f64>,
    current_strain: f64,
    has_hidden_mod: bool,
    has_touch_device_mod: bool,
    has_relax_mod: bool,
    has_autopilot_mod: bool,
    attraction_strength: Option<f64>,
    overall_difficulty: f64,
    current_section_peak: f64,
    current_section_end: f64,
    strain_peaks: Vec<f64>,
}

impl Reading {
    const READING_WINDOW_SIZE: f64 = 3_000.0;
    const DISTANCE_INFLUENCE_THRESHOLD: f64 = OsuDifficultyObject::NORMALIZED_DIAMETER as f64 * 1.5;
    const SKILL_MULTIPLIER: f64 = 2.5;
    const STRAIN_DECAY_BASE: f64 = 0.8;
    const REDUCED_DIFFICULTY_DURATION: f64 = 60_000.0;
    const SECTION_LENGTH: f64 = 400.0;

    fn strain_decay(ms: f64) -> f64 {
        Self::STRAIN_DECAY_BASE.powf(ms / 1_000.0)
    }

    pub fn new(mods: &GameMods, _time_preempt: f64, great_hit_window: f64) -> Self {
        Self {
            object_difficulties: Vec::with_capacity(256),
            object_start_times: Vec::with_capacity(256),
            current_strain: 0.0,
            has_hidden_mod: mods.hd_only_fade_approach_circles() != Some(true) && mods.hd(),
            has_touch_device_mod: mods.td(),
            has_relax_mod: mods.rx(),
            has_autopilot_mod: mods.ap(),
            attraction_strength: mods.attraction_strength(),
            overall_difficulty: (79.5 - great_hit_window) / 6.0,
            current_section_peak: 0.0,
            current_section_end: 0.0,
            strain_peaks: Vec::with_capacity(64),
        }
    }

    pub fn process(&mut self, curr: &OsuDifficultyObject<'_>, objects: &[OsuDifficultyObject<'_>]) {
        if curr.idx == 0 {
            self.current_section_end =
                (curr.start_time / Self::SECTION_LENGTH).ceil() * Self::SECTION_LENGTH;
        }

        while curr.start_time > self.current_section_end {
            self.strain_peaks.push(self.current_section_peak);
            let previous_start = curr
                .previous(0, objects)
                .map_or(0.0, HasStartTime::start_time);
            self.current_section_peak = self.current_strain
                * 0.15_f64.powf((self.current_section_end - previous_start) / 1_000.0);
            self.current_section_end += Self::SECTION_LENGTH;
        }

        self.object_start_times.push(curr.start_time);

        let decay = Self::strain_decay(curr.delta_time);
        self.current_strain *= decay;

        let mut difficulty = self.evaluate_difficulty(curr, objects);

        if self.has_touch_device_mod {
            difficulty = difficulty.powf(0.89);
        }
        if let Some(strength) = self.attraction_strength {
            difficulty *= 1.0 - strength;
        }
        if self.has_relax_mod {
            difficulty *= 0.4;
        }
        if self.has_autopilot_mod {
            difficulty *= 0.1;
        }

        difficulty *= 0.825 + self.overall_difficulty.max(0.0).powf(2.2) / 1_125.0;
        self.current_strain += difficulty * (1.0 - decay) * Self::SKILL_MULTIPLIER;
        self.object_difficulties.push(self.current_strain);
        self.current_section_peak = self.current_section_peak.max(self.current_strain);
    }

    fn evaluate_difficulty(
        &self,
        curr: &OsuDifficultyObject<'_>,
        objects: &[OsuDifficultyObject<'_>],
    ) -> f64 {
        if curr.base.is_spinner() || curr.idx == 0 {
            return 0.0;
        }

        let velocity = (curr.lazy_jump_dist / curr.adjusted_delta_time).max(1.0);
        let current_density = self.current_visible_object_density(curr, objects);
        let past_influence = self.past_object_difficulty_influence(curr, objects);
        let angle_nerf = Self::constant_angle_nerf(curr, objects);

        let mut future_influence = current_density.sqrt();
        if let Some(next) = curr.next(0, objects) {
            future_influence *= smootherstep(
                next.lazy_jump_dist,
                15.0,
                Self::DISTANCE_INFLUENCE_THRESHOLD,
            );
        }

        let mut density =
            (past_influence + future_influence).powf(1.7) * 0.4 * angle_nerf * velocity;
        density = (density - 2.5).max(0.0).powf(0.45) * 2.4;

        let mut preempt = ((500.0 - curr.time_preempt + (curr.time_preempt - 500.0).abs()) / 2.0)
            .powf(2.5)
            / 140_000.0;
        preempt *= angle_nerf * velocity;

        let hidden = if self.has_hidden_mod {
            let preempt_factor = curr.time_preempt.powf(2.2) * 0.01;
            let density_factor = (current_density + past_influence).powf(3.3) * 3.0;
            let mut hidden =
                ((preempt_factor + density_factor) * angle_nerf * velocity * 0.01).powf(0.4) * 0.28;

            if curr.lazy_jump_dist == 0.0
                && curr.previous(0, objects).is_some_and(|prev| {
                    curr.opacity_at(prev.base.start_time, true) == 0.0
                        && prev.start_time > curr.start_time - curr.time_preempt
                })
            {
                hidden += 0.28 * 2_500.0 / curr.adjusted_delta_time.powf(1.5);
            }

            hidden
        } else {
            0.0
        };

        norm(1.5, [preempt, hidden, density])
            / (1.0 - Self::STRAIN_DECAY_BASE.powf(curr.adjusted_delta_time / 1_000.0))
    }

    fn past_object_difficulty_influence(
        &self,
        curr: &OsuDifficultyObject<'_>,
        objects: &[OsuDifficultyObject<'_>],
    ) -> f64 {
        (0..curr.idx)
            .map_while(|i| curr.previous(i, objects))
            .take_while(|prev| {
                curr.start_time - prev.start_time <= Self::READING_WINDOW_SIZE
                    && prev.start_time >= curr.start_time - curr.time_preempt
            })
            .map(|prev| {
                curr.opacity_at(prev.base.start_time, false)
                    * smootherstep(
                        prev.lazy_jump_dist,
                        15.0,
                        Self::DISTANCE_INFLUENCE_THRESHOLD,
                    )
                    * Self::time_nerf(curr.start_time - prev.start_time)
            })
            .sum()
    }

    fn current_visible_object_density(
        &self,
        curr: &OsuDifficultyObject<'_>,
        objects: &[OsuDifficultyObject<'_>],
    ) -> f64 {
        (0..)
            .map_while(|i| curr.next(i, objects))
            .take_while(|next| {
                next.start_time - curr.start_time <= Self::READING_WINDOW_SIZE
                    && curr.start_time >= next.start_time - next.time_preempt
            })
            .map(|next| {
                next.opacity_at(curr.base.start_time, false)
                    * Self::time_nerf(next.start_time - curr.start_time)
            })
            .sum()
    }

    fn constant_angle_nerf(
        curr: &OsuDifficultyObject<'_>,
        objects: &[OsuDifficultyObject<'_>],
    ) -> f64 {
        let mut count = 0.0;
        let mut index = 0;
        let mut current_time_gap = 0.0;
        let mut prev0 = Some(curr);
        let mut prev1: Option<&OsuDifficultyObject<'_>> = None;
        let mut prev2: Option<&OsuDifficultyObject<'_>> = None;

        while current_time_gap < 2_000.0 {
            let Some(previous) = curr.previous(index, objects) else {
                break;
            };
            let long_interval_factor =
                1.0 - reverse_lerp(previous.adjusted_delta_time, 200.0, 2_000.0);

            if let (Some(current_angle), Some(previous_angle)) = (curr.angle, previous.angle) {
                let angle_difference = (current_angle - previous_angle).abs();
                let mut alternating_difference = PI;

                if let (Some(prev0_angle), Some(prev1_angle), Some(prev2_angle)) = (
                    prev0.and_then(|object| object.angle),
                    prev1.and_then(|object| object.angle),
                    prev2.and_then(|object| object.angle),
                ) {
                    alternating_difference =
                        (prev1_angle - previous_angle).abs() + (prev2_angle - prev0_angle).abs();
                    let weight =
                        reverse_lerp(previous_angle.min(prev0_angle).to_degrees(), 20.0, 5.0)
                            * reverse_lerp(
                                previous_angle.max(prev0_angle).to_degrees(),
                                60.0,
                                120.0,
                            );
                    alternating_difference = PI + (0.1 * alternating_difference - PI) * weight;
                }

                let stack_factor = smootherstep(
                    previous.lazy_jump_dist,
                    0.0,
                    OsuDifficultyObject::NORMALIZED_RADIUS as f64,
                );
                count += (3.0
                    * (PI / 6.0).min(angle_difference.min(alternating_difference) * stack_factor))
                .cos()
                    * long_interval_factor;
            }

            current_time_gap = curr.start_time - previous.start_time;
            index += 1;
            prev2 = prev1;
            prev1 = prev0;
            prev0 = Some(previous);
        }

        (2.0 / count).clamp(0.2, 1.0)
    }

    fn time_nerf(delta_time: f64) -> f64 {
        (2.0 - delta_time / (Self::READING_WINDOW_SIZE / 2.0)).clamp(0.0, 1.0)
    }

    fn harmonic_difficulty(&self) -> (f64, f64) {
        let mut difficulties: Vec<f64> = self
            .object_difficulties
            .iter()
            .copied()
            .filter(|&value| value > 0.0)
            .collect();

        let reduced_note_count = self.object_start_times.first().map_or(0, |&first| {
            let end = first + Self::REDUCED_DIFFICULTY_DURATION;
            self.object_start_times
                .iter()
                .take_while(|&&start_time| start_time <= end)
                .count()
        });

        for (index, difficulty) in difficulties.iter_mut().take(reduced_note_count).enumerate() {
            let scale =
                (1.0 + 9.0 * (index as f64 / reduced_note_count as f64).clamp(0.0, 1.0)).log10();
            *difficulty *= scale;
        }

        difficulties.sort_unstable_by(|a, b| b.total_cmp(a));

        difficulties.into_iter().enumerate().fold(
            (0.0, 0.0),
            |(difficulty, weight_sum), (index, value)| {
                let index = index as f64;
                let harmonic = 1.0 / (1.0 + index);
                let weight = (1.0 + harmonic) / (index.powf(0.9) + 1.0 + harmonic);
                (difficulty + value * weight, weight_sum + weight)
            },
        )
    }

    pub fn cloned_difficulty_value(&self) -> f64 {
        self.harmonic_difficulty().0
    }

    pub fn count_top_weighted_notes(&self, difficulty_value: f64) -> f64 {
        if self.object_difficulties.is_empty() {
            return 0.0;
        }

        let weight_sum = self.harmonic_difficulty().1;
        if weight_sum == 0.0 {
            return 0.0;
        }

        let consistent_top_note = difficulty_value / weight_sum;
        if consistent_top_note == 0.0 {
            return 0.0;
        }

        self.object_difficulties
            .iter()
            .map(|&value| logistic(value / consistent_top_note, 1.15, 5.0, Some(1.1)))
            .sum()
    }

    pub fn current_strain_peaks(&self) -> Vec<f64> {
        let mut peaks = self.strain_peaks.clone();
        peaks.push(self.current_section_peak);

        peaks
    }

    pub fn into_current_strain_peaks(mut self) -> Vec<f64> {
        self.strain_peaks.push(self.current_section_peak);

        self.strain_peaks
    }

    pub fn difficulty_to_performance(difficulty: f64) -> f64 {
        4.0 * difficulty.powi(3)
    }
}
