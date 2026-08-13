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
    attributes::{CatchDifficultyAttributes, CatchPerformanceAttributes},
    difficulty::gradual::CatchGradualDifficulty,
    performance::{CatchPerformance, gradual::CatchGradualPerformance},
    score_state::{CatchHitResults, CatchScoreState},
    strains::CatchStrains,
};

mod attributes;
mod catcher;
mod convert;
mod difficulty;
mod object;
mod performance;
mod score_state;
mod strains;

pub(crate) const PLAYFIELD_WIDTH: f32 = 512.0;

/// Marker type for [`GameMode::Catch`].

impl Catch {
    pub fn convert(map: &mut Beatmap) {
        debug_assert!(!map.is_convert && map.mode == GameMode::Osu);
        convert::convert(map);
    }
}
pub struct Catch;

#[cfg(feature = "oracle-debug")]
/// A catch object produced while converting one source hit object.
#[derive(Clone, Debug, PartialEq)]
pub struct ConversionObject {
    pub start_time: f64,
    pub position: f64,
    pub hyper_dash: bool,
}

#[cfg(feature = "oracle-debug")]
/// Objects produced by one source hit object during catch conversion.
#[derive(Clone, Debug, PartialEq)]
pub struct ConversionMapping {
    pub start_time: f64,
    pub objects: Vec<ConversionObject>,
}

#[cfg(feature = "oracle-debug")]
pub fn conversion_debug(
    map: &Beatmap,
    mods: &crate::GameMods,
) -> Result<Vec<ConversionMapping>, String> {
    convert::conversion_debug(map, mods)
}

impl IGameMode for Catch {
    type DifficultyAttributes = CatchDifficultyAttributes;
    type Strains = CatchStrains;
    type Performance<'map> = CatchPerformance<'map>;
    type HitResults = CatchHitResults;
    type GradualDifficulty = CatchGradualDifficulty;
    type GradualPerformance = CatchGradualPerformance;

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
        CatchPerformance::new(map)
    }

    fn gradual_difficulty(
        difficulty: Difficulty,
        map: &Beatmap,
    ) -> Result<Self::GradualDifficulty, ConvertError> {
        CatchGradualDifficulty::new(difficulty, map)
    }

    fn gradual_performance(
        difficulty: Difficulty,
        map: &Beatmap,
    ) -> Result<Self::GradualPerformance, ConvertError> {
        CatchGradualPerformance::new(difficulty, map)
    }
}
