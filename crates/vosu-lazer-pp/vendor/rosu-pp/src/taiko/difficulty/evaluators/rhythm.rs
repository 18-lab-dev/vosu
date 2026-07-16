use std::f64::consts::PI;

use crate::{
    taiko::{
        difficulty::{
            object::TaikoDifficultyObject,
            rhythm::data::same_rhythm_hit_object_grouping::SameRhythmHitObjectGrouping,
        },
        object::HitType,
    },
    util::{
        difficulty::{bell_curve, logistic, reverse_lerp},
        sync::RefCount,
    },
};

pub struct RhythmEvaluator;

impl RhythmEvaluator {
    pub fn evaluate_diff_of(hit_object: &TaikoDifficultyObject, hit_window: f64) -> f64 {
        if hit_object.base_hit_type == HitType::NonHit {
            return 0.0;
        }
        let rhythm_data = &hit_object.rhythm_data;
        let mut difficulty = 0.0;

        let mut same_rhythm = 0.0;
        let mut same_pattern = 0.0;
        let mut interval_penalty = 0.0;
        let mut gap_penalty = 0.0;

        // * Difficulty for SameRhythmGroupedHitObjects
        if let Some(ref same_rhythm_grouped) = rhythm_data.same_rhythm_grouped_hit_objects
            && same_rhythm_grouped
                .get()
                .first_hit_object()
                .is_some_and(|h| &*h.get() == hit_object)
        {
            let full_hit_window = hit_window * 2.0;
            same_rhythm += 10.0 * Self::evaluate_diff_of_(same_rhythm_grouped, full_hit_window);
            interval_penalty =
                Self::repeated_interval_penalty(same_rhythm_grouped, full_hit_window, None);
            gap_penalty =
                Self::long_gap_penalty(same_rhythm_grouped.get().upgraded_previous().as_ref());
        }

        // * Difficulty for SamePatternsGroupedHitObjects
        if let Some(ref same_pattern_grouped) = rhythm_data.same_patterns_grouped_hit_objects
            && same_pattern_grouped
                .get()
                .first_hit_object()
                .is_some_and(|h| &*h.get() == hit_object)
        {
            same_pattern +=
                1.15 * Self::ratio_difficulty(same_pattern_grouped.get().interval_ratio(), None);
        }

        difficulty += f64::max(same_rhythm, same_pattern) * interval_penalty * gap_penalty;

        difficulty
    }

    fn evaluate_diff_of_(
        same_rhythm_grouped_hit_objects: &RefCount<SameRhythmHitObjectGrouping>,
        hit_window: f64,
    ) -> f64 {
        let mut interval_diff = Self::ratio_difficulty(
            same_rhythm_grouped_hit_objects
                .get()
                .hit_object_interval_ratio,
            None,
        );
        let prev_interval = same_rhythm_grouped_hit_objects
            .get()
            .upgraded_previous()
            .and_then(|h| h.get().hit_object_interval);

        interval_diff *=
            Self::repeated_interval_penalty(same_rhythm_grouped_hit_objects, hit_window, None);

        let borrowed = same_rhythm_grouped_hit_objects.get();
        let duration = borrowed.duration();

        // * If a previous interval exists and there are multiple hit objects in the sequence:
        if let Some(prev_interval) = prev_interval.filter(|_| borrowed.hit_objects.len() > 1)
            && let Some(duration) = duration
        {
            let expected_duration_from_prev = prev_interval * borrowed.hit_objects.len() as f64;
            let duration_diff = duration - expected_duration_from_prev;

            if duration_diff > 0.0 {
                interval_diff *= logistic(duration_diff / hit_window, 0.35, 2.0, Some(1.0));
            }
        }

        // Penalise patterns that can be hit within a single hit window.
        if let Some(duration) = duration {
            interval_diff *= logistic(duration / hit_window, 0.3, 2.0, Some(1.0));
        }

        f64::powf(interval_diff, 0.75)
    }

