use crate::{
    any::difficulty::object::IDifficultyObject, catch::difficulty::object::CatchDifficultyObject,
};

pub struct MovementEvaluator;

impl MovementEvaluator {
    const NORMALIZED_HITOBJECT_RADIUS: f32 = 41.0;
    const DIRECTION_CHANGE_BONUS: f64 = 21.0;

    pub fn evaluate_diff_of(
        curr: &CatchDifficultyObject,
        diff_objects: &[CatchDifficultyObject],
        clock_rate: f64,
    ) -> f64 {
        let catch_last_obj = curr.previous(0, diff_objects);
        let catch_last_last_obj = curr.previous(1, diff_objects);

        let weighted_strain_time = curr.strain_time + 13.0 + (3.0 / clock_rate);

        let mut dist_addition = f64::from(curr.dist_moved.abs()).powf(1.3) / 510.0;
        let sqrt_strain = weighted_strain_time.sqrt();

        let mut edge_dash_bonus: f64 = 0.0;

        let last_strain_time = catch_last_obj.map_or(0.0, |obj| obj.strain_time);

        if curr.dist_moved.abs() > 0.1 {
            let last_dist_moved = catch_last_obj.map_or(0.0, |obj| obj.dist_moved);

            if curr.idx >= 1
                && last_dist_moved.abs() > 0.1
                && curr.dist_moved.signum() != last_dist_moved.signum()
            {
                let bonus_factor = f64::from(curr.dist_moved.abs().min(50.0) / 50.0);
                let anti_flow_factor = f64::from(last_dist_moved.abs().min(70.0) / 70.0).max(0.38);

                dist_addition += Self::DIRECTION_CHANGE_BONUS / (last_strain_time + 16.0).sqrt()
                    * bonus_factor
                    * anti_flow_factor
                    * (1.0 - (weighted_strain_time / 1000.0).powf(3.0)).max(0.0);
            }

            // * Base bonus for every movement, giving some weight to streams.
            dist_addition += 12.5
                * f64::from(f32::abs(curr.dist_moved).min(Self::NORMALIZED_HITOBJECT_RADIUS * 2.0))
                / f64::from(Self::NORMALIZED_HITOBJECT_RADIUS * 6.0)
                / sqrt_strain;
        }

        // osu! 97e94bab: repeated equal-spacing movements in one direction
        // require no additional input and are progressively nerfed.
        let mut linear_spacing_count = 0;

        for i in 0..curr.idx.min(10) {
            let Some(previous) = curr.previous(i, diff_objects) else {
                break;
            };

            if curr.dist_moved.signum() != previous.dist_moved.signum()
                || curr.dist_moved == 0.0
                || previous.dist_moved == 0.0
            {
                break;
            }

            let current_spacing = (f64::from(curr.dist_moved) / curr.strain_time).abs();
            let previous_spacing = (f64::from(previous.dist_moved) / previous.strain_time).abs();
            let relative_difference = (current_spacing / previous_spacing - 1.0).abs();

            if relative_difference > 0.05 {
                break;
            }

            linear_spacing_count += 1;
        }

        dist_addition *= 0.7_f64.powi(linear_spacing_count);

        // * Bonus for edge dashes.
        if curr.last_object.dist_to_hyper_dash <= 20.0 {
            if !curr.last_object.hyper_dash {
                edge_dash_bonus += 5.7;
            }

            // * Edge Dashes are easier at lower ms values
            dist_addition *= 1.0
                + edge_dash_bonus
                    * f64::from((20.0 - curr.last_object.dist_to_hyper_dash) / 20.0)
                    * ((curr.strain_time * clock_rate).min(265.0) / 265.0).powf(1.5);
        }

        let last_exact_dist_moved = catch_last_obj.map_or(0.0, |obj| obj.exact_dist_moved);
        let last_last_exact_dist_moved =
            catch_last_last_obj.map_or(0.0, |obj| obj.exact_dist_moved);

        let last_last_strain_time = catch_last_last_obj.map_or(0.0, |obj| obj.strain_time);

        // * There is an edge case where horizontal back and forth sliders create "buzz" patterns which are repeated "movements" with a distance lower than
        // * the platter's width but high enough to be considered a movement due to the absolute_player_positioning_error and NORMALIZED_HALF_CATCHER_WIDTH offsets
        // * We are detecting this exact scenario. The first back and forth is counted but all subsequent ones are nullified.
        // * To achieve that, we need to store the exact distances (distance ignoring absolute_player_positioning_error and NORMALIZED_HALF_CATCHER_WIDTH)
        if curr.idx >= 2
            && curr.exact_dist_moved.abs()
                <= CatchDifficultyObject::NORMALIZED_HALF_CATCHER_WIDTH * 2.0
            && curr.exact_dist_moved == -last_exact_dist_moved
            && last_exact_dist_moved == -last_last_exact_dist_moved
            && curr.strain_time == last_strain_time
            && last_strain_time == last_last_strain_time
        {
            dist_addition = 0.0;
        }

        dist_addition / weighted_strain_time
    }
}

#[cfg(test)]
mod tests {
    use crate::catch::difficulty::object::{CatchDifficultyObject, LastObject};

    use super::MovementEvaluator;

    fn object(idx: usize, exact_dist_moved: f32) -> CatchDifficultyObject {
        CatchDifficultyObject {
            idx,
            start_time: idx as f64 * 100.0,
            base_end_time: idx as f64 * 100.0,
            delta_time: 100.0,
            normalized_pos: 0.0,
            last_normalized_pos: 0.0,
            player_pos: 0.0,
            last_player_pos: 0.0,
            dist_moved: 1.0,
            exact_dist_moved,
            strain_time: 100.0,
            last_object: LastObject {
                hyper_dash: false,
                dist_to_hyper_dash: f32::MAX,
                player_pos: None,
            },
        }
    }

    #[test]
    fn buzz_pattern_requires_exact_float_equality() {
        let almost_one = f32::from_bits(1.0_f32.to_bits() + 1);
        let objects = [object(0, almost_one), object(1, -1.0), object(2, 1.0)];

        assert!(MovementEvaluator::evaluate_diff_of(&objects[2], &objects, 1.0) > 0.0);
    }
}
