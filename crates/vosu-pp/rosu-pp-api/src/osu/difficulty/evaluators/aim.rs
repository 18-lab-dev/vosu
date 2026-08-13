use crate::{
    any::difficulty::object::IDifficultyObject,
    osu::difficulty::object::OsuDifficultyObject,
    util::difficulty::{milliseconds_to_bpm, reverse_lerp, smootherstep, smoothstep},
};

pub struct AgilityEvaluator;

impl AgilityEvaluator {
    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) -> f64 {
        if curr.base.is_spinner() {
            return 0.0;
        }
        const CAP: f64 = OsuDifficultyObject::NORMALIZED_DIAMETER as f64 * 1.2;
        let travel = curr
            .previous(0, objects)
            .map_or(0.0, |prev| prev.lazy_travel_dist);
        let scaled = (travel + curr.lazy_jump_dist).min(CAP) / CAP;
        scaled * 1000.0 / curr.adjusted_delta_time * curr.small_circle_bonus.powf(1.5)
            / (1.0 - 0.2_f64.powf(curr.adjusted_delta_time / 1000.0))
    }
}

pub struct FlowAimEvaluator;

impl FlowAimEvaluator {
    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
        sliders: bool,
    ) -> f64 {
        if curr.base.is_spinner() || curr.idx <= 1 {
            return 0.0;
        }
        let Some((last, last_last)) = curr.previous(0, objects).zip(curr.previous(1, objects))
        else {
            return 0.0;
        };
        if last.base.is_spinner() {
            return 0.0;
        }
        let curr_dist = if sliders {
            curr.lazy_jump_dist
        } else {
            curr.jump_dist
        };
        let prev_dist = if sliders {
            last.lazy_jump_dist
        } else {
            last.jump_dist
        };
        let mut curr_vel = curr_dist / curr.adjusted_delta_time;
        if last.base.is_slider() && sliders {
            curr_vel = curr_vel
                .max((last.lazy_travel_dist + curr.lazy_jump_dist) / curr.adjusted_delta_time);
        }
        let prev_vel = prev_dist / last.adjusted_delta_time;
        let mut difficulty = curr_vel * curr.small_circle_bonus.sqrt();
        let delta_diff = curr.adjusted_delta_time.max(last.adjusted_delta_time)
            - curr.adjusted_delta_time.min(last.adjusted_delta_time);
        difficulty *= 1.0 + (delta_diff / 50.0).powi(4).min(0.25);
        if let Some((curr_angle, last_angle)) = curr.angle.zip(last.angle) {
            let angle = ((curr_angle - last_angle).abs() / 2.0).sin() * 180.0;
            difficulty *= 0.8 + (angle / (curr.adjusted_delta_time * 0.1) / 270.0).sqrt();
        }
        let mut overlap_weight = 1.0;
        if curr.idx > 2 {
            overlap_weight = 1.0
                - Self::overlap(curr, last)
                    * Self::overlap(curr, last_last)
                    * Self::overlap(last, last_last);
        }
        if let Some(angle) = curr.angle {
            difficulty += curr_vel * SnapAimEvaluator::calc_angle_acuteness(angle) * overlap_weight;
        }
        if prev_vel.max(curr_vel) != 0.0 {
            if sliders {
                curr_vel = curr_dist / curr.adjusted_delta_time;
            }
            let ratio = smoothstep(
                (prev_vel - curr_vel).abs() / prev_vel.max(curr_vel),
                0.0,
                1.0,
            );
            let buff = (OsuDifficultyObject::NORMALIZED_DIAMETER as f64 * 1.25
                / curr.adjusted_delta_time.min(last.adjusted_delta_time))
            .min((prev_vel - curr_vel).abs());
            difficulty += buff * ratio * overlap_weight * 0.52;
        }
        if curr.base.is_slider() && sliders {
            difficulty += curr.travel_dist / curr.travel_time;
        }
        difficulty.powf(1.45)
            * smootherstep(
                curr_dist,
                0.0,
                OsuDifficultyObject::NORMALIZED_RADIUS as f64,
            )
    }

    fn overlap(first: &OsuDifficultyObject<'_>, second: &OsuDifficultyObject<'_>) -> f64 {
        let distance = f64::from((first.base.stacked_pos() - second.base.stacked_pos()).length());
        (1.0 - ((distance - first.radius).max(0.0) / first.radius).powi(2)).clamp(0.0, 1.0)
    }
}

pub struct SnapAimEvaluator;

