use std::cmp;

use crate::{
    any::difficulty::object::IDifficultyObject,
    osu::difficulty::object::OsuDifficultyObject,
    util::difficulty::{logistic, reverse_lerp, smoothstep_bell_curve},
};

pub struct RhythmEvaluator;

impl RhythmEvaluator {
    const HISTORY_TIME_MAX: u32 = 5 * 1000; // 5 seconds
    const HISTORY_OBJECTS_MAX: usize = 32;
    const RHYTHM_OVERALL_MULTIPLIER: f64 = 0.95;
    const RHYTHM_RATIO_DIFFICULTY_MULTIPLIER: f64 = 26.0;

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

        let mut island = RhythmIsland::new(i32::MAX);
        let mut previous_island = RhythmIsland::new(i32::MAX);
        let mut islands = Vec::<RhythmIsland>::new();

        // Store the difficulty of the current island's start to buff tighter rhythms.
        let mut start_difficulty = 0.0;
        let mut first_delta_switch = false;

        let historical_note_count = cmp::min(curr.idx, Self::HISTORY_OBJECTS_MAX);
        let mut rhythm_start = 0;

        while curr
            .previous(rhythm_start, diff_objects)
            .filter(|previous| {
                rhythm_start + 2 < historical_note_count
                    && curr.start_time - previous.start_time < f64::from(Self::HISTORY_TIME_MAX)
            })
            .is_some()
        {
            rhythm_start += 1;
        }

        if let Some((mut previous_object, mut previous_previous_object)) = curr
            .previous(rhythm_start, diff_objects)
            .zip(curr.previous(rhythm_start + 1, diff_objects))
        {
            // Go from the furthest object back to the current one.
            for i in (1..=rhythm_start).rev() {
                let Some(current_object) = curr.previous(i - 1, diff_objects) else {
                    break;
                };

                if current_object.base.is_spinner() {
                    continue;
                }

                // Scale each note from 0 to 1 from history to now.
                let time_decay = (f64::from(Self::HISTORY_TIME_MAX)
                    - (curr.start_time - current_object.start_time))
                    / f64::from(Self::HISTORY_TIME_MAX);
                let note_decay = (historical_note_count - i) as f64 / historical_note_count as f64;
                // Either time or object count limits the history.
                let current_historical_decay = note_decay.min(time_decay);

                // Use a custom cap to ensure the delta time is not actually zero here.
                const DELTA_MIN_VALUE: f64 = 1e-7;

                let current_delta = current_object.delta_time.max(DELTA_MIN_VALUE);
                let previous_delta = previous_object.delta_time.max(DELTA_MIN_VALUE);
                let delta_difference = (previous_delta - current_delta).abs();

                // Always ensure that the current island is initialised.
                if island.delta == i32::MAX {
                    island = RhythmIsland::new(current_delta as i32);
                }

                // Reduce rhythm bonuses for deltas that are multiples of each other.
                let delta_difference_ratio =
                    previous_delta.max(current_delta) / previous_delta.min(current_delta);

                // Reduce the ratio bonus when the delta difference is too large.
                let difference_multiplier = (2.0 - delta_difference_ratio / 8.0).clamp(0.0, 1.0);
                let window_penalty = ((delta_difference - delta_difference_eps)
                    / delta_difference_eps)
                    .clamp(0.0, 1.0);
                let mut effective_difficulty = Self::effective_difficulty(delta_difference_ratio)
                    * window_penalty
                    * difference_multiplier;

                // A slider followed by circles can be easier to tap because the slider does not
                // require a complete tapping motion. Evaluate both its lazy and real endpoints.
                if previous_object.base.is_slider() {
                    let slider_lazy_end_delta = current_object.min_jump_time;
                    let slider_lazy_delta_difference_ratio = slider_lazy_end_delta
                        .max(current_delta)
                        / slider_lazy_end_delta.min(current_delta);

                    let slider_real_end_delta = current_object.last_object_end_delta_time;
                    let slider_real_delta_difference_ratio = slider_real_end_delta
                        .max(current_delta)
                        / slider_real_end_delta.min(current_delta);

                    let slider_effective_difficulty =
                        Self::effective_difficulty(slider_lazy_delta_difference_ratio).min(
                            Self::effective_difficulty(slider_real_delta_difference_ratio),
                        );

                    effective_difficulty = slider_effective_difficulty.min(effective_difficulty);
                }

                if delta_difference < delta_difference_eps {
                    // The island is still progressing.
                    island.add_delta(current_delta as i32);
                }

                if first_delta_switch {
                    if delta_difference > delta_difference_eps {
                        // A BPM change into a slider has an easy accuracy window.
                        if current_object.base.is_slider() {
                            effective_difficulty *= 0.5;
                        }

                        // Repeated island polarity (2 -> 4, 3 -> 5).
                        if island.is_similar_polarity(&previous_island, delta_difference_eps) {
                            effective_difficulty *= 0.5;
                        }

                        // The previous increase happened one note ago (1/1 -> 1/2 -> 1/4).
                        if previous_previous_object.delta_time.max(DELTA_MIN_VALUE)
                            > previous_delta + delta_difference_eps
                            && previous_delta > current_delta + delta_difference_eps
                        {
                            effective_difficulty *= 0.125;
                        }

                        // Repeated island size (for example, triplet -> triplet).
                        // TODO: Remove this nerf once the flawed ratio calculation no longer
                        // requires it for balancing.
                        if previous_island.delta_count == island.delta_count {
                            effective_difficulty *= 0.5;
                        }

                        let is_speeding_up = previous_delta > current_delta + delta_difference_eps;

                        if is_speeding_up {
                            effective_difficulty *= 0.65;
                        }

                        let mut found = false;

                        for existing_island in &mut islands {
                            if existing_island.almost_equals(&island, delta_difference_eps) {
                                // Only increase occurrences for consecutive matching islands.
                                if previous_island.almost_equals(&island, delta_difference_eps) {
                                    existing_island.occurrences += 1;
                                }

                                // Repeated island (for example, triplet -> triplet).
                                let power =
                                    logistic(f64::from(island.delta), 58.33, 0.24, Some(2.75));
                                let occurrences = f64::from(existing_island.occurrences);
                                effective_difficulty *=
                                    (3.0 / occurrences).min((1.0 / occurrences).powf(power));

                                found = true;
                                break;
                            }
                        }

                        if !found && island.delta_count > 0 {
                            islands.push(island);
                        }

                        // Scale down difficulty when the object is double-tappable.
                        effective_difficulty *= 1.0
                            - previous_object.get_doubletapness(Some(current_object), hit_window)
                                * 0.75;

                        if island.delta_count > 1 {
                            rhythm_complexity_sum += (effective_difficulty * start_difficulty)
                                .sqrt()
                                * current_historical_decay;
                        } else {
                            // Constant difficulty for single-note islands.
                            rhythm_complexity_sum += 0.7 * current_historical_decay;
                        }

                        start_difficulty = effective_difficulty;

                        if previous_delta + delta_difference_eps < current_delta {
                            // Slowing down stops the count; while speeding up, it remains active.
                            first_delta_switch = false;
                        }

                        previous_island = island;
                        island = RhythmIsland::new(current_delta as i32);
                    }
                } else if previous_delta > current_delta + delta_difference_eps {
                    // Begin counting the island when speeding up, until speed changes again.
                    first_delta_switch = true;

                    // A BPM change into a slider has an easy accuracy window.
                    if current_object.base.is_slider() {
                        effective_difficulty *= 0.6;
                    }

                    // A BPM change from a slider is typically easier than circle -> circle.
                    if previous_object.base.is_slider() {
                        effective_difficulty *= 0.6;
                    }

                    start_difficulty = effective_difficulty;
                    island = RhythmIsland::new(current_delta as i32);
                }

                previous_previous_object = previous_object;
                previous_object = current_object;
            }
        }

