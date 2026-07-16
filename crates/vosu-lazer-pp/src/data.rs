use rosu_pp::any::{DifficultyAttributes, PerformanceAttributes, Strains};
use serde::{Deserialize, Serialize};

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
    pub slider_factor: f64,
    pub speed_note_count: f64,
    pub aim_difficult_slider_count: f64,
    pub aim_top_weighted_slider_factor: f64,
    pub aim_difficult_strain_count: f64,
    pub speed_difficult_strain_count: f64,
    pub n_circles: i32,
    pub n_sliders: i32,
    pub n_spinners: i32,
    pub reading: f64,
    pub stamina: f64,
    pub rhythm: f64,
    pub color: f64,
    /// Retained native attributes used by the wrapper's performance overload.
    ///
    /// The pinned wrapper carries `DifficultyAttributes.Inner` alongside its
    /// projected wire fields and passes that exact object into performance
    /// calculation. This field is deliberately excluded from serialization.
    #[serde(skip)]
    pub(crate) inner: Option<DifficultyAttributes>,
}

impl From<&DifficultyAttributes> for DifficultyAttrsData {
    fn from(attrs: &DifficultyAttributes) -> Self {
        let mut data = match attrs {
            DifficultyAttributes::Osu(attrs) => Self {
                stars: attrs.stars,
                max_combo: attrs.max_combo as i32,
                aim: attrs.aim,
                speed: attrs.speed,
                flashlight: attrs.flashlight,
                slider_factor: attrs.slider_factor,
                speed_note_count: attrs.speed_note_count,
                aim_difficult_slider_count: attrs.aim_difficult_slider_count,
                aim_top_weighted_slider_factor: attrs.aim_top_weighted_slider_factor,
                aim_difficult_strain_count: attrs.aim_difficult_strain_count,
                speed_difficult_strain_count: attrs.speed_difficult_strain_count,
                n_circles: attrs.n_circles as i32,
                n_sliders: attrs.n_sliders as i32,
                n_spinners: attrs.n_spinners as i32,
                reading: attrs.reading,
                ..Self::default()
            },
            DifficultyAttributes::Taiko(attrs) => Self {
                stars: attrs.stars,
                max_combo: attrs.max_combo as i32,
                reading: attrs.reading,
                stamina: attrs.stamina,
                rhythm: attrs.rhythm,
                color: attrs.color,
                ..Self::default()
            },
            DifficultyAttributes::Catch(attrs) => Self {
                stars: attrs.stars,
                max_combo: attrs.max_combo() as i32,
                ..Self::default()
            },
            DifficultyAttributes::Mania(attrs) => Self {
                stars: attrs.stars,
                max_combo: attrs.max_combo as i32,
                ..Self::default()
            },
        };

        data.inner = Some(attrs.clone());

        data
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
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

impl From<&PerformanceAttributes> for PerformanceAttrsData {
    fn from(attrs: &PerformanceAttributes) -> Self {
        match attrs {
            PerformanceAttributes::Osu(attrs) => Self {
                pp: attrs.pp,
                aim: attrs.pp_aim,
                flashlight: attrs.pp_flashlight,
                speed: attrs.pp_speed,
                reading: attrs.pp_reading,
                accuracy: attrs.pp_acc,
                effective_miss_count: attrs.effective_miss_count,
                ..Self::default()
            },
            PerformanceAttributes::Taiko(attrs) => Self {
                pp: attrs.pp,
                // Preserve the pinned wrapper's field-assignment quirk.
                effective_miss_count: attrs.estimated_unstable_rate.unwrap_or(-1.0),
                pp_difficulty: attrs.pp_difficulty,
                ..Self::default()
            },
            PerformanceAttributes::Catch(attrs) => Self {
                pp: attrs.pp,
                ..Self::default()
            },
            PerformanceAttributes::Mania(attrs) => Self {
                pp: attrs.pp,
                pp_difficulty: attrs.pp_difficulty,
                ..Self::default()
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeakStrains {
    pub value: Vec<f64>,
    pub section_length: i32,
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

fn compat_strain_peaks(
    object_strains: &[f64],
    object_start_times: &[f64],
    previous_object_start_times: &[f64],
) -> Vec<f64> {
    const SECTION_LENGTH: f64 = 400.0;
    const STRAIN_DECAY_BASE: f64 = 0.15;

    debug_assert_eq!(object_strains.len(), object_start_times.len());
    if object_strains.is_empty() {
        return Vec::new();
    }
    debug_assert_eq!(object_strains.len(), previous_object_start_times.len());

    let mut strain_peaks = Vec::new();
    let mut current_section_peak = 0.0_f64;
    let mut current_section_end = 0.0_f64;
    let mut current_strain = 0.0_f64;

    for (index, ((&object_strain, &start_time), &previous_start_time)) in object_strains
        .iter()
        .zip(object_start_times)
        .zip(previous_object_start_times)
        .enumerate()
    {
        if index == 0 {
            current_section_end = (start_time / SECTION_LENGTH).ceil() * SECTION_LENGTH;
        }

        while start_time > current_section_end {
            strain_peaks.push(current_section_peak);
            current_section_peak = current_strain
                * STRAIN_DECAY_BASE.powf((current_section_end - previous_start_time) / 1000.0);
            current_section_end += SECTION_LENGTH;
        }

        current_strain = object_strain;
        current_section_peak = current_section_peak.max(current_strain);
    }

    strain_peaks.push(current_section_peak);

    strain_peaks
}

impl From<Strains> for StrainsData {
    fn from(strains: Strains) -> Self {
        let section_length = strains.section_len() as i32;
        let peak = |value: Vec<f64>| PeakStrains {
            value,
            section_length,
        };

        match strains {
            Strains::Osu(strains) => {
                let aim = compat_strain_peaks(
                    &strains.aim,
                    &strains.object_start_times,
                    &strains.previous_object_start_times,
                );
                let aim_without_sliders = compat_strain_peaks(
                    &strains.aim_no_sliders,
                    &strains.object_start_times,
                    &strains.previous_object_start_times,
                );
                let speed = compat_strain_peaks(
                    &strains.speed,
                    &strains.object_start_times,
                    &strains.previous_object_start_times,
                );
                let reading = compat_strain_peaks(
                    &strains.reading,
                    &strains.object_start_times,
                    &strains.previous_object_start_times,
                );

                Self {
                    aim: peak(aim),
                    aim_without_sliders: peak(aim_without_sliders),
                    flashlight: peak(strains.flashlight),
                    speed: peak(speed),
                    reading: peak(reading),
                    ..Self::default()
                }
            }
            Strains::Taiko(strains) => Self {
                color: peak(strains.color),
                reading: peak(strains.reading),
                rhythm: peak(strains.rhythm),
                // StrainsData iterates the pinned taiko skills in declaration
                // order. Single-color stamina is last and overwrites regular
                // stamina under the shared `Stamina` DTO key.
                stamina: peak(strains.single_color_stamina),
                ..Self::default()
            },
            Strains::Catch(strains) => Self {
                movement: peak(strains.movement),
                ..Self::default()
            },
            Strains::Mania(strains) => Self {
                strains: peak(strains.strains),
                ..Self::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use rosu_pp::{any::Strains, taiko::TaikoStrains};

    use super::StrainsData;

    #[test]
    fn taiko_single_color_stamina_overwrites_regular_stamina() {
        let data = StrainsData::from(Strains::Taiko(TaikoStrains {
            color: vec![1.0],
            reading: vec![2.0],
            rhythm: vec![3.0],
            stamina: vec![4.0],
            single_color_stamina: vec![9.0],
        }));

        assert_eq!(data.stamina.value, [9.0]);
        assert_ne!(data.stamina.value, [4.0]);
    }
}
