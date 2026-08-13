use crate::{
    any::difficulty::object::{HasStartTime, IDifficultyObject},
    model::mods::GameMods,
    osu::difficulty::{
        evaluators::{AgilityEvaluator, FlowAimEvaluator, SnapAimEvaluator},
        object::OsuDifficultyObject,
    },
    util::difficulty::{logistic, logistic_exp, norm},
};

#[derive(Clone, Copy)]
struct StrainPeak {
    value: f64,
    length: f64,
}

#[derive(Clone)]
pub struct Aim {
    include_sliders: bool,
    has_autopilot_mod: bool,
    has_relax_mod: bool,
    has_touch_device_mod: bool,
    attraction_strength: Option<f64>,
    overall_difficulty: f64,
    current_strain: f64,
    current_section_peak: f64,
    current_section_begin: f64,
    current_section_end: f64,
    strain_peaks: Vec<StrainPeak>,
    fixed_current_section_peak: f64,
    fixed_current_section_end: f64,
    fixed_strain_peaks: Vec<f64>,
    total_length: f64,
    queued_strains: Vec<(f64, f64)>,
    object_strains: Vec<f64>,
    slider_strains: Vec<f64>,
}

impl Aim {
    const DECAY_WEIGHT: f64 = 0.9;
    const MAX_SECTION_LENGTH: f64 = 400.0;
    const MAX_STORED_LENGTH: f64 = 11.0 / (1.0 - Self::DECAY_WEIGHT);
    const REDUCED_SECTION_TIME: f64 = 4_000.0;
    const REDUCED_STRAIN_BASELINE: f64 = 0.727;

    pub fn new(include_sliders: bool, mods: &GameMods, overall_difficulty: f64) -> Self {
        Self {
            include_sliders,
            has_autopilot_mod: mods.ap(),
            has_relax_mod: mods.rx(),
            has_touch_device_mod: mods.td(),
            attraction_strength: mods.attraction_strength(),
            overall_difficulty,
            current_strain: 0.0,
            current_section_peak: 0.0,
            current_section_begin: 0.0,
            current_section_end: 0.0,
            fixed_current_section_peak: 0.0,
            fixed_current_section_end: 0.0,
            fixed_strain_peaks: Vec::with_capacity(256),
            strain_peaks: Vec::with_capacity(256),
            total_length: 0.0,
            queued_strains: Vec::with_capacity(32),
            object_strains: Vec::with_capacity(256),
            slider_strains: Vec::with_capacity(64),
        }
    }

