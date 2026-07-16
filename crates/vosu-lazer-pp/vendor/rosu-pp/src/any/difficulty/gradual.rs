use rosu_map::section::general::GameMode;

use crate::{
    Beatmap, Difficulty,
    any::DifficultyAttributes,
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

    /// Process exactly one top-level playable beatmap object without
    /// evaluating the skills.
    pub fn advance(&mut self) -> bool {
        match self {
            Self::Osu(gradual) => gradual.advance(),
            Self::Taiko(gradual) => gradual.advance(),
            Self::Catch(gradual) => gradual.advance(),
            Self::Mania(gradual) => gradual.advance(),
        }
    }

    /// Append up to `count` top-level objects as one source batch and only
    /// then drain difficulty objects through the final appended end time.
    pub fn skip_batch(&mut self, count: usize) -> usize {
        match self {
            Self::Osu(gradual) => OsuGradualDifficulty::skip(gradual, count),
            Self::Taiko(gradual) => TaikoGradualDifficulty::skip(gradual, count),
            Self::Catch(gradual) => CatchGradualDifficulty::skip(gradual, count),
            Self::Mania(gradual) => ManiaGradualDifficulty::skip(gradual, count),
        }
    }

    /// Append every remaining top-level object as one source batch.
    pub fn skip_batch_to_end(&mut self) -> usize {
        match self {
            Self::Osu(gradual) => {
                let count = gradual.len();
                gradual.skip_to_end();
                count
            }
            Self::Taiko(gradual) => gradual.skip_to_end(),
            Self::Catch(gradual) => gradual.skip_to_end(),
            Self::Mania(gradual) => gradual.skip_to_end(),
        }
    }

    /// Evaluate attributes for the currently processed prefix.
    pub fn attributes(&mut self) -> DifficultyAttributes {
        match self {
            Self::Osu(gradual) => DifficultyAttributes::Osu(gradual.attributes()),
            Self::Taiko(gradual) => DifficultyAttributes::Taiko(gradual.attributes()),
            Self::Catch(gradual) => DifficultyAttributes::Catch(gradual.attributes()),
            Self::Mania(gradual) => DifficultyAttributes::Mania(gradual.attributes()),
        }
    }
}

impl Iterator for GradualDifficulty {
    type Item = DifficultyAttributes;

    fn next(&mut self) -> Option<Self::Item> {
        self.advance().then(|| self.attributes())
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
        for _ in 0..n {
            if !self.advance() {
                return None;
            }
        }

        self.next()
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
