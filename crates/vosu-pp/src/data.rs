use rosu_pp::any::{DifficultyAttributes, PerformanceAttributes, Strains};
use serde::{Deserialize, Serialize};

use crate::{Error, Ruleset};

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatmapDifficultyData {
    pub approach_rate: f32,
    pub circle_size: f32,
    pub overall_difficulty: f32,
    pub drain_rate: f32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DifficultyAttrsData {
    pub stars: f64,
    pub max_combo: i32,
    pub aim: f64,
    pub speed: f64,
    pub flashlight: f64,
    pub color: f64,
    pub slider_factor: f64,
    pub speed_note_count: f64,
    pub aim_difficult_slider_count: f64,
    pub aim_top_weighted_slider_factor: f64,
    #[serde(skip)]
    pub speed_top_weighted_slider_factor: f64,
    pub aim_difficult_strain_count: f64,
    pub speed_difficult_strain_count: f64,
    pub n_circles: i32,
    pub n_sliders: i32,
    pub n_spinners: i32,
    pub reading: f64,
    pub stamina: f64,
    pub rhythm: f64,
    #[serde(skip)]
    pub n_fruits: i32,
    #[serde(skip)]
    pub n_droplets: i32,
    #[serde(skip)]
    pub n_tiny_droplets: i32,
    #[serde(skip)]
    pub n_large_ticks: i32,
    #[serde(skip)]
    pub n_objects: i32,
    #[serde(skip)]
    pub n_hold_notes: i32,
    #[serde(skip)]
    pub(crate) mode: Ruleset,
    #[serde(skip)]
    pub(crate) inner: Option<DifficultyAttributes>,
    #[serde(skip)]
    pub(crate) context: Option<rosu_pp::Difficulty>,
}

impl DifficultyAttrsData {
    pub(crate) fn from_inner(inner: DifficultyAttributes) -> Self {
        let mut value = Self {
            stars: inner.stars(),
            max_combo: inner.max_combo() as i32,
            mode: mode_of(&inner),
            inner: Some(inner.clone()),
            ..Self::default()
        };
        match inner {
            DifficultyAttributes::Osu(a) => {
                value.aim = a.aim;
                value.speed = a.speed;
                value.flashlight = a.flashlight;
                value.reading = a.reading;
                value.slider_factor = a.slider_factor;
                value.speed_note_count = a.speed_note_count;
                value.aim_difficult_slider_count = a.aim_difficult_slider_count;
                value.aim_top_weighted_slider_factor = a.aim_top_weighted_slider_factor;
                value.speed_top_weighted_slider_factor = a.speed_top_weighted_slider_factor;
                value.aim_difficult_strain_count = a.aim_difficult_strain_count;
                value.speed_difficult_strain_count = a.speed_difficult_strain_count;
                value.n_circles = a.n_circles as i32;
                value.n_sliders = a.n_sliders as i32;
                value.n_spinners = a.n_spinners as i32;
                value.n_large_ticks = a.n_large_ticks as i32;
                value.n_objects = a.n_objects() as i32;
            }
            DifficultyAttributes::Taiko(a) => {
                value.stamina = a.stamina;
                value.rhythm = a.rhythm;
                value.color = a.color;
                value.reading = a.reading;
                value.n_objects = a.max_combo as i32;
            }
            DifficultyAttributes::Catch(a) => {
                value.n_fruits = a.n_fruits as i32;
                value.n_droplets = a.n_droplets as i32;
                value.n_tiny_droplets = a.n_tiny_droplets as i32;
                value.n_objects = (a.n_fruits + a.n_droplets) as i32;
            }
            DifficultyAttributes::Mania(a) => {
                value.n_objects = a.n_objects as i32;
                value.n_hold_notes = a.n_hold_notes as i32;
            }
        }
        value
    }

    pub(crate) fn inner_for(&self, mode: Ruleset) -> Result<DifficultyAttributes, Error> {
        if self.mode != mode {
            return Err(Error::DifficultyModeMismatch {
                expected: mode,
                actual: self.mode,
            });
        }
        self.inner.clone().ok_or(Error::DifficultyUnavailable)
    }
}

pub(crate) const fn mode_of(attrs: &DifficultyAttributes) -> Ruleset {
    match attrs {
        DifficultyAttributes::Osu(_) => Ruleset::Osu,
        DifficultyAttributes::Taiko(_) => Ruleset::Taiko,
        DifficultyAttributes::Catch(_) => Ruleset::Catch,
        DifficultyAttributes::Mania(_) => Ruleset::Mania,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceAttrsData {
    pub pp: f64,
    pub aim: f64,
    pub flashlight: f64,
    pub speed: f64,
    pub reading: f64,
    pub accuracy: f64,
    pub effective_miss_count: f64,
    pub estimated_unstable_rate: f64,
    pub speed_deviation: f64,
    pub pp_difficulty: f64,
}

impl From<PerformanceAttributes> for PerformanceAttrsData {
    fn from(value: PerformanceAttributes) -> Self {
        match value {
            PerformanceAttributes::Osu(a) => Self {
                pp: a.pp,
                aim: a.pp_aim,
                flashlight: a.pp_flashlight,
                speed: a.pp_speed,
                reading: a.pp_reading,
                accuracy: a.pp_acc,
                effective_miss_count: a.effective_miss_count,
                ..Self::default()
            },
            PerformanceAttributes::Taiko(a) => Self {
                pp: a.pp,
                effective_miss_count: a.estimated_unstable_rate.unwrap_or(-1.0),
                pp_difficulty: a.pp_difficulty,
                ..Self::default()
            },
            PerformanceAttributes::Catch(a) => Self {
                pp: a.pp,
                ..Self::default()
            },
            PerformanceAttributes::Mania(a) => Self {
                pp: a.pp,
                pp_difficulty: a.pp_difficulty,
                ..Self::default()
            },
        }
    }
}
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeakStrains {
    pub value: Vec<f64>,
    pub section_length: f64,
}

fn peak_strains(value: Vec<f64>, section_length: f64) -> PeakStrains {
    PeakStrains {
        section_length: if value.is_empty() {
            0.0
        } else {
            section_length
        },
        value,
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StrainsData {
    pub aim: PeakStrains,
    pub aim_without_sliders: PeakStrains,
    pub flashlight: PeakStrains,
    pub speed: PeakStrains,
    pub reading: PeakStrains,
    pub color: PeakStrains,
    pub rhythm: PeakStrains,
    pub stamina: PeakStrains,
    pub movement: PeakStrains,
    pub strains: PeakStrains,
}

impl From<Strains> for StrainsData {
    fn from(value: Strains) -> Self {
        match value {
            Strains::Osu(s) => {
                let len = rosu_pp::osu::OsuStrains::SECTION_LEN;
                Self {
                    aim: peak_strains(s.aim, len),
                    aim_without_sliders: peak_strains(s.aim_no_sliders, len),
                    flashlight: peak_strains(s.flashlight, len),
                    speed: peak_strains(s.speed, len),
                    reading: peak_strains(s.reading, len),
                    ..Self::default()
                }
            }
            Strains::Taiko(s) => {
                let len = rosu_pp::taiko::TaikoStrains::SECTION_LEN;
                Self {
                    color: peak_strains(s.color, len),
                    reading: peak_strains(s.reading, len),
                    rhythm: peak_strains(s.rhythm, len),
                    stamina: peak_strains(s.single_color_stamina, len),
                    ..Self::default()
                }
            }
            Strains::Catch(s) => Self {
                movement: peak_strains(s.movement, rosu_pp::catch::CatchStrains::SECTION_LEN),
                ..Self::default()
            },
            Strains::Mania(s) => Self {
                strains: peak_strains(s.strains, rosu_pp::mania::ManiaStrains::SECTION_LEN),
                ..Self::default()
            },
        }
    }
}