impl SnapAimEvaluator {
    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
        sliders: bool,
    ) -> f64 {
        if curr.base.is_spinner() || curr.idx <= 1 {
            return 0.0;
        }
        let Some((last, _)) = curr.previous(0, objects).zip(curr.previous(1, objects)) else {
            return 0.0;
        };
        if last.base.is_spinner() {
            return 0.0;
        }
        const RADIUS: f64 = OsuDifficultyObject::NORMALIZED_RADIUS as f64;
        const DIAMETER: f64 = OsuDifficultyObject::NORMALIZED_DIAMETER as f64;
        let curr_dist = if sliders {
            curr.lazy_jump_dist
        } else {
            curr.jump_dist
        };
        let prev_dist = if sliders {
            last.lazy_jump_dist
        } else {
            last.jump_dist
        };
        let mut curr_vel = curr_dist / curr.adjusted_delta_time;
        if last.base.is_slider() && sliders {
            curr_vel = curr_vel
                .max((last.lazy_travel_dist + curr.lazy_jump_dist) / curr.adjusted_delta_time);
        }
        let prev_vel = prev_dist / last.adjusted_delta_time;
        let mut difficulty = curr_vel * Self::vector_repetition(curr, last, objects);
        if let Some((curr_angle, last_angle)) = curr.angle.zip(last.angle) {
            let influence = curr_vel.min(prev_vel);
            let mut acute = 0.0;
            if curr.adjusted_delta_time.max(last.adjusted_delta_time)
                < 1.25 * curr.adjusted_delta_time.min(last.adjusted_delta_time)
            {
                acute = Self::calc_angle_acuteness(curr_angle);
                acute *=
                    0.08 + 0.92 * (1.0 - acute.min(Self::calc_angle_acuteness(last_angle).powi(3)));
                acute *= influence
                    * smootherstep(
                        milliseconds_to_bpm(curr.adjusted_delta_time, Some(2)),
                        300.0,
                        400.0,
                    )
                    * smootherstep(curr_dist, 0.0, DIAMETER * 2.0);
            }
            let mut wide = Self::angle_wideness(curr_angle);
            wide *= 0.25 + 0.75 * (1.0 - wide.min(Self::angle_wideness(last_angle).powi(3)));
            let mut wide_curr = curr_dist / curr.adjusted_delta_time.powf(1.45);
            let wide_prev = prev_dist / last.adjusted_delta_time.powf(1.45);
            if last.base.is_slider() && sliders {
                wide_curr = wide_curr.max(
                    (last.lazy_travel_dist + curr.lazy_jump_dist)
                        / curr.adjusted_delta_time.powf(1.45),
                );
            }
            wide *= wide_curr.min(wide_prev);
            if let Some(last_2) = curr.previous(2, objects) {
                let distance =
                    f64::from((last_2.base.stacked_pos() - last.base.stacked_pos()).length());
                if distance < 1.0 {
                    wide *= 1.0 - 0.55 * (1.0 - distance);
                }
            }
            difficulty += (acute * 2.41).max(wide * 9.67);
            let wiggle = influence
                * smootherstep(curr_dist, RADIUS, DIAMETER)
                * reverse_lerp(curr_dist, DIAMETER * 3.0, DIAMETER).powf(1.8)
                * smootherstep(curr_angle, 110_f64.to_radians(), 60_f64.to_radians())
                * smootherstep(prev_dist, RADIUS, DIAMETER)
                * reverse_lerp(prev_dist, DIAMETER * 3.0, DIAMETER).powf(1.8)
                * smootherstep(last_angle, 110_f64.to_radians(), 60_f64.to_radians());
            difficulty += wiggle * 1.02;
        }
        if prev_vel.max(curr_vel) != 0.0 {
            if sliders {
                curr_vel = curr_dist / curr.adjusted_delta_time;
            }
            let ratio = smoothstep(
                (prev_vel - curr_vel).abs() / prev_vel.max(curr_vel),
                0.0,
                1.0,
            );
            let buff = (DIAMETER * 1.25 / curr.adjusted_delta_time.min(last.adjusted_delta_time))
                .min((prev_vel - curr_vel).abs());
            let rhythm = (curr.adjusted_delta_time.min(last.adjusted_delta_time)
                / curr.adjusted_delta_time.max(last.adjusted_delta_time))
            .powi(2);
            difficulty += buff * ratio * rhythm * 0.9;
        }
        if curr.base.is_slider() && sliders {
            let bonus = curr.travel_dist / curr.travel_time;
            difficulty += (if bonus < 1.0 { bonus } else { bonus.powf(0.75) }) * 1.5;
        }
        difficulty * curr.small_circle_bonus
            / (1.0 - 0.03_f64.powf((curr.adjusted_delta_time / 1000.0).powf(0.65)))
    }

    fn vector_repetition<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        last: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) -> f64 {
        let Some((curr_angle, last_angle)) = curr.angle.zip(last.angle) else {
            return 1.0;
        };
        let mut count = 0.0;
        for index in 0..6 {
            let Some(prev) = curr.previous(index, objects) else {
                break;
            };
            if curr.adjusted_delta_time.max(prev.adjusted_delta_time)
                > 1.1 * curr.adjusted_delta_time.min(prev.adjusted_delta_time)
            {
                break;
            }
            if let Some((a, b)) = prev
                .normalized_vector_angle
                .zip(curr.normalized_vector_angle)
            {
                count += (8.0 * 11.25_f64.to_radians().min((b - a).abs())).cos();
            }
        }
        let vector = (0.5 / count).min(1.0).powi(2);
        let stack = smootherstep(
            curr.lazy_jump_dist,
            0.0,
            OsuDifficultyObject::NORMALIZED_DIAMETER as f64,
        );
        let adjusted = (2.0
            * 45_f64
                .to_radians()
                .min((curr_angle - last_angle).abs() * stack))
        .cos();
        let base = 1.0 - 0.15 * Self::calc_angle_acuteness(last_angle) * adjusted;
        (base + (1.0 - base) * vector * 0.5 * stack).powi(2)
    }

    fn angle_wideness(angle: f64) -> f64 {
        smoothstep(angle, 40_f64.to_radians(), 140_f64.to_radians())
    }
    pub fn calc_angle_acuteness(angle: f64) -> f64 {
        smoothstep(angle, 140_f64.to_radians(), 40_f64.to_radians())
    }
}
