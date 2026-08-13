use crate::{
    any::difficulty::object::{HasStartTime, IDifficultyObject},
    model::mods::GameMods,
    osu::difficulty::{
        evaluators::{RhythmEvaluator, SpeedEvaluator},
        object::OsuDifficultyObject,
    },
    util::difficulty::logistic,
};

#[derive(Clone)]
struct CSharpCompatProjection {
    current_section_peak: f64,
    current_strain: f64,
    strain_peaks: Vec<f64>,
}

#[derive(Clone)]
pub struct Speed {
    object_difficulties: Vec<f64>,
    current_strain: f64,
    has_autopilot_mod: bool,
    hit_window: f64,
    has_relax_mod: bool,
    slider_strains: Vec<f64>,
    current_section_peak: f64,
    current_section_end: f64,
    strain_peaks: Vec<f64>,
    csharp_compat_projection: Option<CSharpCompatProjection>,
}

impl Speed {
    const SECTION_LENGTH: f64 = 400.0;

    fn strain_decay(ms: f64) -> f64 {
        0.3_f64.powf(ms / 1_000.0)
    }

    pub fn new(hit_window: f64, mods: &GameMods) -> Self {
        Self {
            object_difficulties: Vec::with_capacity(256),
            current_strain: 0.0,
            has_autopilot_mod: mods.ap(),
            hit_window,
            has_relax_mod: mods.rx(),
            slider_strains: Vec::with_capacity(64),
            current_section_peak: 0.0,
            current_section_end: 0.0,
            csharp_compat_projection: None,
            strain_peaks: Vec::with_capacity(64),
        }
    }
    pub fn enable_csharp_compat_projection(&mut self) {
        self.csharp_compat_projection = Some(CSharpCompatProjection {
            current_section_peak: 0.0,
            current_strain: 0.0,
            strain_peaks: Vec::with_capacity(64),
        });
    }

    pub fn process(&mut self, curr: &OsuDifficultyObject<'_>, objects: &[OsuDifficultyObject<'_>]) {
        if curr.idx == 0 {
            self.current_section_end =
                (curr.start_time / Self::SECTION_LENGTH).ceil() * Self::SECTION_LENGTH;
        }

        while curr.start_time > self.current_section_end {
            self.strain_peaks.push(self.current_section_peak);
            if let Some(projection) = self.csharp_compat_projection.as_mut() {
                projection
                    .strain_peaks
                    .push(projection.current_section_peak);
            }

            let previous_start_time = curr
                .previous(0, objects)
                .map_or(0.0, HasStartTime::start_time);

            self.current_section_peak = self.current_strain
                * Self::strain_decay(self.current_section_end - previous_start_time);
            if let Some(projection) = self.csharp_compat_projection.as_mut() {
                projection.current_section_peak = projection.current_strain
                    * 0.15_f64.powf((self.current_section_end - previous_start_time) / 1_000.0);
            }
            self.current_section_end += Self::SECTION_LENGTH;
        }
        let total_strain = if self.has_relax_mod {
            0.0
        } else {
            let decay = Self::strain_decay(curr.adjusted_delta_time);
            self.current_strain *= decay;
            let mut difficulty = SpeedEvaluator::evaluate_diff_of(curr, objects);
            if self.has_autopilot_mod {
                difficulty *= 0.5;
            }
            self.current_strain += difficulty * (1.0 - decay) * 1.16;
            self.current_strain * RhythmEvaluator::evaluate_diff_of(curr, objects, self.hit_window)
        };

        if curr.base.is_slider() {
            self.slider_strains.push(total_strain);
        }

        self.object_difficulties.push(total_strain);
        self.current_section_peak = self.current_section_peak.max(total_strain);
        if let Some(projection) = self.csharp_compat_projection.as_mut() {
            projection.current_section_peak = projection.current_section_peak.max(total_strain);
            projection.current_strain = total_strain;
        }
    }

    fn harmonic_difficulty(&self) -> (f64, f64) {
        let mut difficulties: Vec<f64> = self
            .object_difficulties
            .iter()
            .copied()
            .filter(|&value| value > 0.0)
            .collect();
        difficulties.sort_unstable_by(|a, b| b.total_cmp(a));

        difficulties.into_iter().enumerate().fold(
            (0.0, 0.0),
            |(difficulty, weight_sum), (index, value)| {
                let index = index as f64;
                let harmonic = 20.0 / (1.0 + index);
                let weight = (1.0 + harmonic) / (index.powf(0.9) + 1.0 + harmonic);
                (difficulty + value * weight, weight_sum + weight)
            },
        )
    }

    pub fn cloned_difficulty_value(&self) -> f64 {
        self.harmonic_difficulty().0
    }

    pub fn count_top_weighted_strains(&self, difficulty_value: f64) -> f64 {
        if self.object_difficulties.is_empty() {
            return 0.0;
        }

        let weight_sum = self.harmonic_difficulty().1;
        if weight_sum == 0.0 {
            return 0.0;
        }

        let consistent_top = difficulty_value / weight_sum;
        if consistent_top == 0.0 {
            return 0.0;
        }

        self.object_difficulties
            .iter()
            .map(|&value| logistic(value / consistent_top, 0.88, 10.0, Some(1.1)))
            .sum()
    }

    pub fn count_top_weighted_sliders(&self, difficulty_value: f64) -> f64 {
        if self.slider_strains.is_empty() {
            return 0.0;
        }

        let weight_sum = self.harmonic_difficulty().1;
        if weight_sum == 0.0 {
            return 0.0;
        }

        let consistent_top = difficulty_value / weight_sum;
        if consistent_top == 0.0 {
            return 0.0;
        }

        self.slider_strains
            .iter()
            .map(|&value| logistic(value / consistent_top, 0.88, 10.0, Some(1.1)))
            .sum()
    }

    pub fn relevant_note_count(&self) -> f64 {
        self.object_difficulties
            .iter()
            .copied()
            .reduce(f64::max)
            .filter(|&max| max > 0.0)
            .map_or(0.0, |max| {
                self.object_difficulties
                    .iter()
                    .map(|&value| (1.0 + (-(value / max * 12.0 - 6.0)).exp()).recip())
                    .sum()
            })
    }

    pub fn current_strain_peaks(&self) -> Vec<f64> {
        let mut peaks = self.strain_peaks.clone();
        peaks.push(self.current_section_peak);

        peaks
    }

    pub fn csharp_compat_strain_peaks(&self) -> Vec<f64> {
        let Some(projection) = self.csharp_compat_projection.as_ref() else {
            return self.current_strain_peaks();
        };

        let mut peaks = projection.strain_peaks.clone();
        peaks.push(projection.current_section_peak);

        peaks
    }

    pub fn into_current_strain_peaks(mut self) -> Vec<f64> {
        self.strain_peaks.push(self.current_section_peak);

        self.strain_peaks
    }
    pub fn into_csharp_compat_strain_peaks(self) -> Vec<f64> {
        let Some(mut projection) = self.csharp_compat_projection else {
            return self.into_current_strain_peaks();
        };

        projection
            .strain_peaks
            .push(projection.current_section_peak);

        projection.strain_peaks
    }

    pub fn difficulty_to_performance(difficulty: f64) -> f64 {
        4.0 * difficulty.powi(3)
    }
}