    pub fn process<'a>(
        &mut self,
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) {
        if curr.idx == 0 {
            self.current_section_begin = curr.start_time;
            self.current_section_end = self.current_section_begin + Self::MAX_SECTION_LENGTH;
            self.fixed_current_section_end =
                (curr.start_time / Self::MAX_SECTION_LENGTH).ceil() * Self::MAX_SECTION_LENGTH;
        }

        while curr.start_time > self.fixed_current_section_end {
            self.fixed_strain_peaks
                .push(self.fixed_current_section_peak);
            let previous_start = curr
                .previous(0, objects)
                .map_or(0.0, HasStartTime::start_time);
            self.fixed_current_section_peak = self.current_strain
                * 0.15_f64.powf((self.fixed_current_section_end - previous_start) / 1_000.0);
            self.fixed_current_section_end += Self::MAX_SECTION_LENGTH;
        }

        if curr.idx > 0 {
            self.backfill_peaks(curr, objects);
        }

        let strain = self.strain_value_at(curr, objects);
        self.fixed_current_section_peak = self.fixed_current_section_peak.max(strain);

        if curr.idx == 0 {
            self.current_section_peak = strain;
        } else if strain > self.current_section_peak {
            self.queued_strains.clear();
            self.save_current_peak(curr.start_time - self.current_section_begin);
            self.current_section_begin = curr.start_time;
            self.current_section_end = self.current_section_begin + Self::MAX_SECTION_LENGTH;
            self.current_section_peak = strain;
        } else {
            while self
                .queued_strains
                .last()
                .is_some_and(|&(value, _)| value < strain)
            {
                self.queued_strains.pop();
            }
            self.queued_strains.push((strain, curr.start_time));
        }

        self.object_strains.push(strain);
    }

    fn backfill_peaks<'a>(
        &mut self,
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) {
        while curr.start_time > self.current_section_end {
            self.save_current_peak(self.current_section_end - self.current_section_begin);
            self.current_section_begin = self.current_section_end;

            if !self.queued_strains.is_empty() {
                let (strain, start_time) = self.queued_strains.remove(0);
                self.current_section_end = start_time + Self::MAX_SECTION_LENGTH;
                self.start_new_section_from(self.current_section_begin, curr, objects);
                self.current_section_peak = self.current_section_peak.max(strain);
            } else {
                self.current_section_end = self.current_section_begin + Self::MAX_SECTION_LENGTH;
                self.start_new_section_from(self.current_section_begin, curr, objects);
            }
        }
    }

    fn start_new_section_from<'a>(
        &mut self,
        time: f64,
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) {
        let previous_start = curr
            .previous(0, objects)
            .map_or(0.0, HasStartTime::start_time);
        self.current_section_peak =
            self.current_strain * 0.2_f64.powf((time - previous_start) / 1_000.0);
    }

    fn strain_value_at<'a>(
        &mut self,
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) -> f64 {
        if self.has_autopilot_mod {
            return 0.0;
        }
        let decay = 0.2_f64.powf(curr.adjusted_delta_time / 1_000.0);
        self.current_strain *= decay;
        self.current_strain += self.adjusted_difficulty(curr, objects) * (1.0 - decay);
        if curr.base.is_slider() {
            self.slider_strains.push(self.current_strain);
        }
        self.current_strain
    }

    fn adjusted_difficulty<'a>(
        &self,
        curr: &'a OsuDifficultyObject<'a>,
        objects: &'a [OsuDifficultyObject<'a>],
    ) -> f64 {
        let snap = SnapAimEvaluator::evaluate_diff_of(curr, objects, self.include_sliders) * 70.9;
        let agility = AgilityEvaluator::evaluate_diff_of(curr, objects) * 2.35;
        let mut flow =
            FlowAimEvaluator::evaluate_diff_of(curr, objects, self.include_sliders) * 242.0;
        let mut combined = norm(1.2, [snap, agility]);
        let ratio = flow / combined;
        let p_snap = if ratio == 0.0 {
            0.0
        } else if ratio.is_nan() {
            1.0
        } else {
            logistic_exp(-7.27 * ratio.ln(), None)
        };

        if self.has_touch_device_mod {
            combined = norm(1.2, [snap.powf(0.89), agility]);
        }

        if self.has_relax_mod {
            combined *= 0.75;
            flow *= 0.6;
        }

        let mut total = (combined * p_snap + flow * (1.0 - p_snap)) * 1.12;

        if let Some(strength) = self.attraction_strength {
            total *= 1.0 - strength;
        }

        total * (0.985 + self.overall_difficulty.max(0.0).powi(2) / 4_000.0)
    }

    fn save_current_peak(&mut self, length: f64) {
        let peak = StrainPeak {
            value: self.current_section_peak,
            length: length.round_ties_even(),
        };
        let index = self
            .strain_peaks
            .binary_search_by(|other| other.value.total_cmp(&peak.value).reverse())
            .unwrap_or_else(|index| index);
        self.strain_peaks.insert(index, peak);
        self.total_length += peak.length;
        while self.total_length > Self::MAX_STORED_LENGTH * Self::MAX_SECTION_LENGTH {
            let removed = self
                .strain_peaks
                .pop()
                .expect("stored strain length exceeds retained peaks");
            self.total_length -= removed.length;
        }
    }

    fn current_peaks(&self) -> Vec<StrainPeak> {
        let mut this = self.clone();
        this.save_current_peak(this.current_section_end - this.current_section_begin);
        this.strain_peaks
    }

    fn reduced_peaks(&self) -> Vec<StrainPeak> {
        let mut strains: Vec<_> = self
            .current_peaks()
            .into_iter()
            .filter(|peak| peak.value > 0.0)
            .collect();
        let mut time = 0.0;
        let mut skip = 0;
        while skip < strains.len() && time < Self::REDUCED_SECTION_TIME {
            let strain = strains[skip];
            let mut added = 0.0;
            while added < strain.length {
                let scale = (1.0
                    + 9.0 * ((time + added) / Self::REDUCED_SECTION_TIME).clamp(0.0, 1.0))
                .log10();
                strains.push(StrainPeak {
                    value: strain.value
                        * (Self::REDUCED_STRAIN_BASELINE
                            + (1.0 - Self::REDUCED_STRAIN_BASELINE) * scale),
                    length: 20.0_f64.min(strain.length - added),
                });
                added += 20.0;
            }
            time += strain.length;
            skip += 1;
        }
        let mut result = strains.split_off(skip);
        result.sort_unstable_by(|a, b| b.value.total_cmp(&a.value));
        result
    }

    pub fn cloned_difficulty_value(&self) -> f64 {
        let mut difficulty = 0.0;
        let mut time = 0.0;
        for peak in self.reduced_peaks() {
            let end = time + peak.length / Self::MAX_SECTION_LENGTH;
            difficulty +=
                peak.value * (Self::DECAY_WEIGHT.powf(time) - Self::DECAY_WEIGHT.powf(end));
            time = end;
        }

        difficulty / (1.0 - Self::DECAY_WEIGHT)
    }

    pub fn count_top_weighted_strains(&self, difficulty: f64) -> f64 {
        let consistent = difficulty * (1.0 - Self::DECAY_WEIGHT);
        if consistent == 0.0 {
            return self.object_strains.len() as f64;
        }
        self.object_strains
            .iter()
            .map(|&value| logistic(value / consistent, 0.88, 10.0, Some(1.1)))
            .sum()
    }

    pub fn count_top_weighted_sliders(&self, difficulty: f64) -> f64 {
        let consistent = difficulty * (1.0 - Self::DECAY_WEIGHT);
        if consistent == 0.0 {
            return 0.0;
        }
        self.slider_strains
            .iter()
            .map(|&value| logistic(value / consistent, 0.88, 10.0, Some(1.1)))
            .sum()
    }

    pub fn get_difficult_sliders(&self) -> f64 {
        self.slider_strains
            .iter()
            .copied()
            .reduce(f64::max)
            .filter(|&max| max > 0.0)
            .map_or(0.0, |max| {
                self.slider_strains
                    .iter()
                    .map(|&value| (1.0 + (-(value / max * 12.0 - 6.0)).exp()).recip())
                    .sum()
            })
    }

    pub fn into_current_strain_peaks(mut self) -> Vec<f64> {
        self.fixed_strain_peaks
            .push(self.fixed_current_section_peak);
        self.fixed_strain_peaks
    }

    pub fn current_strain_peaks(&self) -> Vec<f64> {
        let mut peaks = self.fixed_strain_peaks.clone();
        peaks.push(self.fixed_current_section_peak);
        peaks
    }

    pub fn difficulty_to_performance(difficulty: f64) -> f64 {
        4.0 * difficulty.powi(3)
    }
}
