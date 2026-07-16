use rosu_map::section::general::GameMode;

use crate::{
    Beatmap, Difficulty,
    any::{CalculateError, difficulty::skills::StrainSkill},
    mania::{object::ObjectParams, strains::ManiaStrains},
    model::mode::ConvertError,
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
    objects_is_circle: Box<[bool]>,
    is_convert: bool,
    strain: Strain,
    diff_objects: Box<[RefCount<ManiaDifficultyObject>]>,
    diff_idx: usize,
    note_state: NoteState,
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
}

fn new(difficulty: Difficulty, map: &Beatmap) -> ManiaGradualDifficulty {
    debug_assert_eq!(map.mode, GameMode::Mania);

    let take = difficulty.get_passed_objects();
    let total_columns = map.cs.round_ties_even().max(1.0);
    let clock_rate = difficulty.get_clock_rate();
    let mut params = ObjectParams::new(map);

    let objects = map
        .hit_objects
        .iter()
        .take(take)
        .map(|h| ManiaObject::new(h, total_columns, &mut params))
        .collect::<Box<[_]>>();

    let diff_objects = DifficultyValues::create_difficulty_objects(
        clock_rate,
        total_columns as usize,
        objects.iter().copied(),
    );

    let strain = Strain::new(total_columns as usize);

    let note_state = NoteState::default();
    let objects_is_circle = map
        .hit_objects
        .iter()
        .take(take)
        .map(|object| object.is_circle())
        .collect();

    ManiaGradualDifficulty {
        idx: 0,
        difficulty,
        objects,
        objects_is_circle,
        is_convert: map.is_convert,
        strain,
        diff_objects,
        diff_idx: 0,
        note_state,
    }
}

impl ManiaGradualDifficulty {
    /// End times of the prepared hit objects in gradual iteration order.
    ///
    /// These are the post-conversion and post-modification times, so callers
    /// can advance by play time without consulting the unprepared source map.
    pub fn object_end_times(&self) -> impl ExactSizeIterator<Item = f64> + '_ {
        self.objects.iter().map(|object| object.end_time)
    }

    /// Current strain peaks after the objects processed so far.
    pub fn current_strains(&self) -> ManiaStrains {
        ManiaStrains {
            strains: self.strain.current_strain_peaks(),
        }
    }

    /// Append up to `count` prepared top-level objects as one source batch.
    ///
    /// Combo and object cardinality are updated for every appended object.
    /// Difficulty objects are drained only once, against the raw end time of
    /// the final appended top-level object, matching the pinned gradual patch.
    pub fn skip(&mut self, count: usize) -> usize {
        let take = count.min(self.objects.len().saturating_sub(self.idx));
        let Some(final_idx) = self
            .idx
            .checked_add(take)
            .and_then(|end| end.checked_sub(1))
        else {
            return 0;
        };

        for idx in self.idx..=final_idx {
            let object = &self.objects[idx];
            increment_combo_raw(
                self.objects_is_circle[idx],
                object.start_time,
                object.end_time,
                &mut self.note_state,
            );
        }

        let final_end_time = self.objects[final_idx].end_time;

        while let Some(curr) = self.diff_objects.get(self.diff_idx) {
            let borrowed = curr.get();

            if borrowed.end_time * self.difficulty.get_clock_rate() > final_end_time {
                break;
            }

            self.strain.process(&borrowed, &self.diff_objects);
            self.diff_idx += 1;
        }

        self.idx += take;

        take
    }

    /// Append every remaining prepared top-level object as one source batch.
    pub fn skip_to_end(&mut self) -> usize {
        self.skip(self.objects.len().saturating_sub(self.idx))
    }

    pub(crate) fn advance(&mut self) -> bool {
        self.skip(1) == 1
    }

    pub(crate) fn attributes(&self) -> ManiaDifficultyAttributes {
        ManiaDifficultyAttributes {
            stars: self.strain.cloned_difficulty_value() * DIFFICULTY_MULTIPLIER,
            max_combo: self.note_state.curr_combo,
            n_objects: self.idx as u32,
            n_hold_notes: self.note_state.n_hold_notes,
            is_convert: self.is_convert,
        }
    }
}

impl Iterator for ManiaGradualDifficulty {
    type Item = ManiaDifficultyAttributes;

