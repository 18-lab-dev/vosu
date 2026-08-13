use rosu_map::section::general::GameMode;

use crate::{
    Beatmap, Difficulty,
    any::{CalculateError, difficulty::skills::StrainSkill},
    mania::{object::ObjectParams, strains::ManiaStrains},
    model::{hit_object::HitObject, mode::ConvertError},
    util::sync::RefCount,
};

use super::{
    DIFFICULTY_MULTIPLIER, DifficultyValues, ManiaDifficultyAttributes, ManiaObject,
    object::ManiaDifficultyObject, skills::strain::Strain,
};

/// Gradually calculate the difficulty attributes of an osu!mania map.
///
/// Note that this struct implements [`Iterator`].
/// On every call of [`Iterator::next`], the map's next hit object will
/// be processed and the [`ManiaDifficultyAttributes`] will be updated and returned.
///
/// If you want to calculate performance attributes, use
/// [`ManiaGradualPerformance`] instead.
///
/// # Example
///
/// ```
/// use rosu_pp::{Beatmap, Difficulty};
/// use rosu_pp::mania::ManiaGradualDifficulty;
///
/// let map = Beatmap::from_path("./resources/1638954.osu").unwrap();
///
/// let difficulty = Difficulty::new().mods(64); // DT
/// let mut iter = ManiaGradualDifficulty::new(difficulty, &map).unwrap();
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
/// [`ManiaGradualPerformance`]: crate::mania::ManiaGradualPerformance
pub struct ManiaGradualDifficulty {
    pub(crate) idx: usize,
    pub(crate) difficulty: Difficulty,
    objects: Box<[ManiaObject]>,
    object_limit: usize,
    objects_is_circle: Box<[bool]>,
    is_convert: bool,
    strain: Strain,
    diff_objects: Box<[RefCount<ManiaDifficultyObject>]>,
    diff_idx: usize,
    note_state: NoteState,
    object_end_times: Box<[f64]>,
}

#[derive(Default)]
struct NoteState {
    curr_combo: u32,
    n_hold_notes: u32,
}

impl ManiaGradualDifficulty {
    /// Create a new difficulty attributes iterator for osu!mania maps.
    pub fn new(difficulty: Difficulty, map: &Beatmap) -> Result<Self, ConvertError> {
        let map = super::prepare_map(&difficulty, map)?;

        Ok(new(difficulty, &map))
    }

    /// Same as [`ManiaGradualDifficulty::new`] but verifies that the map is not
    /// suspicious.
    pub fn checked_new(difficulty: Difficulty, map: &Beatmap) -> Result<Self, CalculateError> {
        let map = super::prepare_map(&difficulty, map)?;
        map.check_suspicion()?;

        Ok(new(difficulty, &map))
    }
    pub(crate) fn attributes(&self) -> Option<ManiaDifficultyAttributes> {
        Some(ManiaDifficultyAttributes {
            stars: self.strain.cloned_difficulty_value() * DIFFICULTY_MULTIPLIER,
            max_combo: self.note_state.curr_combo,
            n_objects: self.idx as u32,
            n_hold_notes: self.note_state.n_hold_notes,
            is_convert: self.is_convert,
        })
    }
    pub(crate) fn advance(&mut self) -> bool {
        if self.idx >= self.object_limit {
            return false;
        }

        self.skip(1);
        true
    }

    pub(crate) fn skip(&mut self, count: usize) {
        let target = self.idx.saturating_add(count).min(self.object_limit);

        if target == self.idx {
            return;
        }

        while self.idx < target {
            let object = &self.objects[self.idx];
            increment_combo_raw(
                self.objects_is_circle[self.idx],
                object.start_time,
                object.end_time,
                &mut self.note_state,
            );
            self.idx += 1;
        }

        while self.diff_idx < target.saturating_sub(1) {
            let curr = self
                .diff_objects
                .get(self.diff_idx)
                .expect("each object after the first has a difficulty object")
                .get();
            self.strain.process(&curr, &self.diff_objects);
            self.diff_idx += 1;
        }
    }
    pub(crate) fn advance_source(&mut self) -> bool {
        if self.idx >= self.object_limit {
            return false;
        }

        self.skip_source(1);

        true
    }

    pub(crate) fn skip_source(&mut self, count: usize) {
        let target = self.idx.saturating_add(count).min(self.object_limit);

        if target == self.idx {
            return;
        }

        let final_end_time = self.objects[target - 1].end_time;

        while self.idx < target {
            let object = &self.objects[self.idx];
            increment_combo_raw(
                self.objects_is_circle[self.idx],
                object.start_time,
                object.end_time,
                &mut self.note_state,
            );
            self.idx += 1;
        }

        // The C# gradual enumerator advances by source object, then processes every
        // difficulty object ending no later than that source boundary. Equal-time
        // chord members are therefore intentionally processed together.
        while let Some(curr) = self.diff_objects.get(self.diff_idx) {
            let curr = curr.get();

            if curr.end_time * self.difficulty.get_clock_rate() > final_end_time {
                break;
            }

            self.strain.process(&curr, &self.diff_objects);
            self.diff_idx += 1;
        }
    }

