use crate::{
    any::difficulty::{
        object::IDifficultyObject,
        skills::{StrainPeak, VariableLengthStrain, VariableLengthStrainSkill},
    },
    osu::difficulty::{
        evaluators::{AgilityEvaluator, FlowAimEvaluator, SnapAimEvaluator},
        object::OsuDifficultyObject,
    },
    util::difficulty::{logistic, logistic_exp, norm},
};

#[derive(Clone)]
pub struct Aim {
    variable: VariableLengthStrain,
    include_sliders: bool,
    current_strain: f64,
    slider_strains: Vec<f64>,
    object_radius: f64,
    overall_difficulty: f64,
    has_autopilot_mod: bool,
    has_touch_device_mod: bool,
    has_relax_mod: bool,
    magnetised_strength: Option<f64>,
}

impl Aim {
    const DECAY_WEIGHT: f64 = 0.9;
    const MAX_SECTION_LENGTH: u32 = 400;
    const REDUCED_SECTION_TIME: f64 = 4000.0;
    const REDUCED_STRAIN_BASELINE: f64 = 0.727;

    pub fn new(
        include_sliders: bool,
        object_radius: f64,
        overall_difficulty: f64,
        has_autopilot_mod: bool,
        has_touch_device_mod: bool,
        has_relax_mod: bool,
        magnetised_strength: Option<f64>,
    ) -> Self {
        Self {
            variable: VariableLengthStrain::new(Self::DECAY_WEIGHT, Self::MAX_SECTION_LENGTH),
            include_sliders,
            current_strain: 0.0,
            slider_strains: Vec::with_capacity(64),
            object_radius,
            overall_difficulty,
            has_autopilot_mod,
            has_touch_device_mod,
            has_relax_mod,
            magnetised_strength,
        }
    }

    pub fn process<'a>(
        &mut self,
        current: &OsuDifficultyObject<'a>,
        objects: &[OsuDifficultyObject<'a>],
    ) {
        <Self as VariableLengthStrainSkill>::process(self, current, objects);
    }

    pub fn difficulty_to_performance(difficulty: f64) -> f64 {
        4.0 * difficulty.powi(3)
    }

    pub fn object_difficulties(&self) -> &[f64] {
        self.variable.object_difficulties()
    }

    pub fn count_top_weighted_strains(&self, difficulty_value: f64) -> f64 {
        self.variable.count_top_weighted_strains(difficulty_value)
    }

    pub fn get_difficult_sliders(&self) -> f64 {
        if self.slider_strains.is_empty() {
            return 0.0;
        }

        let max_slider_strain = self.slider_strains.iter().copied().fold(0.0, f64::max);

        if max_slider_strain == 0.0 {
            return 0.0;
        }

        self.slider_strains
            .iter()
            .map(|&strain| 1.0 / (1.0 + (-(strain / max_slider_strain * 12.0 - 6.0)).exp()))
            .sum()
    }

    pub fn count_top_weighted_sliders(&self, difficulty_value: f64) -> f64 {
        if self.slider_strains.is_empty() {
            return 0.0;
        }

        let consistent_top_strain = difficulty_value * (1.0 - self.variable.decay_weight());

        if consistent_top_strain == 0.0 {
            return 0.0;
        }

        self.slider_strains
            .iter()
            .map(|&strain| logistic(strain / consistent_top_strain, 0.88, 10.0, Some(1.1)))
            .sum()
    }

    pub fn difficulty_value(&mut self) -> f64 {
        let mut difficulty = 0.0;
        let mut time = 0.0;
        let decay_weight = self.variable.decay_weight();
        let max_section_length = self.variable.max_section_length();

        for strain in self.reduced_strain_peaks() {
            let start_time = time;
            let end_time = time + strain.section_length / max_section_length;
            let weight = decay_weight.powf(start_time) - decay_weight.powf(end_time);

            difficulty += strain.value * weight;
            time = end_time;
        }

        difficulty / (1.0 - decay_weight)
    }

    fn reduced_strain_peaks(&mut self) -> Vec<StrainPeak> {
        let mut strains = self.variable.current_strain_peaks();
        strains.retain(|strain| strain.value > 0.0);

        const CHUNK_SIZE: f64 = 20.0;

        let mut time = 0.0;
        let mut skip_count = 0;

        // This deliberately tests the growing list. The target appends reduced
        // chunks to the same list and may process those appended chunks again.
        while skip_count < strains.len() && time < Self::REDUCED_SECTION_TIME {
            let strain = strains[skip_count];
            let mut added_time = 0.0;

            while added_time < strain.section_length {
                let progress = ((time + added_time) / Self::REDUCED_SECTION_TIME).clamp(0.0, 1.0);
                let scale = lerp(1.0, 10.0, progress).log10();

                strains.push(StrainPeak::new(
                    strain.value * lerp(Self::REDUCED_STRAIN_BASELINE, 1.0, scale),
                    CHUNK_SIZE.min(strain.section_length - added_time),
                ));

                added_time += CHUNK_SIZE;
            }

            time += strain.section_length;
            skip_count += 1;
        }

        strains.drain(..skip_count);
        strains.sort_by(|a, b| b.value.total_cmp(&a.value));
        strains
    }

