use rosu_map::section::general::GameMode;

use crate::{
    Beatmap, Difficulty,
    any::{
        CalculateError,
        difficulty::{object::IDifficultyObjects, skills::StrainSkill},
    },
    model::{hit_object::HitObject, mode::ConvertError},
    taiko::strains::TaikoStrains,
};

use super::{
    DifficultyValues, TaikoDifficultyAttributes, object::TaikoDifficultyObjects,
    skills::TaikoSkills,
};

/// Gradually calculate the difficulty attributes of an osu!taiko map.
///
/// Note that this struct implements [`Iterator`]. On every call of
/// [`Iterator::next`], the map's next hit object will be processed and the
/// [`TaikoDifficultyAttributes`] will be updated and returned.
///
/// If you want to calculate performance attributes, use
/// [`TaikoGradualPerformance`] instead.
///
/// # Example
///
/// ```
/// use rosu_pp::{Beatmap, Difficulty};
/// use rosu_pp::taiko::{Taiko, TaikoGradualDifficulty};
///
/// let map = Beatmap::from_path("./resources/1028484.osu").unwrap();
///
/// let difficulty = Difficulty::new().mods(64); // DT
/// let mut iter = TaikoGradualDifficulty::new(difficulty, &map).unwrap();
///
/// // the difficulty of the map after the first hit object
/// let attrs1 = iter.next();
/// // ... after the second hit object
/// let attrs2 = iter.next();
///
/// // Remaining hit objects
/// for difficulty in iter {
///     // ...
/// }
/// ```
///
/// [`TaikoGradualPerformance`]: crate::taiko::TaikoGradualPerformance
pub struct TaikoGradualDifficulty {
    pub(crate) idx: usize,
    source_idx: usize,
    skill_idx: usize,
    total_hits: usize,
    pub(crate) difficulty: Difficulty,
    attrs: TaikoDifficultyAttributes,
    diff_objects: TaikoDifficultyObjects,
    skills: TaikoSkills,
    source_is_hit: Box<[bool]>,
    object_end_times: Box<[f64]>,
}

impl TaikoGradualDifficulty {
    /// Create a new difficulty attributes iterator for osu!taiko maps.
    pub fn new(difficulty: Difficulty, map: &Beatmap) -> Result<Self, ConvertError> {
        let map = super::prepare_map(&difficulty, map)?;

        Ok(new(difficulty, &map))
    }

    /// Same as [`TaikoGradualDifficulty::new`] but verifies that the map is not
    /// suspicious.
    pub fn checked_new(difficulty: Difficulty, map: &Beatmap) -> Result<Self, CalculateError> {
        let map = super::prepare_map(&difficulty, map)?;
        map.check_suspicion()?;

        Ok(new(difficulty, &map))
    }
    pub(crate) fn attributes(&self) -> TaikoDifficultyAttributes {
        let mut attrs = self.attrs.clone();
        DifficultyValues::eval(
            &mut attrs,
            self.skills.clone(),
            self.difficulty.get_mods().rx(),
        );

        attrs
    }

    pub(crate) fn current_strains(&self) -> TaikoStrains {
        TaikoStrains {
            color: self.skills.color.current_strain_peaks(),
            reading: self.skills.reading.current_strain_peaks(),
            rhythm: self.skills.rhythm.current_strain_peaks(),
            stamina: self.skills.stamina.current_strain_peaks(),
            single_color_stamina: self.skills.single_color_stamina.current_strain_peaks(),
        }
    }

    pub(crate) fn advance(&mut self) -> bool {
        self.skip_hits(1) == 1
    }

    pub(crate) fn advance_source(&mut self) -> bool {
        self.skip_source(1) == 1
    }

    pub(crate) fn skip_hits(&mut self, count: usize) -> usize {
        let take = count.min(self.total_hits.saturating_sub(self.idx));
        let initial_hits = self.idx;

        while self.idx - initial_hits < take {
            if !self.advance_one_source() {
                break;
            }
        }

        self.idx - initial_hits
    }

    pub(crate) fn skip_source(&mut self, count: usize) -> usize {
        let take = count.min(self.source_is_hit.len().saturating_sub(self.source_idx));

        for _ in 0..take {
            let advanced = self.advance_one_source();
            debug_assert!(advanced);
        }

        take
    }

    fn advance_one_source(&mut self) -> bool {
        let Some(&is_hit) = self.source_is_hit.get(self.source_idx) else {
            return false;
        };

        if self.source_idx >= 2 {
            let curr = self
                .diff_objects
                .get(self.skill_idx)
                .expect("each source object after the second has a difficulty object");
            self.skill_idx += 1;

            let borrowed = curr.get();
            self.skills.rhythm.process(&borrowed, &self.diff_objects);
            self.skills.reading.process(&borrowed, &self.diff_objects);
            self.skills.color.process(&borrowed, &self.diff_objects);
            self.skills.stamina.process(&borrowed, &self.diff_objects);
            self.skills
                .single_color_stamina
                .process(&borrowed, &self.diff_objects);
        }

        if is_hit {
            self.attrs.max_combo += 1;
            self.idx += 1;
        }

        self.source_idx += 1;

        true
    }

