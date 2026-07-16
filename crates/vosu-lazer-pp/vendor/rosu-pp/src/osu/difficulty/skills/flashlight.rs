use crate::{
    GameMods,
    any::difficulty::{
        object::{HasStartTime, IDifficultyObject},
        skills::{StrainSkill, strain_decay},
    },
    osu::difficulty::{evaluators::FlashlightEvaluator, object::OsuDifficultyObject},
    util::{difficulty::reverse_lerp, traits::IEnumerable},
};

/// Represents the skill required to memorise and hit every object in a map with
/// Flashlight enabled.
pub struct Flashlight {
    current_strain: f64,

    has_flashlight_mod: bool,
    has_hidden_mod: bool,
    hidden_fades_objects: bool,
    has_touch_device_mod: bool,
    has_relax_mod: bool,
    has_autopilot_mod: bool,
    attraction_strength: Option<f64>,
    deflate_initial_scale: Option<f64>,

    total_objects: u32,
    overall_difficulty: f64,
    evaluator: FlashlightEvaluator,

    strain_skill_current_section_peak: f64,
    strain_skill_current_section_end: f64,
    strain_skill_strain_peaks: Vec<f64>,
    strain_skill_object_strains: Vec<f64>,
}

impl Flashlight {
    const SKILL_MULTIPLIER: f64 = 0.058;
    const STRAIN_DECAY_BASE: f64 = 0.15;

    pub fn new(
        mods: &GameMods,
        radius: f64,
        time_preempt: f64,
        total_objects: u32,
        overall_difficulty: f64,
    ) -> Self {
        let scaling_factor = 52.0 / radius;
        let has_hidden_mod = mods.hd();

        Self {
            current_strain: 0.0,

            has_flashlight_mod: mods.fl(),
            has_hidden_mod,
            hidden_fades_objects: has_hidden_mod
                && !mods.hd_only_fade_approach_circles().unwrap_or(false),
            has_touch_device_mod: mods.td(),
            has_relax_mod: mods.rx(),
            has_autopilot_mod: mods.ap(),
            attraction_strength: mods.attraction_strength(),
            deflate_initial_scale: mods.deflate_start_scale(),

            total_objects,
            overall_difficulty,
            evaluator: FlashlightEvaluator::new(scaling_factor, time_preempt),

            strain_skill_current_section_peak: 0.0,
            strain_skill_current_section_end: 0.0,
            strain_skill_strain_peaks: Vec::with_capacity(256),
            strain_skill_object_strains: Vec::with_capacity(256),
        }
    }

    pub fn current_strain_peaks(&self) -> Vec<f64> {
        <Self as StrainSkill>::get_current_strain_peaks(
            self.strain_skill_strain_peaks.clone(),
            self.strain_skill_current_section_peak,
        )
    }