        // Long current islands should have a smaller effect on the sum.
        rhythm_complexity_sum *= reverse_lerp(f64::from(island.delta_count), 22.0, 3.0);

        // Produces a strain multiplier in the range [1, infinity) (not really though).
        (4.0 + rhythm_complexity_sum * Self::RHYTHM_OVERALL_MULTIPLIER).sqrt() / 2.0
    }

    #[inline(always)]
    fn effective_difficulty(delta_difference_ratio: f64) -> f64 {
        // Take only the fractional part because only multiples should be punished.
        let delta_difference_fraction = delta_difference_ratio - delta_difference_ratio.trunc();

        1.0 + Self::RHYTHM_RATIO_DIFFICULTY_MULTIPLIER
            * smoothstep_bell_curve(delta_difference_fraction, 0.5, 0.5).min(0.5)
    }
}

#[derive(Copy, Clone)]
struct RhythmIsland {
    delta: i32,
    delta_count: i32,
    occurrences: i32,
}

const MIN_DELTA_TIME: i32 = 25;

// Compile-time check in case `OsuDifficultyObject::MIN_DELTA_TIME` changes
// but this corresponding integer constant is not updated.
const _: [(); 0 - !{ MIN_DELTA_TIME - OsuDifficultyObject::MIN_DELTA_TIME as i32 == 0 } as usize] =
    [];

impl RhythmIsland {
    fn new(delta: i32) -> Self {
        Self {
            delta: cmp::max(delta, MIN_DELTA_TIME),
            delta_count: 1,
            occurrences: 1,
        }
    }

    fn add_delta(&mut self, delta: i32) {
        if self.delta == i32::MAX {
            self.delta = cmp::max(delta, MIN_DELTA_TIME);
        }

        self.delta_count += 1;
    }

    fn is_similar_polarity(&self, other: &Self, epsilon: f64) -> bool {
        // Single-delta islands should not be compared.
        if self.delta_count <= 1 || other.delta_count <= 1 {
            return false;
        }

        f64::from((self.delta - other.delta).abs()) < epsilon
            && self.delta_count % 2 == other.delta_count % 2
    }

    fn almost_equals(&self, other: &Self, epsilon: f64) -> bool {
        f64::from((self.delta - other.delta).abs()) < epsilon
            && self.delta_count == other.delta_count
    }
}