    pub(crate) fn skip_source_to_end(&mut self) {
        self.skip_source(self.len());
    }

    pub(crate) fn skip_to_end(&mut self) {
        self.skip(self.len());
    }

    pub(crate) fn current_strains(&self) -> ManiaStrains {
        ManiaStrains {
            strains: self.strain.current_strain_peaks(),
        }
    }

    pub(crate) fn object_end_times(&self) -> &[f64] {
        &self.object_end_times
    }
}

fn new(difficulty: Difficulty, map: &Beatmap) -> ManiaGradualDifficulty {
    debug_assert_eq!(map.mode, GameMode::Mania);

    let total_columns = map.cs.round_ties_even().max(1.0);
    let clock_rate = difficulty.get_clock_rate();
    let mut params = ObjectParams::new(map);

    let objects: Box<[_]> = map
        .hit_objects
        .iter()
        .map(|h| ManiaObject::new(h, total_columns, &mut params))
        .collect();
    let object_limit = difficulty.get_passed_objects().min(objects.len());
    let objects_is_circle: Box<[_]> = map.hit_objects.iter().map(HitObject::is_circle).collect();
    let object_end_times = objects
        .iter()
        .take(object_limit)
        .map(|object| object.end_time)
        .collect();

    let diff_objects = DifficultyValues::create_difficulty_objects(
        clock_rate,
        total_columns as usize,
        objects.iter().take(object_limit).cloned(),
    );

    ManiaGradualDifficulty {
        idx: 0,
        difficulty,
        objects,
        object_limit,
        objects_is_circle,
        is_convert: map.is_convert,
        strain: Strain::new(total_columns as usize),
        diff_objects,
        diff_idx: 0,
        note_state: NoteState::default(),
        object_end_times,
    }
}

impl Iterator for ManiaGradualDifficulty {
    type Item = ManiaDifficultyAttributes;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.advance() {
            return None;
        }

        self.attributes()
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();

        (len, Some(len))
    }

    fn nth(&mut self, n: usize) -> Option<Self::Item> {
        let remaining = self.len();

        if n >= remaining {
            self.skip_to_end();
            return None;
        }

        self.skip(n + 1);
        self.attributes()
    }
}

impl ExactSizeIterator for ManiaGradualDifficulty {
    fn len(&self) -> usize {
        self.object_limit.saturating_sub(self.idx)
    }
}

fn increment_combo_raw(is_circle: bool, start_time: f64, end_time: f64, state: &mut NoteState) {
    if is_circle {
        state.curr_combo += 1;
    } else {
        state.curr_combo += 1 + ((end_time - start_time) / 100.0) as u32;
        state.n_hold_notes += 1;
    }
}

#[cfg(test)]
mod tests {
    use crate::{Beatmap, mania::Mania};

    use super::*;

    #[test]
    fn empty() {
        let map = Beatmap::from_bytes(&[]).unwrap();
        let mut gradual = ManiaGradualDifficulty::new(Difficulty::new(), &map).unwrap();
        assert!(gradual.next().is_none());
    }

    #[test]
    fn nth_out_of_range_exhausts() {
        let map = Beatmap::from_path("./resources/1638954.osu").unwrap();
        let mut gradual = ManiaGradualDifficulty::new(Difficulty::new(), &map).unwrap();
        let remaining = gradual.len();

        assert!(gradual.nth(remaining).is_none());
        assert_eq!(gradual.len(), 0);
        assert!(gradual.next().is_none());
    }

    #[test]
    fn next_and_nth() {
        let map = Beatmap::from_path("./resources/1638954.osu").unwrap();

        let difficulty = Difficulty::new();

        let mut gradual = ManiaGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_2nd = ManiaGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_3rd = ManiaGradualDifficulty::new(difficulty.clone(), &map).unwrap();

        let hit_objects_len = map.hit_objects.len();

        for i in 1.. {
            let Some(next_gradual) = gradual.next() else {
                assert_eq!(i, hit_objects_len + 1);
                assert!(gradual_2nd.last().is_some() || hit_objects_len % 2 == 0);
                assert!(gradual_3rd.last().is_some() || hit_objects_len % 3 == 0);
                break;
            };

            if i % 2 == 0 {
                let next_gradual_2nd = gradual_2nd.nth(1).unwrap();
                assert_eq!(next_gradual, next_gradual_2nd);
            }

            if i % 3 == 0 {
                let next_gradual_3rd = gradual_3rd.nth(2).unwrap();
                assert_eq!(next_gradual, next_gradual_3rd);
            }

            let expected = difficulty
                .clone()
                .passed_objects(i as u32)
                .calculate_for_mode::<Mania>(&map)
                .unwrap();

            assert_eq!(next_gradual, expected);
        }
    }
}