    fn calculate_initial_strain(
        &self,
        time: f64,
        curr: &OsuDifficultyObject<'_>,
        objects: &[OsuDifficultyObject<'_>],
    ) -> f64 {
        let previous_start_time = curr
            .previous(0, objects)
            .map_or(0.0, HasStartTime::start_time);

        self.current_strain * strain_decay(time - previous_start_time, Self::STRAIN_DECAY_BASE)
    }

    fn strain_value_at(
        &mut self,
        curr: &OsuDifficultyObject<'_>,
        objects: &[OsuDifficultyObject<'_>],
    ) -> f64 {
        if !self.has_flashlight_mod {
            return 0.0;
        }

        self.current_strain *= strain_decay(curr.delta_time, Self::STRAIN_DECAY_BASE);
        self.current_strain +=
            self.calculate_adjusted_difficulty(curr, objects) * Self::SKILL_MULTIPLIER;

        self.current_strain
    }

    fn calculate_adjusted_difficulty(
        &self,
        curr: &OsuDifficultyObject<'_>,
        objects: &[OsuDifficultyObject<'_>],
    ) -> f64 {
        let mut difficulty = self.evaluator.evaluate_diff_of(
            curr,
            objects,
            self.has_hidden_mod,
            self.hidden_fades_objects,
        );

        if self.has_touch_device_mod {
            difficulty = difficulty.powf(0.9);
        }

        if let Some(attraction_strength) = self.attraction_strength {
            difficulty *= 1.0 - attraction_strength;
        }

        if let Some(deflate_initial_scale) = self.deflate_initial_scale {
            difficulty *= reverse_lerp(deflate_initial_scale, 11.0, 1.0).clamp(0.1, 1.0);
        }

        if self.has_relax_mod {
            difficulty *= 0.7;
        }

        if self.has_autopilot_mod {
            difficulty *= 0.4;
        }

        // It is important to consider accuracy difficulty when scaling with accuracy.
        difficulty *= 0.985 + self.overall_difficulty.max(0.0).powf(2.0) / 4000.0;

        difficulty
    }

    fn length_adjustment(&self) -> f64 {
        let total_objects = f64::from(self.total_objects);

        0.7 + 0.1 * (total_objects / 200.0).min(1.0)
            + f64::from(u8::from(self.total_objects > 200))
                * 0.2
                * (f64::from(self.total_objects.saturating_sub(200)) / 200.0).min(1.0)
    }

    pub fn difficulty_to_performance(difficulty: f64) -> f64 {
        25.0 * difficulty.powf(2.0)
    }
}

// This is intentionally written out instead of using `define_skill!`.
// The target Flashlight skill's final DifficultyValue depends on `total_objects`,
// but the current macro only supports a static `difficulty_value(Vec<f64>)`.
impl StrainSkill for Flashlight {
    type DifficultyObject<'a> = OsuDifficultyObject<'a>;
    type DifficultyObjects<'a> = [OsuDifficultyObject<'a>];

    fn process<'a>(
        &mut self,
        curr: &Self::DifficultyObject<'a>,
        objects: &Self::DifficultyObjects<'a>,
    ) {
        let section_length = f64::from(Self::SECTION_LENGTH);

        // The first object does not generate strain, so begin with an incremented section end.
        if curr.idx == 0 {
            self.strain_skill_current_section_end =
                (curr.start_time / section_length).ceil() * section_length;
        }

        while curr.start_time > self.strain_skill_current_section_end {
            self.save_current_peak();
            self.start_new_section_from(self.strain_skill_current_section_end, curr, objects);
            self.strain_skill_current_section_end += section_length;
        }

        let strain = self.strain_value_at(curr, objects);
        self.strain_skill_current_section_peak = strain.max(self.strain_skill_current_section_peak);
        self.strain_skill_object_strains.push(strain);
    }

    fn object_strains(&self) -> &[f64] {
        &self.strain_skill_object_strains
    }

    fn count_top_weighted_strains(&self, difficulty_value: f64) -> f64 {
        crate::any::difficulty::skills::count_top_weighted_strains(
            &self.strain_skill_object_strains,
            difficulty_value,
        )
    }

    fn save_current_peak(&mut self) {
        self.strain_skill_strain_peaks
            .push(self.strain_skill_current_section_peak);
    }

    fn start_new_section_from<'a>(
        &mut self,
        time: f64,
        curr: &Self::DifficultyObject<'a>,
        objects: &Self::DifficultyObjects<'a>,
    ) {
        self.strain_skill_current_section_peak = self.calculate_initial_strain(time, curr, objects);
    }

    fn into_current_strain_peaks(self) -> Vec<f64> {
        Self::get_current_strain_peaks(
            self.strain_skill_strain_peaks,
            self.strain_skill_current_section_peak,
        )
    }

    fn difficulty_value(current_strain_peaks: Vec<f64>) -> f64 {
        current_strain_peaks.cs_sum()
    }

    fn into_difficulty_value(self) -> f64 {
        let length_adjustment = self.length_adjustment();
        let current_strain_peaks = Self::get_current_strain_peaks(
            self.strain_skill_strain_peaks,
            self.strain_skill_current_section_peak,
        );

        Self::difficulty_value(current_strain_peaks) * length_adjustment
    }

    fn cloned_difficulty_value(&self) -> f64 {
        let current_strain_peaks = Self::get_current_strain_peaks(
            self.strain_skill_strain_peaks.clone(),
            self.strain_skill_current_section_peak,
        );

        Self::difficulty_value(current_strain_peaks) * self.length_adjustment()
    }
}
