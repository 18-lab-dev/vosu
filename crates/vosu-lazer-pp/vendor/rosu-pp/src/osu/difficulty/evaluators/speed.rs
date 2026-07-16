use crate::{
    any::difficulty::object::IDifficultyObject,
    osu::difficulty::object::OsuDifficultyObject,
    util::difficulty::{bpm_to_milliseconds, milliseconds_to_bpm},
};

pub struct SpeedEvaluator;

impl SpeedEvaluator {
    const MIN_SPEED_BONUS: f64 = 200.0;
    const SPEED_BALANCING_FACTOR: f64 = 40.0;

    pub fn evaluate_diff_of<'a>(
        curr: &'a OsuDifficultyObject<'a>,
        diff_objects: &'a [OsuDifficultyObject<'a>],
        hit_window: f64,
    ) -> f64 {
        if curr.base.is_spinner() {
            return 0.0;
        }

        let mut strain_time = curr.adjusted_delta_time;
        let double_tap_feasibility =
            1.0 - curr.get_doubletapness(curr.next(0, diff_objects), hit_window);

        strain_time /= ((strain_time / hit_window) / 0.93).clamp(0.92, 1.0);

        let speed_bonus = if milliseconds_to_bpm(strain_time, None) > Self::MIN_SPEED_BONUS {
            let base = (bpm_to_milliseconds(Self::MIN_SPEED_BONUS, None) - strain_time)
                / Self::SPEED_BALANCING_FACTOR;

            0.75 * base.powi(2)
        } else {
            0.0
        };

        let mut speed_difficulty = (1.0 + speed_bonus) * 1000.0 / strain_time;
        speed_difficulty *= 1.0 / (1.0 - 0.3_f64.powf(curr.adjusted_delta_time / 1000.0));

        speed_difficulty * double_tap_feasibility
    }
}