    fn calculate_adjusted_difficulty(
        &self,
        current: &OsuDifficultyObject<'_>,
        objects: &[OsuDifficultyObject<'_>],
    ) -> f64 {
        const SKILL_MULTIPLIER_SNAP: f64 = 70.9;
        const SKILL_MULTIPLIER_AGILITY: f64 = 2.35;
        const SKILL_MULTIPLIER_FLOW: f64 = 242.0;

        let snap_difficulty =
            SnapAimEvaluator::evaluate_diff_of(current, objects, self.include_sliders)
                * SKILL_MULTIPLIER_SNAP;
        let agility_difficulty =
            AgilityEvaluator::evaluate_diff_of(current, objects) * SKILL_MULTIPLIER_AGILITY;
        let flow_difficulty = FlowAimEvaluator::evaluate_diff_of(
            current,
            objects,
            self.include_sliders,
            self.object_radius,
        ) * SKILL_MULTIPLIER_FLOW;

        let mut total_difficulty =
            self.calculate_total_value(snap_difficulty, agility_difficulty, flow_difficulty);

        if let Some(magnetised_strength) = self.magnetised_strength {
            total_difficulty *= 1.0 - magnetised_strength;
        }

        let overall_difficulty = self.overall_difficulty.max(0.0);
        total_difficulty *= 0.985 + overall_difficulty * overall_difficulty / 4000.0;

        total_difficulty
    }

    fn calculate_total_value(
        &self,
        mut snap_difficulty: f64,
        agility_difficulty: f64,
        mut flow_difficulty: f64,
    ) -> f64 {
        const SKILL_MULTIPLIER_TOTAL: f64 = 1.12;
        const COMBINED_SNAP_NORM_EXPONENT: f64 = 1.2;

        let mut combined_snap_difficulty = norm(
            COMBINED_SNAP_NORM_EXPONENT,
            [snap_difficulty, agility_difficulty],
        );

        // This probability must be calculated before TD and RX adjustments.
        let p_snap =
            Self::calculate_snap_flow_probability(flow_difficulty / combined_snap_difficulty);
        let p_flow = 1.0 - p_snap;

        if self.has_touch_device_mod {
            snap_difficulty = snap_difficulty.powf(0.89);
            combined_snap_difficulty = norm(
                COMBINED_SNAP_NORM_EXPONENT,
                [snap_difficulty, agility_difficulty],
            );
        }

        if self.has_relax_mod {
            combined_snap_difficulty *= 0.75;
            flow_difficulty *= 0.6;
        }

        (combined_snap_difficulty * p_snap + flow_difficulty * p_flow) * SKILL_MULTIPLIER_TOTAL
    }

    fn calculate_snap_flow_probability(ratio: f64) -> f64 {
        const K: f64 = 7.27;

        if ratio == 0.0 {
            return 0.0;
        }

        if ratio.is_nan() {
            return 1.0;
        }

        logistic_exp(-K * ratio.ln(), None)
    }

    fn strain_decay(ms: f64) -> f64 {
        0.2_f64.powf(ms / 1000.0)
    }
}

impl VariableLengthStrainSkill for Aim {
    type DifficultyObject<'a> = OsuDifficultyObject<'a>;
    type DifficultyObjects<'a> = [OsuDifficultyObject<'a>];

    fn variable_length_strain(&self) -> &VariableLengthStrain {
        &self.variable
    }

    fn variable_length_strain_mut(&mut self) -> &mut VariableLengthStrain {
        &mut self.variable
    }

    fn calculate_initial_strain<'a>(
        &self,
        time: f64,
        current: &Self::DifficultyObject<'a>,
        objects: &Self::DifficultyObjects<'a>,
    ) -> f64 {
        let previous_start_time = current
            .previous(0, objects)
            .map_or(0.0, |previous| previous.start_time);

        self.current_strain * Self::strain_decay(time - previous_start_time)
    }

    fn strain_value_at<'a>(
        &mut self,
        current: &Self::DifficultyObject<'a>,
        objects: &Self::DifficultyObjects<'a>,
    ) -> f64 {
        if self.has_autopilot_mod {
            return 0.0;
        }

        let decay = Self::strain_decay(current.adjusted_delta_time);

        self.current_strain *= decay;
        self.current_strain += self.calculate_adjusted_difficulty(current, objects) * (1.0 - decay);

        if current.base.is_slider() {
            self.slider_strains.push(self.current_strain);
        }

        self.current_strain
    }
}

const fn lerp(start: f64, end: f64, amount: f64) -> f64 {
    start + (end - start) * amount
}
