use crate::{
    any::difficulty::skills::HarmonicSkill,
    model::mods::GameMods,
    osu::difficulty::{
        evaluators::{RhythmEvaluator, SpeedEvaluator},
        object::OsuDifficultyObject,
    },
    util::difficulty::logistic,
};

/// Represents the skill required to press keys while keeping up with the
/// speed of objects.
#[derive(Clone)]
pub struct Speed {
    harmonic: HarmonicSkill,
    current_strain: f64,
    hit_window: f64,
    has_relax_mod: bool,
    has_autopilot_mod: bool,
    slider_strains: Vec<f64>,
}

impl Speed {
    const HARMONIC_SCALE: f64 = 20.0;
    const DECAY_EXPONENT: f64 = 0.9;
    const STRAIN_DECAY_BASE: f64 = 0.3;
    const SKILL_MULTIPLIER: f64 = 1.16;

    /// `hit_window` is the full, rate-adjusted great hit window, matching
    /// `OsuDifficultyHitObject.HitWindowGreat` in lazer. Construct it as
    /// `2.0 * great_hit_window` from rosu-pp's half-window attribute.
    pub fn new(mods: &GameMods, hit_window: f64) -> Self {
        Self {
            harmonic: HarmonicSkill::default(),
            current_strain: 0.0,
            hit_window,
            has_relax_mod: mods.rx(),
            has_autopilot_mod: mods.ap(),
            slider_strains: Vec::with_capacity(64),
        }
    }

    pub fn process<'a>(
        &mut self,
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) {
        let object_difficulty = self.object_difficulty_of(curr, objects);
        self.harmonic.process(object_difficulty);
    }

    fn object_difficulty_of<'a>(
        &mut self,
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) -> f64 {
        // This early return is intentional: Relax neither decays nor updates
        // the running strain and does not record slider strain.
        if self.has_relax_mod {
            return 0.0;
        }

        let decay = Self::STRAIN_DECAY_BASE.powf(curr.adjusted_delta_time / 1000.0);
        self.current_strain *= decay;
        self.current_strain += self.calculate_adjusted_difficulty(curr, objects)
            * (1.0 - decay)
            * Self::SKILL_MULTIPLIER;

        let current_rhythm = RhythmEvaluator::evaluate_diff_of(curr, objects, self.hit_window);
        let total_strain = self.current_strain * current_rhythm;

        if curr.base.is_slider() {
            self.slider_strains.push(total_strain);
        }

        total_strain
    }

    fn calculate_adjusted_difficulty<'a>(
        &self,
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) -> f64 {
        let mut difficulty = SpeedEvaluator::evaluate_diff_of(curr, objects, self.hit_window);

        if self.has_autopilot_mod {
            difficulty *= 0.5;
        }

        difficulty
    }

    pub fn difficulty_value(&mut self) -> f64 {
        self.harmonic
            .difficulty_value(Self::HARMONIC_SCALE, Self::DECAY_EXPONENT, |_| {})
    }

    pub fn difficulty_to_performance(difficulty: f64) -> f64 {
        4.0 * difficulty.powi(3)
    }

    pub fn count_top_weighted_object_difficulties(&self, difficulty_value: f64) -> f64 {
        self.harmonic.count_top_weighted_object_difficulties(
            difficulty_value,
            Self::HARMONIC_SCALE,
            Self::DECAY_EXPONENT,
            |_| {},
            0.88,
            10.0,
        )
    }

    pub fn relevant_object_count(&self) -> f64 {
        let object_difficulties = self.harmonic.object_difficulties();

        let Some(max_strain) = object_difficulties.iter().copied().reduce(f64::max) else {
            return 0.0;
        };

        if max_strain == 0.0 {
            return 0.0;
        }

        object_difficulties
            .iter()
            .map(|&strain| 1.0 / (1.0 + (-(strain / max_strain * 12.0 - 6.0)).exp()))
            .sum()
    }

    pub fn count_top_weighted_sliders(&self, difficulty_value: f64) -> f64 {
        if self.slider_strains.is_empty() {
            return 0.0;
        }

        let object_weight_sum = self.harmonic.object_weight_sum();

        if object_weight_sum == 0.0 {
            return 0.0;
        }

        let consistent_top_object = difficulty_value / object_weight_sum;

        if consistent_top_object == 0.0 {
            return 0.0;
        }

        self.slider_strains
            .iter()
            .map(|&strain| logistic(strain / consistent_top_object, 0.88, 10.0, Some(1.1)))
            .sum()
    }

    /// The target HarmonicSkill stores one value per processed difficulty
    /// object, not 400 ms section peaks.
    pub fn object_difficulties(&self) -> &[f64] {
        self.harmonic.object_difficulties()
    }
}