    fn next(&mut self) -> Option<Self::Item> {
        self.advance().then(|| self.attributes())
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();

        (len, Some(len))
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

impl ExactSizeIterator for ManiaGradualDifficulty {
    fn len(&self) -> usize {
        self.objects.len() - self.idx
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
    use rosu_map::{section::general::GameMode, util::Pos};

    use crate::{
        Beatmap,
        mania::Mania,
        model::hit_object::{HitObject, HitObjectKind, HoldNote},
    };

    use super::*;

    #[test]
    fn empty() {
        let map = Beatmap::from_bytes(&[]).unwrap();
        let mut gradual = ManiaGradualDifficulty::new(Difficulty::new(), &map).unwrap();
        assert!(gradual.next().is_none());
    }

    #[test]
    fn hold_end_drains_eligible_diff_objects_without_advancing_top_level_cardinality() {
        fn circle(column: f32, start_time: f64) -> HitObject {
            HitObject {
                pos: Pos::new(column * 128.0 + 64.0, 192.0),
                start_time,
                kind: HitObjectKind::Circle,
            }
        }

        let objects = vec![
            circle(0.0, 0.0),
            circle(1.0, 100.0),
            circle(2.0, 200.0),
            HitObject {
                pos: Pos::new(448.0, 192.0),
                start_time: 300.0,
                kind: HitObjectKind::Hold(HoldNote { duration: 700.0 }),
            },
            circle(0.0, 400.0),
        ];
        let map = Beatmap {
            mode: GameMode::Mania,
            cs: 4.0,
            hit_sounds: vec![Default::default(); objects.len()],
            hit_objects: objects,
            ..Default::default()
        };
        let mut gradual = ManiaGradualDifficulty::new(Difficulty::new(), &map).unwrap();

        let empty_attrs = gradual.attributes();
        assert_eq!(empty_attrs.n_objects, 0);
        assert_eq!(empty_attrs.max_combo, 0);
        assert_eq!(gradual.diff_idx, 0);
        assert_eq!(gradual.current_strains().strains, [0.0]);
        assert_eq!(gradual.attributes(), empty_attrs);
        assert_eq!(gradual.current_strains().strains, [0.0]);

        assert_eq!(gradual.next().unwrap().n_objects, 1);
        assert_eq!(gradual.diff_idx, 0);
        assert_eq!(gradual.next().unwrap().n_objects, 2);
        assert_eq!(gradual.diff_idx, 1);
        assert_eq!(gradual.next().unwrap().n_objects, 3);
        assert_eq!(gradual.diff_idx, 2);

        let through_hold = gradual.next().unwrap();
        assert_eq!(through_hold.n_objects, 4);
        assert_eq!(through_hold.n_hold_notes, 1);
        assert_eq!(
            gradual.diff_idx,
            gradual.diff_objects.len(),
            "the hold's raw end must drain the later-starting circle's eligible difficulty object",
        );
        let hold_strains = gradual.current_strains();
        assert_eq!(gradual.attributes(), through_hold);
        assert_eq!(gradual.current_strains(), hold_strains);

        let final_attrs = gradual.next().unwrap();
        assert_eq!(final_attrs.n_objects, 5);
        assert_eq!(gradual.diff_idx, gradual.diff_objects.len());
        assert_eq!(
            gradual.current_strains(),
            hold_strains,
            "the already-drained final difficulty object must not be processed twice",
        );
        assert!(gradual.next().is_none());
    }

    #[test]
    fn batch_skip_uses_only_final_appended_raw_end_for_overlapping_holds() {
        let circle = |column: f32, start_time: f64| HitObject {
            pos: Pos::new(column * 128.0 + 64.0, 192.0),
            start_time,
            kind: HitObjectKind::Circle,
        };
        let objects = vec![
            circle(0.0, 0.0),
            HitObject {
                pos: Pos::new(192.0, 192.0),
                start_time: 100.0,
                kind: HitObjectKind::Hold(HoldNote { duration: 900.0 }),
            },
            HitObject {
                pos: Pos::new(320.0, 192.0),
                start_time: 200.0,
                kind: HitObjectKind::Hold(HoldNote { duration: 100.0 }),
            },
            circle(3.0, 400.0),
        ];
        let map = Beatmap {
            mode: GameMode::Mania,
            cs: 4.0,
            hit_sounds: vec![Default::default(); objects.len()],
            hit_objects: objects,
            ..Default::default()
        };
        let mut batch = ManiaGradualDifficulty::new(Difficulty::new(), &map).unwrap();
        let mut repeated = ManiaGradualDifficulty::new(Difficulty::new(), &map).unwrap();

        assert_eq!(ManiaGradualDifficulty::skip(&mut batch, 0), 0);
        assert_eq!(ManiaGradualDifficulty::skip(&mut batch, 3), 3);
        assert_eq!(batch.attributes().n_objects, 3);
        assert_eq!(batch.attributes().n_hold_notes, 2);
        assert_eq!(batch.diff_idx, 0);

        assert!(repeated.advance());
        assert!(repeated.advance());
        assert!(repeated.advance());
        assert_eq!(repeated.attributes().n_objects, 3);
        assert_eq!(repeated.diff_idx, repeated.diff_objects.len());
        assert_ne!(batch.current_strains(), repeated.current_strains());

        assert_eq!(batch.skip_to_end(), 1);
        assert_eq!(batch.attributes().n_objects, 4);
        assert_eq!(batch.diff_idx, 0);
        assert_eq!(batch.skip_to_end(), 0);
    }

    #[test]
    fn next_and_nth() {
        let map = Beatmap::from_path(crate::test_resource("1638954.osu")).unwrap();

        let difficulty = Difficulty::new();

        let mut gradual = ManiaGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_2nd = ManiaGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_3rd = ManiaGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut nth_len = ManiaGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let len = nth_len.len();
        assert!(nth_len.nth(len).is_none());
        assert_eq!(nth_len.len(), 0);

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

            assert_eq!(next_gradual.n_objects, i as u32);

            if i == 1 {
                let full_prefix = difficulty
                    .clone()
                    .passed_objects(1)
                    .calculate_for_mode::<Mania>(&map)
                    .unwrap();

                // The pinned gradual calculator drains precomputed difficulty objects
                // by end time. The map begins with simultaneous objects, so later
                // objects contribute to the first gradual step but not a full
                // calculation truncated to one top-level object.
                assert_eq!(next_gradual.stars, 0.09);
                assert_eq!(full_prefix.stars, 0.0);
            }
        }
    }
}
