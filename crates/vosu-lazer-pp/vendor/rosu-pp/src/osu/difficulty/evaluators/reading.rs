use std::f64::consts::PI;

use crate::{
    any::difficulty::object::IDifficultyObject,
    osu::{difficulty::object::OsuDifficultyObject, object::OsuObject},
    util::{
        difficulty::{norm, reverse_lerp, smootherstep},
        float_ext::FloatExt,
    },
};

use super::super::{HD_FADE_IN_DURATION_MULTIPLIER, HD_FADE_OUT_DURATION_MULTIPLIER};

pub struct ReadingEvaluator;

impl ReadingEvaluator {
    const READING_WINDOW_SIZE: f64 = 3000.0;
    const DISTANCE_INFLUENCE_THRESHOLD: f64 = OsuDifficultyObject::NORMALIZED_DIAMETER as f64 * 1.5;

    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        hidden: bool,
        _time_preempt: f64,
        clock_rate: f64,
    ) -> f64 {
        if curr.base.is_spinner() || curr.idx == 0 {
            return 0.0;
        }

        let next_obj = curr.next(0, diff_objects);
        let time_preempt = curr.base.time_preempt;
        let adjusted_time_preempt = time_preempt / clock_rate;

        // Only allow velocity to buff.
        let velocity = (curr.lazy_jump_dist / curr.adjusted_delta_time).max(1.0);

        let current_visible_object_density =
            Self::retrieve_current_visible_object_density(curr, diff_objects, clock_rate);
        let past_object_difficulty_influence =
            Self::get_past_object_difficulty_influence(curr, diff_objects, adjusted_time_preempt);

        let constant_angle_nerf_factor = Self::get_constant_angle_nerf_factor(curr, diff_objects);

        let note_density_difficulty = Self::calculate_density_difficulty(
            next_obj,
            velocity,
            constant_angle_nerf_factor,
            past_object_difficulty_influence,
            current_visible_object_density,
        );

        let hidden_difficulty = if hidden {
            Self::calculate_hidden_difficulty(
                curr,
                diff_objects,
                past_object_difficulty_influence,
                current_visible_object_density,
                velocity,
                constant_angle_nerf_factor,
                adjusted_time_preempt,
            )
        } else {
            0.0
        };

        let preempt_difficulty = Self::calculate_preempt_difficulty(
            velocity,
            constant_angle_nerf_factor,
            adjusted_time_preempt,
        );

        let reading_difficulty = norm(
            1.5,
            [
                preempt_difficulty,
                hidden_difficulty,
                note_density_difficulty,
            ],
        );

        // Having less time to process information is harder.
        reading_difficulty * Self::high_bpm_bonus(curr.adjusted_delta_time)
    }

    /// Calculates the density difficulty of the current object and how hard it is to aim it
    /// because of cursor velocity, repeated angles, and visible-object density.
    fn calculate_density_difficulty(
        next_obj: Option<&OsuDifficultyObject<'_>>,
        velocity: f64,
        constant_angle_nerf_factor: f64,
        past_object_difficulty_influence: f64,
        current_visible_object_density: f64,
    ) -> f64 {
        const DENSITY_MULTIPLIER: f64 = 2.4;
        const DENSITY_DIFFICULTY_BASE: f64 = 2.5;

        // Consider future densities too because they can make the cursor path less clear.
        let mut future_object_difficulty_influence = current_visible_object_density.sqrt();

        if let Some(next_obj) = next_obj {
            // Reduce difficulty if movement to the next object is small.
            future_object_difficulty_influence *= smootherstep(
                next_obj.lazy_jump_dist,
                15.0,
                Self::DISTANCE_INFLUENCE_THRESHOLD,
            );
        }

        // Value higher note densities exponentially.
        let mut note_density_difficulty = f64::powf(
            past_object_difficulty_influence + future_object_difficulty_influence,
            1.7,
        ) * 0.4
            * constant_angle_nerf_factor
            * velocity;

        // Award only denser-than-average maps.
        note_density_difficulty = (note_density_difficulty - DENSITY_DIFFICULTY_BASE).max(0.0);

        // Apply a soft cap to general density reading to account for partial memorization.
        f64::powf(note_density_difficulty, 0.45) * DENSITY_MULTIPLIER
    }

    /// Calculates the difficulty of aiming the current object at very high approach rates.
    fn calculate_preempt_difficulty(
        velocity: f64,
        constant_angle_nerf_factor: f64,
        preempt: f64,
    ) -> f64 {
        const PREEMPT_BALANCING_FACTOR: f64 = 140_000.0;
        const PREEMPT_STARTING_POINT: f64 = 500.0;

        // Arbitrary curve for the base preempt difficulty as approach rate increases.
        // https://www.desmos.com/calculator/c175335a71
        let mut preempt_difficulty = f64::powf(
            (PREEMPT_STARTING_POINT - preempt + (preempt - PREEMPT_STARTING_POINT).abs()) / 2.0,
            2.5,
        ) / PREEMPT_BALANCING_FACTOR;

        preempt_difficulty *= constant_angle_nerf_factor * velocity;

        preempt_difficulty
    }

    /// Calculates the difficulty of aiming the current object while Hidden is active.
    #[expect(clippy::too_many_arguments, reason = "staying in-sync with lazer")]
    fn calculate_hidden_difficulty<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        past_object_difficulty_influence: f64,
        current_visible_object_density: f64,
        velocity: f64,
        constant_angle_nerf_factor: f64,
        adjusted_time_preempt: f64,
    ) -> f64 {
        const HIDDEN_MULTIPLIER: f64 = 0.28;

        // Higher preempt means that time spent invisible is higher too, so reward it.
        let preempt_factor = f64::powf(adjusted_time_preempt, 2.2) * 0.01;

        // Account for both past and current densities.
        let density_factor = f64::powf(
            current_visible_object_density + past_object_difficulty_influence,
            3.3,
        ) * 3.0;

        let mut hidden_difficulty =
            (preempt_factor + density_factor) * constant_angle_nerf_factor * velocity * 0.01;

        // Apply a soft cap to general HD reading to account for partial memorization.
        hidden_difficulty = f64::powf(hidden_difficulty, 0.4) * HIDDEN_MULTIPLIER;

        if let Some(previous_obj) = curr.previous(0, diff_objects) {
            // Buff perfect stacks only if the current note is completely invisible when the
            // previous note is clicked.
            if curr.lazy_jump_dist == 0.0
                && Self::opacity_at(curr, previous_obj.base.start_time, true) == 0.0
                && previous_obj.start_time > curr.start_time - adjusted_time_preempt
            {
                // Perfect stacks are harder the less time there is between notes.
                hidden_difficulty +=
                    HIDDEN_MULTIPLIER * 2500.0 / f64::powf(curr.adjusted_delta_time, 1.5);
            }
        }

        hidden_difficulty
    }

    fn get_past_object_difficulty_influence<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        adjusted_time_preempt: f64,
    ) -> f64 {
        let mut past_object_difficulty_influence = 0.0;

        for loop_obj in
            Self::retrieve_past_visible_objects(curr, diff_objects, adjusted_time_preempt)
        {
            let mut loop_difficulty = Self::opacity_at(curr, loop_obj.base.start_time, false);

            // With small distances, previous objects may be cheesed, so their confusing
            // arrangement matters less.
            loop_difficulty *= smootherstep(
                loop_obj.lazy_jump_dist,
                15.0,
                Self::DISTANCE_INFLUENCE_THRESHOLD,
            );

            // Account less for objects close to the maximum reading window.
            let time_between_curr_and_loop_obj = curr.start_time - loop_obj.start_time;
            let time_nerf_factor = Self::get_time_nerf_factor(time_between_curr_and_loop_obj);

            loop_difficulty *= time_nerf_factor;
            past_object_difficulty_influence += loop_difficulty;
        }

        past_object_difficulty_influence
    }

    /// Returns objects visible when the current object becomes visible.
    fn retrieve_past_visible_objects<'a>(
        current: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        adjusted_time_preempt: f64,
    ) -> impl Iterator<Item = &'a OsuDifficultyObject<'a>> + 'a {
        (0..current.idx).map_while(move |i| {
            let hit_object = current.previous(i, diff_objects)?;

            if current.start_time - hit_object.start_time > Self::READING_WINDOW_SIZE
                || hit_object.start_time < current.start_time - adjusted_time_preempt
            {
                return None;
            }

            Some(hit_object)
        })
    }

    /// Returns the density of objects visible when the current object must be clicked, capped by
    /// the reading window.
    fn retrieve_current_visible_object_density<'a>(
        current: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        clock_rate: f64,
    ) -> f64 {
        let mut visible_object_count = 0.0;
        let mut hit_object = current.next(0, diff_objects);

        while let Some(object) = hit_object {
            if object.start_time - current.start_time > Self::READING_WINDOW_SIZE
                || current.start_time < object.start_time - object.base.time_preempt / clock_rate
            {
                break;
            }

            let time_between_curr_and_loop_obj = object.start_time - current.start_time;
            let time_nerf_factor = Self::get_time_nerf_factor(time_between_curr_and_loop_obj);

            visible_object_count +=
                Self::opacity_at(object, current.base.start_time, false) * time_nerf_factor;

            hit_object = object.next(0, diff_objects);
        }

        visible_object_count
    }

    /// Returns a factor for how often the current object's angle has repeated recently.
    fn get_constant_angle_nerf_factor<'a>(
        current: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
    ) -> f64 {
        const MINIMUM_ANGLE_RELEVANCY_TIME: f64 = 2000.0;
        const MAXIMUM_ANGLE_RELEVANCY_TIME: f64 = 200.0;

        let mut constant_angle_count = 0.0;
        let mut index = 0;
        let mut current_time_gap = 0.0;

        let mut loop_obj_prev_0 = current;
        let mut loop_obj_prev_1: Option<&OsuDifficultyObject<'_>> = None;
        let mut loop_obj_prev_2: Option<&OsuDifficultyObject<'_>> = None;

        while current_time_gap < MINIMUM_ANGLE_RELEVANCY_TIME {
            let Some(loop_obj) = current.previous(index, diff_objects) else {
                break;
            };

            // Account less for objects close to the time limit.
            let long_interval_factor = 1.0
                - reverse_lerp(
                    loop_obj.adjusted_delta_time,
                    MAXIMUM_ANGLE_RELEVANCY_TIME,
                    MINIMUM_ANGLE_RELEVANCY_TIME,
                );

            if let (Some(loop_angle), Some(current_angle)) = (loop_obj.angle, current.angle) {
                let angle_difference = (current_angle - loop_angle).abs();
                let mut angle_difference_alternating = PI;

                if let (Some(prev_0_angle), Some(prev_1_angle), Some(prev_2_angle)) = (
                    loop_obj_prev_0.angle,
                    loop_obj_prev_1.and_then(|object| object.angle),
                    loop_obj_prev_2.and_then(|object| object.angle),
                ) {
                    angle_difference_alternating = (prev_1_angle - loop_angle).abs();
                    angle_difference_alternating += (prev_2_angle - prev_0_angle).abs();

                    let mut weight = 1.0;

                    // Be sure that one angle is very sharp while the other is wide.
                    weight *= reverse_lerp(loop_angle.min(prev_0_angle) * 180.0 / PI, 20.0, 5.0);
                    weight *= reverse_lerp(loop_angle.max(prev_0_angle) * 180.0 / PI, 60.0, 120.0);

                    // Lerp between maximum angle difference and the rescaled alternating
                    // difference, with harsher scaling than the normal difference.
                    angle_difference_alternating =
                        f64::lerp(PI, 0.1 * angle_difference_alternating, weight);
                }

                let stack_factor = smootherstep(
                    loop_obj.lazy_jump_dist,
                    0.0,
                    OsuDifficultyObject::NORMALIZED_RADIUS as f64,
                );

                constant_angle_count += f64::cos(
                    3.0 * f64::to_radians(30.0)
                        .min(angle_difference.min(angle_difference_alternating) * stack_factor),
                ) * long_interval_factor;
            }

            current_time_gap = current.start_time - loop_obj.start_time;
            index += 1;

            loop_obj_prev_2 = loop_obj_prev_1;
            loop_obj_prev_1 = Some(loop_obj_prev_0);
            loop_obj_prev_0 = loop_obj;
        }

        (2.0 / constant_angle_count).clamp(0.2, 1.0)
    }

    /// Rust's existing `OsuDifficultyObject::opacity_at` receives a single fade-in duration,
    /// while lazer reading uses the unmodified fade-in duration together with Hidden's adjusted
    /// fade-out start. Keep the two values distinct here for exact parity.
    fn opacity_at(object: &OsuDifficultyObject<'_>, time: f64, hidden: bool) -> f64 {
        let time_preempt = object.base.time_preempt;
        if time > object.base.start_time {
            return 0.0;
        }

        let fade_in_start_time = object.base.start_time - time_preempt;
        let fade_in_duration = 400.0 * (time_preempt / OsuObject::PREEMPT_MIN).min(1.0);
        let fade_in = ((time - fade_in_start_time) / fade_in_duration).clamp(0.0, 1.0);

        if hidden {
            let fade_out_start_time = object.base.start_time - time_preempt
                + time_preempt * HD_FADE_IN_DURATION_MULTIPLIER;
            let fade_out_duration = time_preempt * HD_FADE_OUT_DURATION_MULTIPLIER;
            let fade_out = 1.0 - ((time - fade_out_start_time) / fade_out_duration).clamp(0.0, 1.0);

            fade_in.min(fade_out)
        } else {
            fade_in
        }
    }

    /// Returns a nerfing factor for objects distant in time.
    fn get_time_nerf_factor(delta_time: f64) -> f64 {
        (2.0 - delta_time / (Self::READING_WINDOW_SIZE / 2.0)).clamp(0.0, 1.0)
    }

    fn high_bpm_bonus(ms: f64) -> f64 {
        1.0 / (1.0 - f64::powf(0.8, ms / 1000.0))
    }
}
