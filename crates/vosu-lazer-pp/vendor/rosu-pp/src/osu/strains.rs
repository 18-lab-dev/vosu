use crate::{
    Beatmap, Difficulty, any::difficulty::skills::StrainSkill, model::mode::ConvertError,
    osu::convert::prepare_map,
};

use super::difficulty::{DifficultyValues, skills::OsuSkills};

/// The result of calculating the strains on a osu! map.
///
/// Suitable to plot the difficulty of a map over time.
#[derive(Clone, Debug, PartialEq)]
pub struct OsuStrains {
    /// Per-object difficulties of the aim skill.
    pub aim: Vec<f64>,
    /// Per-object difficulties of the aim skill without sliders.
    pub aim_no_sliders: Vec<f64>,
    /// Per-object difficulties of the speed skill.
    pub speed: Vec<f64>,
    /// Per-object difficulties of the reading skill.
    pub reading: Vec<f64>,
    /// Strain peaks of the flashlight skill.
    pub flashlight: Vec<f64>,
    /// Start times of the difficulty objects represented by the per-object
    /// difficulty vectors.
    pub object_start_times: Vec<f64>,
    /// Start times of each difficulty object's immediately preceding object.
    pub previous_object_start_times: Vec<f64>,
}

impl OsuStrains {
    /// Time between two strains in ms.
    pub const SECTION_LEN: f64 = 400.0;
}

pub fn strains(difficulty: &Difficulty, map: &Beatmap) -> Result<OsuStrains, ConvertError> {
    let map = prepare_map(difficulty, map)?;

    let DifficultyValues {
        osu_objects,
        skills:
            OsuSkills {
                aim,
                aim_no_sliders,
                speed,
                flashlight,
                reading,
            },
        attrs: _,
    } = DifficultyValues::calculate(difficulty, &map);

    let object_count = difficulty.get_passed_objects().min(osu_objects.len());
    let clock_rate = difficulty.get_clock_rate();
    let previous_object_start_times = osu_objects
        .iter()
        .take(object_count.saturating_sub(1))
        .map(|object| object.start_time / clock_rate)
        .collect();
    let object_start_times = osu_objects
        .iter()
        .take(object_count)
        .skip(1)
        .map(|object| object.start_time / clock_rate)
        .collect();

    Ok(OsuStrains {
        aim: aim.object_difficulties().to_vec(),
        aim_no_sliders: aim_no_sliders.object_difficulties().to_vec(),
        speed: speed.object_difficulties().to_vec(),
        reading: reading.object_difficulties().to_vec(),
        flashlight: flashlight.into_current_strain_peaks(),
        object_start_times,
        previous_object_start_times,
    })
}
