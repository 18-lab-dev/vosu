use crate::{
    any::difficulty::object::IDifficultyObject,
    osu::difficulty::object::OsuDifficultyObject,
    util::difficulty::{milliseconds_to_bpm, reverse_lerp, smootherstep, smoothstep},
};

pub struct SnapAimEvaluator;

impl SnapAimEvaluator {
    const WIDE_ANGLE_MULTIPLIER: f64 = 9.67;
    const ACUTE_ANGLE_MULTIPLIER: f64 = 2.41;
    const SLIDER_MULTIPLIER: f64 = 1.5;
    const VELOCITY_CHANGE_MULTIPLIER: f64 = 0.9;

    // Increasing this beyond 1.02 reduces difficulty as distance increases.
    const WIGGLE_MULTIPLIER: f64 = 1.02;

    #[allow(clippy::float_cmp, clippy::too_many_lines)]
    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        with_slider_travel_dist: bool,
    ) -> f64 {
        if curr.base.is_spinner() || curr.idx <= 1 {
            return 0.0;
        }

        let Some(osu_last_obj) = curr.previous(0, diff_objects) else {
            return 0.0;
        };

        if osu_last_obj.base.is_spinner() {
            return 0.0;
        }

        const RADIUS: f64 = OsuDifficultyObject::NORMALIZED_RADIUS as f64;
        const DIAMETER: f64 = OsuDifficultyObject::NORMALIZED_DIAMETER as f64;

        let osu_curr_obj = curr;
        let osu_last_2_obj = curr.previous(2, diff_objects);

        // Calculate the velocity to the current hitobject, starting with a base
        // distance / time that assumes the last object is a hitcircle.
        let curr_distance = if with_slider_travel_dist {
            osu_curr_obj.lazy_jump_dist
        } else {
            osu_curr_obj.jump_dist
        };
        let mut curr_velocity = curr_distance / osu_curr_obj.adjusted_delta_time;

        // If the last object is a slider, extend the travel velocity through the
        // slider into the current object.
        if osu_last_obj.base.is_slider() && with_slider_travel_dist {
            let slider_distance = osu_last_obj.lazy_travel_dist + osu_curr_obj.lazy_jump_dist;
            curr_velocity = curr_velocity.max(slider_distance / osu_curr_obj.adjusted_delta_time);
        }

        let prev_distance = if with_slider_travel_dist {
            osu_last_obj.lazy_jump_dist
        } else {
            osu_last_obj.jump_dist
        };
        let prev_velocity = prev_distance / osu_last_obj.adjusted_delta_time;

        let mut snap_difficulty = curr_velocity;

        // Penalize angle repetition.
        snap_difficulty *= Self::vector_angle_repetition(osu_curr_obj, osu_last_obj, diff_objects);

        if let Some((curr_angle, last_angle)) = osu_curr_obj.angle.zip(osu_last_obj.angle) {
            // Rewarding angles use the smaller velocity as their base.
            let velocity_influence = curr_velocity.min(prev_velocity);

            let mut acute_angle_bonus = 0.0;

            // Only reward acute angles when rhythms are the same.
            if osu_curr_obj
                .adjusted_delta_time
                .max(osu_last_obj.adjusted_delta_time)
                < 1.25
                    * osu_curr_obj
                        .adjusted_delta_time
                        .min(osu_last_obj.adjusted_delta_time)
            {
                acute_angle_bonus = Self::calc_angle_acuteness(curr_angle);

                // This repetition penalty must be applied before multiplying by
                // velocity because raw acuteness is compared here.
                acute_angle_bonus *= 0.08
                    + 0.92
                        * (1.0
                            - acute_angle_bonus
                                .min(Self::calc_angle_acuteness(last_angle).powi(3)));

                acute_angle_bonus *= velocity_influence
                    * smootherstep(
                        milliseconds_to_bpm(osu_curr_obj.adjusted_delta_time, Some(2)),
                        300.0,
                        400.0,
                    )
                    * smootherstep(curr_distance, 0.0, DIAMETER * 2.0);
            }

            let mut wide_angle_bonus = Self::calc_angle_wideness(curr_angle);

            // This repetition penalty must be applied before multiplying by
            // velocity because raw wideness is compared here.
            wide_angle_bonus *= 0.25
                + 0.75
                    * (1.0 - wide_angle_bonus.min(Self::calc_angle_wideness(last_angle).powi(3)));

            const WIDE_ANGLE_TIME_SCALE: f64 = 1.45;

            let mut wide_angle_curr_velocity =
                curr_distance / osu_curr_obj.adjusted_delta_time.powf(WIDE_ANGLE_TIME_SCALE);
            let wide_angle_prev_velocity =
                prev_distance / osu_last_obj.adjusted_delta_time.powf(WIDE_ANGLE_TIME_SCALE);

            if osu_last_obj.base.is_slider() && with_slider_travel_dist {
                let slider_distance = osu_last_obj.lazy_travel_dist + osu_curr_obj.lazy_jump_dist;
                wide_angle_curr_velocity = wide_angle_curr_velocity.max(
                    slider_distance / osu_curr_obj.adjusted_delta_time.powf(WIDE_ANGLE_TIME_SCALE),
                );
            }

            wide_angle_bonus *= wide_angle_curr_velocity.min(wide_angle_prev_velocity);

            if let Some(osu_last_2_obj) = osu_last_2_obj {
                // Objects going back and forth through a middle point receive less
                // wide-angle bonus. Previous(2) and Previous(0) are intentional:
                // the center point of an object's angle is its previous object.
                let distance = f64::from(
                    (osu_last_2_obj.base.stacked_pos() - osu_last_obj.base.stacked_pos()).length(),
                );

                if distance < 1.0 {
                    wide_angle_bonus *= 1.0 - 0.55 * (1.0 - distance);
                }
            }

            snap_difficulty += (acute_angle_bonus * Self::ACUTE_ANGLE_MULTIPLIER)
                .max(wide_angle_bonus * Self::WIDE_ANGLE_MULTIPLIER);

            // Reward wiggles for jumps in [radius, 3 * diameter] with angles
            // below 110 degrees.
            let wiggle_bonus = velocity_influence
                * smootherstep(curr_distance, RADIUS, DIAMETER)
                * reverse_lerp(curr_distance, DIAMETER * 3.0, DIAMETER).powf(1.8)
                * smootherstep(curr_angle, f64::to_radians(110.0), f64::to_radians(60.0))
                * smootherstep(prev_distance, RADIUS, DIAMETER)
                * reverse_lerp(prev_distance, DIAMETER * 3.0, DIAMETER).powf(1.8)
                * smootherstep(last_angle, f64::to_radians(110.0), f64::to_radians(60.0));

            snap_difficulty += wiggle_bonus * Self::WIGGLE_MULTIPLIER;
        }

        if prev_velocity.max(curr_velocity) != 0.0 {
            if with_slider_travel_dist {
                // Award velocity differences using only the object jump, without
                // slider velocity.
                curr_velocity = curr_distance / osu_curr_obj.adjusted_delta_time;
            }

            let dist_ratio = smoothstep(
                (prev_velocity - curr_velocity).abs() / prev_velocity.max(curr_velocity),
                0.0,
                1.0,
            );

            let overlap_velocity_buff = (DIAMETER * 1.25
                / osu_curr_obj
                    .adjusted_delta_time
                    .min(osu_last_obj.adjusted_delta_time))
            .min((prev_velocity - curr_velocity).abs());

            let mut velocity_change_bonus = overlap_velocity_buff * dist_ratio;

            let rhythm_ratio = osu_curr_obj
                .adjusted_delta_time
                .min(osu_last_obj.adjusted_delta_time)
                / osu_curr_obj
                    .adjusted_delta_time
                    .max(osu_last_obj.adjusted_delta_time);
            velocity_change_bonus *= rhythm_ratio.powi(2);

            snap_difficulty += velocity_change_bonus * Self::VELOCITY_CHANGE_MULTIPLIER;
        }

        // Reward sliders based on velocity.
        if osu_curr_obj.base.is_slider() && with_slider_travel_dist {
            let slider_bonus = osu_curr_obj.travel_dist / osu_curr_obj.travel_time;
            snap_difficulty += if slider_bonus < 1.0 {
                slider_bonus
            } else {
                slider_bonus.powf(0.75)
            } * Self::SLIDER_MULTIPLIER;
        }

        snap_difficulty *= osu_curr_obj.small_circle_bonus;
        snap_difficulty *= Self::high_bpm_bonus(osu_curr_obj.adjusted_delta_time);

        snap_difficulty
    }

    fn high_bpm_bonus(ms: f64) -> f64 {
        1.0 / (1.0 - 0.03_f64.powf((ms / 1000.0).powf(0.65)))
    }

    fn vector_angle_repetition<'a>(
        current: &'a OsuDifficultyObject<'a>,
        previous: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
    ) -> f64 {
        let Some((curr_angle, last_angle)) = current.angle.zip(previous.angle) else {
            return 1.0;
        };

        const NOTE_LIMIT: usize = 6;
        const MAXIMUM_REPETITION_NERF: f64 = 0.15;
        const MAXIMUM_VECTOR_INFLUENCE: f64 = 0.5;

        let mut constant_angle_count = 0.0;

        for index in 0..NOTE_LIMIT {
            let Some(prev_obj) = current.previous(index, diff_objects) else {
                break;
            };

            // Only consider vectors in the same jump section. A rhythm change
            // breaks the momentum.
            if current
                .adjusted_delta_time
                .max(prev_obj.adjusted_delta_time)
                > 1.1
                    * current
                        .adjusted_delta_time
                        .min(prev_obj.adjusted_delta_time)
            {
                break;
            }

            if let Some((prev_vector_angle, curr_vector_angle)) = prev_obj
                .normalised_vector_angle
                .zip(current.normalised_vector_angle)
            {
                let angle_difference = (curr_vector_angle - prev_vector_angle).abs();
                constant_angle_count += (8.0 * f64::to_radians(11.25).min(angle_difference)).cos();
            }
        }

        let vector_repetition = (0.5 / constant_angle_count).min(1.0).powi(2);

        let stack_factor = smootherstep(
            current.lazy_jump_dist,
            0.0,
            OsuDifficultyObject::NORMALIZED_DIAMETER as f64,
        );

        let angle_difference_adjusted =
            (2.0 * f64::to_radians(45.0).min((curr_angle - last_angle).abs() * stack_factor)).cos();

        let base_nerf = 1.0
            - MAXIMUM_REPETITION_NERF
                * Self::calc_angle_acuteness(last_angle)
                * angle_difference_adjusted;

        (base_nerf
            + (1.0 - base_nerf) * vector_repetition * MAXIMUM_VECTOR_INFLUENCE * stack_factor)
            .powi(2)
    }

    const fn calc_angle_wideness(angle: f64) -> f64 {
        smoothstep(angle, f64::to_radians(40.0), f64::to_radians(140.0))
    }

    pub const fn calc_angle_acuteness(angle: f64) -> f64 {
        smoothstep(angle, f64::to_radians(140.0), f64::to_radians(40.0))
    }
}