    pub(crate) fn skip_to_end(&mut self) {
        self.skip_hits(self.len());
    }

    pub(crate) fn skip_source_to_end(&mut self) {
        self.skip_source(self.source_is_hit.len().saturating_sub(self.source_idx));
    }

    pub(crate) fn object_end_times(&self) -> &[f64] {
        &self.object_end_times
    }
}

fn new(difficulty: Difficulty, map: &Beatmap) -> TaikoGradualDifficulty {
    debug_assert_eq!(map.mode, GameMode::Taiko);

    let take = difficulty.get_passed_objects();
    let clock_rate = difficulty.get_clock_rate();

    let hit_windows = map
        .attributes()
        .difficulty(&difficulty)
        .build()
        .hit_windows();

    let great_hit_window = hit_windows.od_great.unwrap_or(0.0);
    let ok_hit_window = hit_windows.od_ok.unwrap_or(0.0);

    let mut n_diff_objects = 0;
    let mut max_combo = 0;

    let diff_objects = DifficultyValues::create_difficulty_objects(
        map,
        take as u32,
        clock_rate,
        &mut max_combo,
        &mut n_diff_objects,
        difficulty.get_mods(),
    );

    let skills = TaikoSkills::new(great_hit_window, map.is_convert);

    let attrs = TaikoDifficultyAttributes {
        great_hit_window,
        ok_hit_window,
        is_convert: map.is_convert,
        ..Default::default()
    };

    let source_is_hit: Box<[_]> = map.hit_objects.iter().map(HitObject::is_circle).collect();
    let object_end_times = map.hit_objects.iter().map(HitObject::end_time).collect();
    let total_hits = source_is_hit.iter().filter(|&&is_hit| is_hit).count();

    TaikoGradualDifficulty {
        idx: 0,
        source_idx: 0,
        skill_idx: 0,
        total_hits,
        difficulty,
        diff_objects,
        skills,
        attrs,
        source_is_hit,
        object_end_times,
    }
}

impl Iterator for TaikoGradualDifficulty {
    type Item = TaikoDifficultyAttributes;

    fn next(&mut self) -> Option<Self::Item> {
        self.advance().then(|| self.attributes())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();

        (len, Some(len))
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        if n >= self.len() {
            self.skip_to_end();
            return None;
        }

        self.skip_hits(n + 1);

        Some(self.attributes())
    }
}

impl ExactSizeIterator for TaikoGradualDifficulty {
    fn len(&self) -> usize {
        self.total_hits.saturating_sub(self.idx)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use crate::{Beatmap, taiko::Taiko, taiko::attributes::tests::assert_eq_attrs};

    use super::*;

    #[test]
    fn empty() {
        let map = Beatmap::from_bytes(&[]).unwrap();
        let mut gradual = TaikoGradualDifficulty::new(Difficulty::new(), &map).unwrap();
        assert!(gradual.next().is_none());
    }

    #[test]
    fn next_and_nth() {
        let map = Beatmap::from_path("./resources/1028484.osu").unwrap();

        let difficulty = Difficulty::new();

        let mut gradual = TaikoGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_2nd = TaikoGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_3rd = TaikoGradualDifficulty::new(difficulty.clone(), &map).unwrap();

        let hit_objects_len = map.hit_objects.len();

        let n_hits = map.hit_objects.iter().filter(|h| h.is_circle()).count();

        for i in 1.. {
            let Some(next_gradual) = gradual.next() else {
                assert_eq!(i, n_hits + 1);
                assert!(gradual_2nd.last().is_some() || hit_objects_len % 2 == 0);
                assert!(gradual_3rd.last().is_some() || hit_objects_len % 3 == 0);
                break;
            };

            if i % 2 == 0 {
                let next_gradual_2nd = gradual_2nd.nth(1).unwrap();
                assert_eq_attrs(&next_gradual, &next_gradual_2nd);
            }

            if i % 3 == 0 {
                let next_gradual_3rd = gradual_3rd.nth(2).unwrap();
                assert_eq_attrs(&next_gradual, &next_gradual_3rd);
            }

            let expected = difficulty
                .clone()
                .passed_objects(i as u32)
                .calculate_for_mode::<Taiko>(&map)
                .unwrap();

            assert_eq_attrs(&next_gradual, &expected);
        }
    }
}
