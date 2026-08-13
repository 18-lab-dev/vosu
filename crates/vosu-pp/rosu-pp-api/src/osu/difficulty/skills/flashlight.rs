use crate::{
    GameMods,
    any::difficulty::{
        object::{HasStartTime, IDifficultyObject},
        skills::{StrainSkill, strain_decay},
    },
    osu::difficulty::{evaluators::FlashlightEvaluator, object::OsuDifficultyObject},
    util::traits::IEnumerable,
};

define_skill! {
    pub struct Flashlight: StrainSkill => [OsuDifficultyObject<'a>][OsuDifficultyObject<'a>] {
        current_strain: f64,
        has_hidden_mod: bool,
        hidden_for_opacity: bool,
        has_touch_device_mod: bool,
        has_relax_mod: bool,
        has_autopilot_mod: bool,
        attraction_strength: Option<f64>,
        deflate_start_scale: Option<f64>,
        total_objects: usize,
        overall_difficulty: f64,
        evaluator: FlashlightEvaluator,
    }

    pub fn new(
        mods: &GameMods,
        radius: f64,
        total_objects: usize,
        overall_difficulty: f64
    ) -> Self {
        let scaling_factor = 52.0 / radius;
        let has_hidden_mod = mods.hd();
        let hidden_for_opacity =
            has_hidden_mod && mods.hd_only_fade_approach_circles() != Some(true);
        let has_touch_device_mod = mods.td();
        let has_relax_mod = mods.rx();
        let has_autopilot_mod = mods.ap();
        let attraction_strength = mods.attraction_strength();
        let deflate_start_scale = mods.deflate_start_scale();
        let evaluator = FlashlightEvaluator::new(scaling_factor);

        Self {
            current_strain: 0.0,
            has_hidden_mod: has_hidden_mod,
            hidden_for_opacity: hidden_for_opacity,
            has_touch_device_mod: has_touch_device_mod,
            has_relax_mod: has_relax_mod,
            has_autopilot_mod: has_autopilot_mod,
            attraction_strength: attraction_strength,
            deflate_start_scale: deflate_start_scale,
            total_objects: total_objects,
            overall_difficulty: overall_difficulty,
            evaluator: evaluator,
        }
    }
}

impl Flashlight {
    const SKILL_MULTIPLIER: f64 = 0.058;
    const STRAIN_DECAY_BASE: f64 = 0.15;

    fn calculate_initial_strain(
        &mut self,
        time: f64,
        curr: &OsuDifficultyObject<'_>,
        objects: &[OsuDifficultyObject<'_>],
    ) -> f64 {
        let prev_start_time = curr
            .previous(0, objects)
            .map_or(0.0, HasStartTime::start_time);

        self.current_strain * strain_decay(time - prev_start_time, Self::STRAIN_DECAY_BASE)
    }

    fn strain_value_at(
        &mut self,
        curr: &OsuDifficultyObject<'_>,
        objects: &[OsuDifficultyObject<'_>],
    ) -> f64 {
        self.current_strain *= strain_decay(curr.delta_time, Self::STRAIN_DECAY_BASE);

        let mut difficulty = self.evaluator.evaluate_diff_of(
            curr,
            objects,
            self.hidden_for_opacity,
            self.has_hidden_mod,
        );

        if self.has_touch_device_mod {
            difficulty = difficulty.powf(0.9);
        }

        if let Some(strength) = self.attraction_strength {
            difficulty *= 1.0 - strength;
        }

        if let Some(initial_scale) = self.deflate_start_scale {
            difficulty *=
                crate::util::difficulty::reverse_lerp(initial_scale, 11.0, 1.0).clamp(0.1, 1.0);
        }

        if self.has_relax_mod {
            difficulty *= 0.7;
        }

        if self.has_autopilot_mod {
            difficulty *= 0.4;
        }

        difficulty *= 0.985 + self.overall_difficulty.max(0.0).powi(2) / 4_000.0;
        self.current_strain += difficulty * Self::SKILL_MULTIPLIER;

        self.current_strain
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "function definition needs to stay in-sync with `StrainSkill::difficulty_value`"
    )]
    fn difficulty_value(current_strain_peaks: Vec<f64>) -> f64 {
        current_strain_peaks.cs_sum()
    }

    pub fn cloned_difficulty_value(&self) -> f64 {
        let mut sum = self.current_strain_peaks().cs_sum();
        let total_objects = self.total_objects as f64;

        sum *= 0.7
            + 0.1 * (total_objects / 200.0).min(1.0)
            + if self.total_objects > 200 {
                0.2 * ((total_objects - 200.0) / 200.0).min(1.0)
            } else {
                0.0
            };

        sum
    }

    pub fn difficulty_to_performance(difficulty: f64) -> f64 {
        25.0 * f64::powf(difficulty, 2.0)
    }
}
