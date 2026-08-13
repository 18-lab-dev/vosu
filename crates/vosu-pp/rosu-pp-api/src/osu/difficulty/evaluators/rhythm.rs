use std::cmp;

use crate::{
    any::difficulty::object::IDifficultyObject,
    osu::difficulty::object::OsuDifficultyObject,
    util::difficulty::{logistic, reverse_lerp, smoothstep_bell_curve},
};

pub struct RhythmEvaluator;

impl RhythmEvaluator {
    const HISTORY_TIME_MAX: u32 = 5 * 1000;
    const HISTORY_OBJECTS_MAX: usize = 32;
    const RHYTHM_OVERALL_MULTIPLIER: f64 = 0.95;
    const RHYTHM_RATIO_MULTIPLIER: f64 = 26.0;

    #[expect(clippy::too_many_lines, reason = "staying in-sync with lazer")]
    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        hit_window: f64,
    ) -> f64 {
        if curr.base.is_spinner() {
            return 0.0;
        }

        let mut rhythm_complexity_sum = 0.0;
        let delta_difference_eps = hit_window * 0.3;
        let mut island = RhythmIsland::new(i32::MAX, delta_difference_eps);
        let mut previous_island = RhythmIsland::new(i32::MAX, delta_difference_eps);
        let mut islands = Vec::<IslandCount>::new();
        let mut start_difficulty = 0.0;
        let mut first_delta_switch = false;
        let historical_note_count = cmp::min(curr.idx, Self::HISTORY_OBJECTS_MAX);
        let mut rhythm_start = 0;

        while rhythm_start + 2 < historical_note_count
            && curr
                .previous(rhythm_start, diff_objects)
                .is_some_and(|prev| {
                    curr.start_time - prev.start_time < f64::from(Self::HISTORY_TIME_MAX)
                })
        {
            rhythm_start += 1;
        }

        if let Some((mut prev_obj, mut prev_prev_obj)) = curr
            .previous(rhythm_start, diff_objects)
            .zip(curr.previous(rhythm_start + 1, diff_objects))
        {
            for i in (1..=rhythm_start).rev() {
                let Some(curr_obj) = curr.previous(i - 1, diff_objects) else {
                    break;
                };

                if curr_obj.base.is_spinner() {
                    continue;
                }

                let time_decay = (f64::from(Self::HISTORY_TIME_MAX)
                    - (curr.start_time - curr_obj.start_time))
                    / f64::from(Self::HISTORY_TIME_MAX);
                let note_decay = (historical_note_count - i) as f64 / historical_note_count as f64;
                let curr_historical_decay = note_decay.min(time_decay);
                let curr_delta = curr_obj.delta_time.max(1e-7);
                let prev_delta = prev_obj.delta_time.max(1e-7);
                let delta_difference = (prev_delta - curr_delta).abs();

                if island.delta == i32::MAX {
                    island = RhythmIsland::new(curr_delta as i32, delta_difference_eps);
                }

                let delta_difference_ratio =
                    prev_delta.max(curr_delta) / prev_delta.min(curr_delta);
                let difference_multiplier = (2.0 - delta_difference_ratio / 8.0).clamp(0.0, 1.0);
                let window_penalty = ((delta_difference - delta_difference_eps)
                    / delta_difference_eps)
                    .clamp(0.0, 1.0);
                let mut effective_difficulty = Self::effective_difficulty(delta_difference_ratio)
                    * window_penalty
                    * difference_multiplier;

                if prev_obj.base.is_slider() {
                    let slider_lazy_end_delta = curr_obj.min_jump_time;
                    let slider_lazy_ratio = slider_lazy_end_delta.max(curr_delta)
                        / slider_lazy_end_delta.min(curr_delta);
                    let slider_real_end_delta = (curr_obj.start_time - prev_obj.end_time)
                        .max(OsuDifficultyObject::MIN_DELTA_TIME);
                    let slider_real_ratio = slider_real_end_delta.max(curr_delta)
                        / slider_real_end_delta.min(curr_delta);
                    let slider_effective_difficulty = Self::effective_difficulty(slider_lazy_ratio)
                        .min(Self::effective_difficulty(slider_real_ratio));
                    effective_difficulty = effective_difficulty.min(slider_effective_difficulty);
                }

                if delta_difference < delta_difference_eps {
                    island.add_delta(curr_delta as i32);
                }

                if first_delta_switch {
                    if delta_difference > delta_difference_eps {
                        if curr_obj.base.is_slider() {
                            effective_difficulty *= 0.5;
                        }

                        if island.is_similar_polarity(&previous_island) {
                            effective_difficulty *= 0.5;
                        }

                        if prev_prev_obj.delta_time.max(1e-7) > prev_delta + delta_difference_eps
                            && prev_delta > curr_delta + delta_difference_eps
                        {
                            effective_difficulty *= 0.125;
                        }

                        if previous_island.delta_count == island.delta_count {
                            effective_difficulty *= 0.5;
                        }

                        if prev_delta > curr_delta + delta_difference_eps {
                            effective_difficulty *= 0.65;
                        }

                        if let Some(existing) =
                            islands.iter_mut().find(|entry| entry.island == island)
                        {
                            if previous_island == island {
                                existing.occurrences += 1;
                            }

                            let power = logistic(f64::from(island.delta), 58.33, 0.24, Some(2.75));
                            effective_difficulty *= (3.0 / existing.occurrences as f64)
                                .min((existing.occurrences as f64).recip().powf(power));
                        } else if island.delta_count > 0 {
                            islands.push(IslandCount {
                                island,
                                occurrences: 1,
                            });
                        }

                        effective_difficulty *=
                            1.0 - prev_obj.get_doubletapness(Some(curr_obj), hit_window) * 0.75;

                        if island.delta_count > 1 {
                            rhythm_complexity_sum += (effective_difficulty * start_difficulty)
                                .sqrt()
                                * curr_historical_decay;
                        } else {
                            rhythm_complexity_sum += 0.7 * curr_historical_decay;
                        }

                        start_difficulty = effective_difficulty;

                        if prev_delta + delta_difference_eps < curr_delta {
                            first_delta_switch = false;
                        }

                        previous_island = island;
                        island = RhythmIsland::new(curr_delta as i32, delta_difference_eps);
                    }
                } else if prev_delta > curr_delta + delta_difference_eps {
                    first_delta_switch = true;

                    if curr_obj.base.is_slider() {
                        effective_difficulty *= 0.6;
                    }

                    if prev_obj.base.is_slider() {
                        effective_difficulty *= 0.6;
                    }

                    start_difficulty = effective_difficulty;
                    island = RhythmIsland::new(curr_delta as i32, delta_difference_eps);
                }

                prev_prev_obj = prev_obj;
                prev_obj = curr_obj;
            }
        }

        rhythm_complexity_sum *= reverse_lerp(f64::from(island.delta_count), 22.0, 3.0);

        (4.0 + rhythm_complexity_sum * Self::RHYTHM_OVERALL_MULTIPLIER).sqrt() / 2.0
    }

    fn effective_difficulty(delta_difference_ratio: f64) -> f64 {
        let fraction = delta_difference_ratio - delta_difference_ratio.trunc();

        1.0 + Self::RHYTHM_RATIO_MULTIPLIER * smoothstep_bell_curve(fraction, 0.5, 0.5).min(0.5)
    }
}

#[derive(Copy, Clone)]
struct RhythmIsland {
    delta_difference_eps: f64,
    delta: i32,
    delta_count: i32,
}

impl RhythmIsland {
    fn new(delta: i32, delta_difference_eps: f64) -> Self {
        Self {
            delta_difference_eps,
            delta: cmp::max(delta, OsuDifficultyObject::MIN_DELTA_TIME as i32),
            delta_count: 1,
        }
    }

    fn add_delta(&mut self, delta: i32) {
        if self.delta == i32::MAX {
            self.delta = cmp::max(delta, OsuDifficultyObject::MIN_DELTA_TIME as i32);
        }

        self.delta_count += 1;
    }

    fn is_similar_polarity(&self, other: &Self) -> bool {
        self.delta_count > 1
            && other.delta_count > 1
            && f64::from((self.delta - other.delta).abs()) < self.delta_difference_eps
            && self.delta_count % 2 == other.delta_count % 2
    }
}

impl PartialEq for RhythmIsland {
    fn eq(&self, other: &Self) -> bool {
        f64::from((self.delta - other.delta).abs()) < self.delta_difference_eps
            && self.delta_count == other.delta_count
    }
}

struct IslandCount {
    island: RhythmIsland,
    occurrences: usize,
}
