use std::cmp;

use crate::{
    any::difficulty::object::IDifficultyObject,
    osu::{
        difficulty::object::OsuDifficultyObject,
        object::{OsuObject, OsuObjectKind},
    },
};

use super::super::{HD_FADE_IN_DURATION_MULTIPLIER, HD_FADE_OUT_DURATION_MULTIPLIER};

pub struct FlashlightEvaluator {
    scaling_factor: f64,
}

impl FlashlightEvaluator {
    const MAX_OPACITY_BONUS: f64 = 0.4;
    const HIDDEN_BONUS: f64 = 0.2;

    const MIN_VELOCITY: f64 = 0.5;
    const SLIDER_MULTIPLIER: f64 = 1.3;

    const MIN_ANGLE_MULTIPLIER: f64 = 0.2;

    pub const fn new(scaling_factor: f64, _time_preempt: f64) -> Self {
        Self { scaling_factor }
    }

    pub fn evaluate_diff_of<'a>(
        &self,
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        has_hidden_mod: bool,
        hidden_fades_objects: bool,
    ) -> f64 {
        if curr.base.is_spinner() {
            return 0.0;
        }

        let osu_curr = curr;
        let osu_hit_obj = curr.base;

        let mut small_dist_nerf = 1.0;
        let mut cumulative_strain_time = 0.0;
        let mut flashlight_difficulty = 0.0;
        let mut last_obj = osu_curr;
        let mut angle_repeat_count = 0.0;

        // This is iterating backwards in time from the current object.
        for i in 0..cmp::min(curr.idx, 10) {
            let Some(curr_obj) = curr.previous(i, diff_objects) else {
                break;
            };

            let curr_hit_obj = curr_obj.base;

            cumulative_strain_time += last_obj.adjusted_delta_time;

            if !curr_obj.base.is_spinner() {
                let jump_distance = f64::from(
                    (osu_hit_obj.stacked_pos() - curr_hit_obj.stacked_end_pos()).length(),
                );

                // We want to nerf objects that can be easily seen within the Flashlight circle radius.
                if i == 0 {
                    small_dist_nerf = (jump_distance / 75.0).min(1.0);
                }

                // We also want to nerf stacks so that only the first object of the stack is accounted for.
                let stack_nerf = ((curr_obj.lazy_jump_dist / self.scaling_factor) / 25.0).min(1.0);

                // Bonus based on how visible the object is.
                let opacity_bonus = 1.0
                    + Self::MAX_OPACITY_BONUS
                        * (1.0
                            - self.opacity_at(
                                osu_curr,
                                curr_hit_obj.start_time,
                                hidden_fades_objects,
                            ));

                flashlight_difficulty +=
                    stack_nerf * opacity_bonus * self.scaling_factor * jump_distance
                        / cumulative_strain_time;

                if let Some((curr_obj_angle, osu_curr_angle)) = curr_obj.angle.zip(osu_curr.angle) {
                    // Objects further back in time should count less for the nerf.
                    if (curr_obj_angle - osu_curr_angle).abs() < 0.02 {
                        angle_repeat_count += (1.0 - 0.1 * i as f64).max(0.0);
                    }
                }
            }

            last_obj = curr_obj;
        }

        flashlight_difficulty = (small_dist_nerf * flashlight_difficulty).powf(2.0);

        // Additional bonus for Hidden due to there being no approach circles.
        if has_hidden_mod {
            flashlight_difficulty *= 1.0 + Self::HIDDEN_BONUS;
        }

        // Nerf patterns with repeated angles.
        flashlight_difficulty *= Self::MIN_ANGLE_MULTIPLIER
            + (1.0 - Self::MIN_ANGLE_MULTIPLIER) / (angle_repeat_count + 1.0);

        let mut slider_bonus = 0.0;

        if let OsuObjectKind::Slider(slider) = &osu_curr.base.kind {
            // Invert the scaling factor to determine the true travel distance independent of circle size.
            let pixel_travel_distance = osu_curr.lazy_travel_dist / self.scaling_factor;

            // Reward sliders based on velocity.
            slider_bonus = ((pixel_travel_distance / osu_curr.travel_time - Self::MIN_VELOCITY)
                .max(0.0))
            .powf(0.5);

            // Longer sliders require more memorisation.
            slider_bonus *= pixel_travel_distance;

            // Nerf sliders with repeats, as less memorisation is required.
            let repeat_count = slider.repeat_count();

            if repeat_count > 0 {
                slider_bonus /= (repeat_count + 1) as f64;
            }
        }

        flashlight_difficulty += slider_bonus * Self::SLIDER_MULTIPLIER;

        flashlight_difficulty
    }

    /// Exact `OsuDifficultyHitObject.OpacityAt` semantics needed by Flashlight.
    ///
    /// The normal fade-in duration is retained for the fade-in opacity while
    /// Hidden's adjusted `TimeFadeIn` is used for the fade-out start.
    fn opacity_at(
        &self,
        object: &OsuDifficultyObject<'_>,
        time: f64,
        hidden_fades_objects: bool,
    ) -> f64 {
        if time > object.base.start_time {
            return 0.0;
        }

        let time_preempt = object.base.time_preempt;
        let fade_in_start_time = object.base.start_time - time_preempt;
        let fade_in_duration = 400.0 * (time_preempt / OsuObject::PREEMPT_MIN).min(1.0);
        let fade_in_opacity = ((time - fade_in_start_time) / fade_in_duration).clamp(0.0, 1.0);

        if !hidden_fades_objects {
            return fade_in_opacity;
        }

        let hidden_time_fade_in = time_preempt * HD_FADE_IN_DURATION_MULTIPLIER;

        let fade_out_start_time = object.base.start_time - time_preempt + hidden_time_fade_in;
        let fade_out_duration = time_preempt * HD_FADE_OUT_DURATION_MULTIPLIER;
        let fade_out_opacity =
            1.0 - ((time - fade_out_start_time) / fade_out_duration).clamp(0.0, 1.0);

        fade_in_opacity.min(fade_out_opacity)
    }
}
