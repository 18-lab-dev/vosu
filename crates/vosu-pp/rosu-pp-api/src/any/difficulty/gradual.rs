use rosu_map::section::general::GameMode;

use crate::{
    Beatmap, Difficulty,
    any::{DifficultyAttributes, Strains},
    catch::{Catch, CatchGradualDifficulty},
    mania::{Mania, ManiaGradualDifficulty},
    model::{
        beatmap::TooSuspicious,
        mode::{ConvertError, IGameMode},
    },
    osu::{Osu, OsuGradualDifficulty},
    taiko::{Taiko, TaikoGradualDifficulty},
};

/// Gradually calculate the difficulty attributes on maps of any mode.
///
/// Note that this type implements [`Iterator`]. On every call of
/// [`Iterator::next`], the next object will be processed and the
/// [`DifficultyAttributes`] will be updated and returned.
///
/// If you want to calculate performance attributes, use [`GradualPerformance`] instead.
///
/// # Example
///
/// ```
/// use rosu_pp::{Beatmap, GradualDifficulty, Difficulty};
///
/// let map = Beatmap::from_path("./resources/2785319.osu").unwrap();
/// let difficulty = Difficulty::new().mods(64); // DT
/// let mut iter = GradualDifficulty::new(difficulty, &map);
///
/// // the difficulty of the map after the first object
/// let attrs1 = iter.next();
/// // ... after the second object
/// let attrs2 = iter.next();
///
/// // Remaining objects
/// for difficulty in iter {
///     // ...
/// }
/// ```
///
/// [`GradualPerformance`]: crate::GradualPerformance
pub enum GradualDifficulty {
    Osu(OsuGradualDifficulty),
    Taiko(TaikoGradualDifficulty),
    Catch(CatchGradualDifficulty),
    Mania(ManiaGradualDifficulty),
}

impl GradualDifficulty {
    /// Create a [`GradualDifficulty`] for a map of any mode.
    #[expect(clippy::missing_panics_doc, reason = "unreachable")]
    pub fn new(difficulty: Difficulty, map: &Beatmap) -> Self {
        Self::new_with_mode(difficulty, map, map.mode).expect("no conversion required")
    }

    /// Same as [`GradualDifficulty::new`] but verifies that the map is not
    /// suspicious.
    pub fn checked_new(difficulty: Difficulty, map: &Beatmap) -> Result<Self, TooSuspicious> {
        // This is fine because `Self::new` will use the map's mode so it won't
        // be converted.
        map.check_suspicion()?;

        Ok(Self::new(difficulty, map))
    }

    /// Create a [`GradualDifficulty`] for a [`Beatmap`] on a specific
    /// [`GameMode`].
    pub fn new_with_mode(
        difficulty: Difficulty,
        map: &Beatmap,
        mode: GameMode,
    ) -> Result<Self, ConvertError> {
        match mode {
            GameMode::Osu => Osu::gradual_difficulty(difficulty, map).map(Self::Osu),
            GameMode::Taiko => Taiko::gradual_difficulty(difficulty, map).map(Self::Taiko),
            GameMode::Catch => Catch::gradual_difficulty(difficulty, map).map(Self::Catch),
            GameMode::Mania => Mania::gradual_difficulty(difficulty, map).map(Self::Mania),
        }
    }
    pub fn advance(&mut self) -> bool {
        match self {
            Self::Osu(gradual) => gradual.advance(),
            Self::Taiko(gradual) => gradual.advance_source(),
            Self::Catch(gradual) => gradual.advance_source(),
            Self::Mania(gradual) => gradual.advance_source(),
        }
    }

    pub fn skip_batch(&mut self, count: usize) {
        match self {
            Self::Osu(gradual) => {
                gradual.skip(count);
            }
            Self::Taiko(gradual) => {
                gradual.skip_source(count);
            }
            Self::Catch(gradual) => {
                gradual.skip_source(count);
            }
            Self::Mania(gradual) => {
                gradual.skip_source(count);
            }
        }
    }

    pub fn skip_batch_to_end(&mut self) {
        match self {
            Self::Osu(gradual) => gradual.skip_to_end(),
            Self::Taiko(gradual) => gradual.skip_source_to_end(),
            Self::Catch(gradual) => gradual.skip_source_to_end(),
            Self::Mania(gradual) => gradual.skip_source_to_end(),
        }
    }

    pub fn current_strains(&self) -> Strains {
        match self {
            Self::Osu(gradual) => gradual.current_strains().into(),
            Self::Taiko(gradual) => gradual.current_strains().into(),
            Self::Catch(gradual) => gradual.current_strains().into(),
            Self::Mania(gradual) => gradual.current_strains().into(),
        }
    }

    pub fn object_end_times(&self) -> &[f64] {
        match self {
            Self::Osu(gradual) => gradual.object_end_times(),
            Self::Taiko(gradual) => gradual.object_end_times(),
            Self::Catch(gradual) => gradual.source_object_end_times(),
            Self::Mania(gradual) => gradual.object_end_times(),
        }
    }

    pub fn attributes(&self) -> Option<DifficultyAttributes> {
        match self {
            GradualDifficulty::Osu(gradual) => {
                Some(DifficultyAttributes::Osu(gradual.attributes()))
            }
            GradualDifficulty::Taiko(gradual) => {
                Some(DifficultyAttributes::Taiko(gradual.attributes()))
            }
            GradualDifficulty::Catch(gradual) => {
                gradual.attributes().map(DifficultyAttributes::Catch)
            }
            GradualDifficulty::Mania(gradual) => {
                gradual.attributes().map(DifficultyAttributes::Mania)
            }
        }
    }
}

impl Iterator for GradualDifficulty {
    type Item = DifficultyAttributes;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            GradualDifficulty::Osu(gradual) => gradual.next().map(DifficultyAttributes::Osu),
            GradualDifficulty::Taiko(gradual) => gradual.next().map(DifficultyAttributes::Taiko),
            GradualDifficulty::Catch(gradual) => gradual.next().map(DifficultyAttributes::Catch),
            GradualDifficulty::Mania(gradual) => gradual.next().map(DifficultyAttributes::Mania),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            GradualDifficulty::Osu(gradual) => gradual.size_hint(),
            GradualDifficulty::Taiko(gradual) => gradual.size_hint(),
            GradualDifficulty::Catch(gradual) => gradual.size_hint(),
            GradualDifficulty::Mania(gradual) => gradual.size_hint(),
        }
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        match self {
            GradualDifficulty::Osu(gradual) => gradual.nth(n).map(DifficultyAttributes::Osu),
            GradualDifficulty::Taiko(gradual) => gradual.nth(n).map(DifficultyAttributes::Taiko),
            GradualDifficulty::Catch(gradual) => gradual.nth(n).map(DifficultyAttributes::Catch),
            GradualDifficulty::Mania(gradual) => gradual.nth(n).map(DifficultyAttributes::Mania),
        }
    }
}

impl ExactSizeIterator for GradualDifficulty {
    fn len(&self) -> usize {
        match self {
            GradualDifficulty::Osu(gradual) => gradual.len(),
            GradualDifficulty::Taiko(gradual) => gradual.len(),
            GradualDifficulty::Catch(gradual) => gradual.len(),
            GradualDifficulty::Mania(gradual) => gradual.len(),
        }
    }
}