    fn repeated_interval_penalty(
        same_rhythm_grouped_hit_objects: &RefCount<SameRhythmHitObjectGrouping>,
        hit_window: f64,
        threshold: Option<f64>,
    ) -> f64 {
        let threshold = threshold.unwrap_or(0.1);

        let same_interval =
            |start_object: RefCount<SameRhythmHitObjectGrouping>, interval_count: usize| -> f64 {
                let mut intervals = Vec::new();
                let mut curr_object = Some(start_object);

                let mut i = 0;

                while let Some(curr) = curr_object.filter(|_| i < interval_count) {
                    let curr = curr.get();

                    if let Some(interval) = curr.hit_object_interval {
                        intervals.push(interval);
                    }

                    curr_object = curr.upgraded_previous();
                    i += 1;
                }

                if intervals.len() < interval_count {
                    return 1.0; // * No penalty if there aren't enough valid intervals.
                }

                for i in 0..intervals.len() {
                    for j in i + 1..intervals.len() {
                        let ratio = intervals[i] / intervals[j];

                        // * If any two intervals are similar, apply a penalty.
                        if f64::abs(1.0 - ratio) <= threshold {
                            return 0.8;
                        }
                    }
                }

                // * No penalty if all intervals are different.
                1.0
            };

        let long_interval_penalty =
            same_interval(RefCount::clone(same_rhythm_grouped_hit_objects), 3);

        let short_interval_penalty = if same_rhythm_grouped_hit_objects.get().hit_objects.len() < 6
        {
            same_interval(RefCount::clone(same_rhythm_grouped_hit_objects), 4)
        } else {
            // * Returns a non-penalty if there are 6 or more notes within an interval.
            1.0
        };

        // * The duration penalty is based on hit object duration relative to hitWindow.
        let duration_penalty = same_rhythm_grouped_hit_objects
            .get()
            .duration()
            .map_or(0.5, |duration| {
                f64::max(1.0 - duration * 2.0 / hit_window, 0.5)
            });

        f64::min(long_interval_penalty, short_interval_penalty) * duration_penalty
    }

    fn long_gap_penalty(previous: Option<&RefCount<SameRhythmHitObjectGrouping>>) -> f64 {
        let Some(previous) = previous else {
            return 1.0;
        };

        let previous = previous.get();
        let Some(first) = previous.first_hit_object() else {
            return 1.0;
        };

        let gap_interval = first.get().delta_time;
        let rhythm_interval = previous.hit_object_interval.unwrap_or(gap_interval);
        let rhythm_length = previous.hit_objects.len() as f64;
        let gap_ratio = gap_interval / rhythm_interval.max(1.0);
        let gap_factor = logistic(gap_ratio, 1.75, 20.0, None);
        let length_factor = reverse_lerp(rhythm_length, 8.0, 2.0);

        1.0 - 0.75 * gap_factor * length_factor
    }

    fn ratio_difficulty(mut ratio: f64, terms: Option<i32>) -> f64 {
        let terms = terms.unwrap_or(8);
        let mut difficulty = 0.0;

        // * Validate the ratio by ensuring it is a normal number in cases where maps breach regular mapping conditions.
        ratio = if ratio.is_normal() { ratio } else { 0.0 };

        for i in 1..=terms {
            difficulty += Self::term_penalty(ratio, i, 4.0, 1.0);
        }

        difficulty += f64::from(terms) / (1.0 + ratio);

        // * Give bonus to near-1 ratios
        difficulty += bell_curve(ratio, 1.0, 0.5, None);

        // * Penalize ratios that are VERY near 1
        difficulty -= bell_curve(ratio, 1.0, 0.3, None);

        difficulty = f64::max(difficulty, 0.0);
        difficulty /= f64::sqrt(8.0);

        difficulty
    }

    fn term_penalty(ratio: f64, denominator: i32, power: f64, multiplier: f64) -> f64 {
        -multiplier * f64::powf(f64::cos(f64::from(denominator) * PI * ratio), power)
    }
}