pub struct FlowAimEvaluator;

impl FlowAimEvaluator {
    const VELOCITY_CHANGE_MULTIPLIER: f64 = 0.52;

    #[allow(clippy::float_cmp, clippy::too_many_lines)]
    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        with_slider_travel_dist: bool,
        object_radius: f64,
    ) -> f64 {
        if curr.base.is_spinner() || curr.idx <= 1 {
            return 0.0;
        }

        let Some(osu_last_obj) = curr.previous(0, diff_objects) else {
            return 0.0;
        };

        if osu_last_obj.base.is_spinner() {
            return 0.0;
        }

        let Some(osu_last_last_obj) = curr.previous(1, diff_objects) else {
            return 0.0;
        };

        let osu_curr_obj = curr;

        let curr_distance = if with_slider_travel_dist {
            osu_curr_obj.lazy_jump_dist
        } else {
            osu_curr_obj.jump_dist
        };
        let prev_distance = if with_slider_travel_dist {
            osu_last_obj.lazy_jump_dist
        } else {
            osu_last_obj.jump_dist
        };

        let mut curr_velocity = curr_distance / osu_curr_obj.adjusted_delta_time;

        if osu_last_obj.base.is_slider() && with_slider_travel_dist {
            let slider_distance = osu_last_obj.lazy_travel_dist + osu_curr_obj.lazy_jump_dist;
            curr_velocity = curr_velocity.max(slider_distance / osu_curr_obj.adjusted_delta_time);
        }

        let prev_velocity = prev_distance / osu_last_obj.adjusted_delta_time;

        let mut flow_difficulty = curr_velocity;

        // Use a reduced circle-size bonus because the original bonus was tuned
        // for a different distance/time scaling.
        flow_difficulty *= osu_curr_obj.small_circle_bonus.sqrt();

        // Rhythm changes are harder to flow.
        let rhythm_difference = (osu_curr_obj
            .adjusted_delta_time
            .max(osu_last_obj.adjusted_delta_time)
            - osu_curr_obj
                .adjusted_delta_time
                .min(osu_last_obj.adjusted_delta_time))
            / 50.0;
        flow_difficulty *= 1.0 + rhythm_difference.powi(4).min(0.25);

        if let Some((curr_angle, last_angle)) = osu_curr_obj.angle.zip(osu_last_obj.angle) {
            let angle_difference = (curr_angle - last_angle).abs();
            let angle_difference_adjusted = (angle_difference / 2.0).sin() * 180.0;
            let angular_velocity =
                angle_difference_adjusted / (osu_curr_obj.adjusted_delta_time * 0.1);

            // Consistent, low-angular-velocity flow is easier to follow than
            // erratic flow.
            flow_difficulty *= 0.8 + (angular_velocity / 270.0).sqrt();
        }

        // If all three notes overlap, do not reward bonuses because no
        // additional movement is required.
        let mut overlapped_notes_weight = 1.0;

        if curr.idx > 2 {
            let o1 = Self::calculate_overlap_factor(osu_curr_obj, osu_last_obj, object_radius);
            let o2 = Self::calculate_overlap_factor(osu_curr_obj, osu_last_last_obj, object_radius);
            let o3 = Self::calculate_overlap_factor(osu_last_obj, osu_last_last_obj, object_radius);

            overlapped_notes_weight = 1.0 - o1 * o2 * o3;
        }

        if let Some(curr_angle) = osu_curr_obj.angle {
            // Acute angles are also hard to flow.
            flow_difficulty += curr_velocity
                * SnapAimEvaluator::calc_angle_acuteness(curr_angle)
                * overlapped_notes_weight;
        }

        if prev_velocity.max(curr_velocity) != 0.0 {
            if with_slider_travel_dist {
                curr_velocity = curr_distance / osu_curr_obj.adjusted_delta_time;
            }

            let dist_ratio = smoothstep(
                (prev_velocity - curr_velocity).abs() / prev_velocity.max(curr_velocity),
                0.0,
                1.0,
            );

            let overlap_velocity_buff = (OsuDifficultyObject::NORMALIZED_DIAMETER as f64 * 1.25
                / osu_curr_obj
                    .adjusted_delta_time
                    .min(osu_last_obj.adjusted_delta_time))
            .min((prev_velocity - curr_velocity).abs());

            flow_difficulty += overlap_velocity_buff
                * dist_ratio
                * overlapped_notes_weight
                * Self::VELOCITY_CHANGE_MULTIPLIER;
        }

        if osu_curr_obj.base.is_slider() && with_slider_travel_dist {
            // Include slider velocity to keep velocity consistent with snap aim.
            flow_difficulty += osu_curr_obj.travel_dist / osu_curr_obj.travel_time;
        }

        flow_difficulty = flow_difficulty.powf(1.45);

        // Spacing below one radius is always flowed, so reduce low-spacing
        // difficulty.
        flow_difficulty
            * smootherstep(
                curr_distance,
                0.0,
                OsuDifficultyObject::NORMALIZED_RADIUS as f64,
            )
    }

    fn calculate_overlap_factor(
        first: &OsuDifficultyObject<'_>,
        second: &OsuDifficultyObject<'_>,
        object_radius: f64,
    ) -> f64 {
        let distance = f64::from((first.base.stacked_pos() - second.base.stacked_pos()).length());

        (1.0 - ((distance - object_radius).max(0.0) / object_radius).powi(2)).clamp(0.0, 1.0)
    }
}

pub struct AgilityEvaluator;

impl AgilityEvaluator {
    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
    ) -> f64 {
        if curr.base.is_spinner() {
            return 0.0;
        }

        const DISTANCE_CAP: f64 = OsuDifficultyObject::NORMALIZED_DIAMETER as f64 * 1.2;

        let travel_distance = if curr.idx > 0 {
            curr.previous(0, diff_objects)
                .map_or(0.0, |prev| prev.lazy_travel_dist)
        } else {
            0.0
        };
        let distance = travel_distance + curr.lazy_jump_dist;
        let distance_scaled = distance.min(DISTANCE_CAP) / DISTANCE_CAP;

        let mut agility_difficulty = distance_scaled * 1000.0 / curr.adjusted_delta_time;

        agility_difficulty *= curr.small_circle_bonus.powf(1.5);
        agility_difficulty *= Self::high_bpm_bonus(curr.adjusted_delta_time);

        agility_difficulty
    }

    fn high_bpm_bonus(ms: f64) -> f64 {
        1.0 / (1.0 - 0.2_f64.powf(ms / 1000.0))
    }
}
