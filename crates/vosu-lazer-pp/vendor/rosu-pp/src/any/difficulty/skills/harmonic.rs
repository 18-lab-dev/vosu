use crate::util::difficulty::logistic;

/// Shared storage and exact harmonic summation used by osu! speed and reading.
#[derive(Clone, Debug, Default)]
pub struct HarmonicSkill {
    object_difficulties: Vec<f64>,
    object_weight_sum: f64,
}

impl HarmonicSkill {
    pub fn process(&mut self, object_difficulty: f64) {
        self.object_difficulties.push(object_difficulty);
    }

    pub fn object_difficulties(&self) -> &[f64] {
        &self.object_difficulties
    }

    pub fn object_weight_sum(&self) -> f64 {
        self.object_weight_sum
    }

    /// `transform` is applied in chronological order before positive values are
    /// sorted. Reading uses it for the 60-second memorisation reduction; speed
    /// passes `|_| {}`.
    pub fn difficulty_value<F>(
        &mut self,
        harmonic_scale: f64,
        decay_exponent: f64,
        transform: F,
    ) -> f64
    where
        F: FnOnce(&mut Vec<f64>),
    {
        let mut difficulties = self.object_difficulties.clone();
        transform(&mut difficulties);
        difficulties.retain(|&difficulty| difficulty > 0.0);
        difficulties.sort_unstable_by(|a, b| b.total_cmp(a));

        difficulties
            .into_iter()
            .enumerate()
            .map(|(index, difficulty)| {
                let weight = harmonic_weight(index, harmonic_scale, decay_exponent);
                self.object_weight_sum += weight;

                difficulty * weight
            })
            .sum()
    }

    pub fn count_top_weighted_object_difficulties<F>(
        &self,
        difficulty_value: f64,
        _harmonic_scale: f64,
        _decay_exponent: f64,
        _transform: F,
        midpoint_offset: f64,
        multiplier: f64,
    ) -> f64
    where
        F: FnOnce(&mut Vec<f64>),
    {
        if self.object_difficulties.is_empty() {
            return 0.0;
        }

        let object_weight_sum = self.object_weight_sum;

        if object_weight_sum == 0.0 {
            return 0.0;
        }

        let consistent_top_object = difficulty_value / object_weight_sum;

        if consistent_top_object == 0.0 {
            return 0.0;
        }

        self.object_difficulties
            .iter()
            .map(|&difficulty| {
                logistic(
                    difficulty / consistent_top_object,
                    midpoint_offset,
                    multiplier,
                    Some(1.1),
                )
            })
            .sum()
    }

    pub fn difficulty_to_performance(difficulty: f64) -> f64 {
        4.0 * difficulty.powi(3)
    }
}

fn harmonic_weight(index: usize, harmonic_scale: f64, decay_exponent: f64) -> f64 {
    let index = index as f64;
    let scaled = harmonic_scale / (1.0 + index);

    (1.0 + scaled) / (index.powf(decay_exponent) + 1.0 + scaled)
}

#[cfg(test)]
mod tests {
    use super::{HarmonicSkill, harmonic_weight};

    #[test]
    fn repeated_difficulty_values_accumulate_object_weight_sum() {
        let mut skill = HarmonicSkill::default();

        for difficulty in [8.0, 4.0, 2.0] {
            skill.process(difficulty);
        }

        let first_difficulty = skill.difficulty_value(1.0, 0.9, |_| {});
        let first_weight_sum = skill.object_weight_sum();
        let expected_weight_sum = (0..3)
            .map(|index| harmonic_weight(index, 1.0, 0.9))
            .sum::<f64>();

        assert_eq!(first_weight_sum, expected_weight_sum);

        let second_difficulty = skill.difficulty_value(1.0, 0.9, |_| {});

        assert_eq!(second_difficulty, first_difficulty);
        assert_eq!(skill.object_weight_sum(), 2.0 * expected_weight_sum);
    }
}
