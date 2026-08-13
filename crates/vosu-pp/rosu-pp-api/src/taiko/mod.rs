use rosu_map::section::general::GameMode;

use crate::{
    Difficulty,
    any::CalculateError,
    model::{
        beatmap::Beatmap,
        mode::{ConvertError, IGameMode},
    },
};

pub use self::{
    attributes::{TaikoDifficultyAttributes, TaikoPerformanceAttributes},
    difficulty::gradual::TaikoGradualDifficulty,
    performance::{InspectTaikoPerformance, TaikoPerformance, gradual::TaikoGradualPerformance},
    score_state::{TaikoHitResults, TaikoScoreState},
    strains::TaikoStrains,
};

mod attributes;
mod convert;
mod difficulty;
mod object;
mod performance;
mod score_state;
mod strains;

#[cfg(feature = "oracle-debug")]
/// A taiko object produced while converting one source hit object.
#[derive(Clone, Debug, PartialEq)]
pub struct ConversionObject {
    pub start_time: f64,
    pub end_time: f64,
    pub is_rim: bool,
    pub is_centre: bool,
    pub is_drum_roll: bool,
    pub is_swell: bool,
    pub is_strong: bool,
}

#[cfg(feature = "oracle-debug")]
/// Objects produced by one source hit object during taiko conversion.
#[derive(Clone, Debug, PartialEq)]
pub struct ConversionMapping {
    pub start_time: f64,
    pub objects: Vec<ConversionObject>,
}

#[cfg(feature = "oracle-debug")]
/// Convert hit objects while retaining the source-to-converted-object mapping.
pub fn conversion_debug(
    map: &Beatmap,
    mods: &crate::GameMods,
) -> Result<Vec<ConversionMapping>, String> {
    convert::conversion_debug(map, mods)
}

/// Marker type for [`GameMode::Taiko`].
///
/// [`GameMode::Taiko`]: rosu_map::section::general::GameMode::Taiko
pub struct Taiko;

impl Taiko {
    pub fn convert(map: &mut Beatmap) {
        debug_assert!(!map.is_convert && map.mode == GameMode::Osu);
        convert::convert(map);
    }
}

impl IGameMode for Taiko {
    type DifficultyAttributes = TaikoDifficultyAttributes;
    type Strains = TaikoStrains;
    type Performance<'map> = TaikoPerformance<'map>;
    type HitResults = TaikoHitResults;
    type GradualDifficulty = TaikoGradualDifficulty;
    type GradualPerformance = TaikoGradualPerformance;

    fn difficulty(
        difficulty: &Difficulty,
        map: &Beatmap,
    ) -> Result<Self::DifficultyAttributes, ConvertError> {
        difficulty::difficulty(difficulty, map)
    }

    fn checked_difficulty(
        difficulty: &Difficulty,
        map: &Beatmap,
    ) -> Result<Self::DifficultyAttributes, CalculateError> {
        difficulty::checked_difficulty(difficulty, map)
    }

    fn strains(difficulty: &Difficulty, map: &Beatmap) -> Result<Self::Strains, ConvertError> {
        strains::strains(difficulty, map)
    }

    fn performance(map: &Beatmap) -> Self::Performance<'_> {
        TaikoPerformance::new(map)
    }

    fn gradual_difficulty(
        difficulty: Difficulty,
        map: &Beatmap,
    ) -> Result<Self::GradualDifficulty, ConvertError> {
        TaikoGradualDifficulty::new(difficulty, map)
    }

    fn gradual_performance(
        difficulty: Difficulty,
        map: &Beatmap,
    ) -> Result<Self::GradualPerformance, ConvertError> {
        TaikoGradualPerformance::new(difficulty, map)
    }
}
