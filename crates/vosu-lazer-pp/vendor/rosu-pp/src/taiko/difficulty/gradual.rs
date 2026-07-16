use rosu_map::section::{general::GameMode, hit_objects::CurveBuffers};

use crate::{
    Beatmap, Difficulty,
    any::{CalculateError, difficulty::skills::StrainSkill},
    model::{
        control_point::{DifficultyPoint, TimingPoint},
        hit_object::{HitObject, HitObjectKind, HoldNote, Spinner},
        mode::ConvertError,
    },
    taiko::TaikoStrains,
    util::get_precision_adjusted_beat_len,
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
    pub(crate) difficulty: Difficulty,
    attrs: TaikoDifficultyAttributes,
    diff_objects: TaikoDifficultyObjects,
    skill_idx: usize,
    skills: TaikoSkills,
    object_is_hit: Box<[bool]>,
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

    /// End times of the prepared top-level hit objects in source order.
    pub fn object_end_times(&self) -> &[f64] {
        &self.object_end_times
    }

    /// Current strain peaks, including the in-progress section.
    pub fn current_strains(&self) -> TaikoStrains {
        TaikoStrains {
            color: self.skills.color.clone().into_current_strain_peaks(),
            reading: self.skills.reading.clone().into_current_strain_peaks(),
            rhythm: self.skills.rhythm.clone().into_current_strain_peaks(),
            stamina: self.skills.stamina.clone().into_current_strain_peaks(),
            single_color_stamina: self
                .skills
                .single_color_stamina
                .clone()
                .into_current_strain_peaks(),
        }
    }
}

fn new(difficulty: Difficulty, map: &Beatmap) -> TaikoGradualDifficulty {
    debug_assert_eq!(map.mode, GameMode::Taiko);
    let take = difficulty.get_passed_objects();
    let clock_rate = difficulty.get_clock_rate();

    let object_is_hit = map
        .hit_objects
        .iter()
        .map(HitObject::is_circle)
        .collect::<Box<[_]>>();
    let mut curve_bufs = CurveBuffers::default();
    let object_end_times = map
        .hit_objects
        .iter()
        .map(|object| object_end_time(map, object, &mut curve_bufs))
        .collect::<Box<[_]>>();

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

    TaikoGradualDifficulty {
        idx: 0,
        difficulty,
        attrs,
        diff_objects,
        skill_idx: 0,
        skills,
        object_is_hit,
        object_end_times,
    }
}

fn object_end_time(map: &Beatmap, object: &HitObject, curve_bufs: &mut CurveBuffers) -> f64 {
    match &object.kind {
        HitObjectKind::Circle => object.start_time,
        HitObjectKind::Spinner(Spinner { duration })
        | HitObjectKind::Hold(HoldNote { duration }) => object.start_time + duration,
        HitObjectKind::Slider(slider) => {
            let dist = slider.borrowed_curve(GameMode::Taiko, curve_bufs).dist();
            let beat_len = map
                .timing_point_at(object.start_time)
                .map_or(TimingPoint::DEFAULT_BEAT_LEN, |point| point.beat_len);
            let slider_velocity = map
                .difficulty_point_at(object.start_time)
                .map_or(DifficultyPoint::DEFAULT_SLIDER_VELOCITY, |point| {
                    point.slider_velocity
                });
            let beat_len = get_precision_adjusted_beat_len(slider_velocity, beat_len);
            let velocity = 100.0 * map.slider_multiplier / beat_len;

            object.start_time + slider.span_count() as f64 * dist / velocity
        }
    }
}

impl TaikoGradualDifficulty {
    pub(crate) fn advance(&mut self) -> bool {
        let Some((&is_hit, &end_time)) = self
            .object_is_hit
            .get(self.idx)
            .zip(self.object_end_times.get(self.idx))
        else {
            return false;
        };

        self.attrs.max_combo += u32::from(is_hit);

        while let Some(curr) = self.diff_objects.objects.get(self.skill_idx) {
            let borrowed = curr.get();
            let Some(&difficulty_object_end_time) = self.object_end_times.get(self.skill_idx + 2)
            else {
                break;
            };

            if difficulty_object_end_time > end_time {
                break;
            }

            self.skills.rhythm.process(&borrowed, &self.diff_objects);
            self.skills.reading.process(&borrowed, &self.diff_objects);
            self.skills.color.process(&borrowed, &self.diff_objects);
            self.skills.stamina.process(&borrowed, &self.diff_objects);
            self.skills
                .single_color_stamina
                .process(&borrowed, &self.diff_objects);
            self.skill_idx += 1;
        }

        self.idx += 1;

        true
    }

    /// Append up to `count` top-level objects as one source batch, then
    /// process difficulty objects only through the final appended end time.
    pub fn skip(&mut self, count: usize) -> usize {
        let take = count.min(self.object_is_hit.len().saturating_sub(self.idx));
        let Some(final_idx) = self
            .idx
            .checked_add(take)
            .and_then(|end| end.checked_sub(1))
        else {
            return 0;
        };

        for _ in 0..take {
            self.attrs.max_combo += u32::from(self.object_is_hit[self.idx]);
            self.idx += 1;
        }

        let end_time = self.object_end_times[final_idx];

        while let Some(curr) = self.diff_objects.objects.get(self.skill_idx) {
            let borrowed = curr.get();
            let Some(&difficulty_object_end_time) = self.object_end_times.get(self.skill_idx + 2)
            else {
                break;
            };

            if difficulty_object_end_time > end_time {
                break;
            }

            self.skills.rhythm.process(&borrowed, &self.diff_objects);
            self.skills.reading.process(&borrowed, &self.diff_objects);
            self.skills.color.process(&borrowed, &self.diff_objects);
            self.skills.stamina.process(&borrowed, &self.diff_objects);
            self.skills
                .single_color_stamina
                .process(&borrowed, &self.diff_objects);
            self.skill_idx += 1;
        }

        take
    }

    /// Append every remaining top-level object as one source batch.
    pub fn skip_to_end(&mut self) -> usize {
        Self::skip(self, self.object_is_hit.len().saturating_sub(self.idx))
    }

    pub(crate) fn attributes(&self) -> TaikoDifficultyAttributes {
        let mut attrs = self.attrs.clone();
        let is_relax = self.difficulty.get_mods().rx();

        DifficultyValues::eval(&mut attrs, self.skills.clone(), is_relax);

        attrs
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
        for _ in 0..n {
            if !self.advance() {
                return None;
            }
        }

        self.next()
    }
}

impl ExactSizeIterator for TaikoGradualDifficulty {
    fn len(&self) -> usize {
        self.object_is_hit.len() - self.idx
    }
}

#[cfg(test)]
pub(super) mod tests {
    use crate::{Beatmap, taiko::Taiko, taiko::attributes::tests::assert_eq_attrs};
    use rosu_map::util::Pos;

    use super::*;

    #[test]
    fn empty() {
        let map = Beatmap::from_bytes(&[]).unwrap();
        let mut gradual = TaikoGradualDifficulty::new(Difficulty::new(), &map).unwrap();
        assert!(gradual.next().is_none());
    }

    #[test]
    fn next_and_nth() {
        let map = Beatmap::from_path(crate::test_resource("1028484.osu")).unwrap();

        let difficulty = Difficulty::new();

        let mut gradual = TaikoGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_2nd = TaikoGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_3rd = TaikoGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut nth_len = TaikoGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let len = nth_len.len();
        assert!(nth_len.nth(len).is_none());
        assert_eq!(nth_len.len(), 0);

        let gradual_len = gradual.len();

        for i in 1.. {
            let Some(next_gradual) = gradual.next() else {
                assert_eq!(i, gradual_len + 1);
                assert_eq!(gradual_2nd.len(), gradual_len % 2);
                assert_eq!(gradual_3rd.len(), gradual_len % 3);
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
                .passed_objects(next_gradual.max_combo)
                .calculate_for_mode::<Taiko>(&map)
                .unwrap();

            assert_eq_attrs(&next_gradual, &expected);
        }
    }

    #[test]
    fn current_strains_include_the_in_progress_section() {
        let hit_objects = (0..12)
            .map(|idx| HitObject {
                pos: Pos::default(),
                start_time: f64::from(idx) * 100.0,
                kind: HitObjectKind::Circle,
            })
            .collect::<Vec<_>>();
        let map = Beatmap {
            mode: GameMode::Taiko,
            hit_sounds: vec![Default::default(); hit_objects.len()],
            hit_objects,
            ..Default::default()
        };
        let mut gradual = TaikoGradualDifficulty::new(Difficulty::new(), &map).unwrap();
        let initial = gradual.current_strains();

        assert_eq!(initial.color, [0.0]);
        assert_eq!(initial.reading, [0.0]);
        assert_eq!(initial.rhythm, [0.0]);
        assert_eq!(initial.stamina, [0.0]);
        assert_eq!(initial.single_color_stamina, [0.0]);
        assert_eq!(gradual.object_end_times().len(), map.hit_objects.len());

        gradual.nth(10).unwrap();
        let strains = gradual.current_strains();

        assert!(strains.stamina.last().is_some_and(|&strain| strain > 0.0));
        assert_eq!(strains, gradual.current_strains());
    }

    #[test]
    fn gradual_advances_top_level_objects_and_gates_skills_by_end_time() {
        let hit_objects = vec![
            HitObject {
                pos: Pos::default(),
                start_time: 100.0,
                kind: HitObjectKind::Circle,
            },
            HitObject {
                pos: Pos::default(),
                start_time: 200.0,
                kind: HitObjectKind::Spinner(Spinner { duration: 800.0 }),
            },
            HitObject {
                pos: Pos::default(),
                start_time: 500.0,
                kind: HitObjectKind::Spinner(Spinner { duration: 1_000.0 }),
            },
        ];
        let map = Beatmap {
            mode: GameMode::Taiko,
            hit_sounds: vec![Default::default(); hit_objects.len()],
            hit_objects,
            ..Default::default()
        };
        let mut gradual = TaikoGradualDifficulty::new(Difficulty::new(), &map).unwrap();

        assert_eq!(gradual.len(), 3);
        assert_eq!(gradual.skill_idx, 0);
        assert!(gradual.advance());
        assert!(gradual.advance());
        assert_eq!(gradual.skill_idx, 0);

        let attrs = gradual.attributes();
        assert_eq!(attrs.max_combo, 1);
        let strains = gradual.current_strains();
        assert_eq_attrs(&attrs, &gradual.attributes());
        assert_eq!(strains, gradual.current_strains());
        assert_eq!(gradual.len(), 1);
        assert_eq!(gradual.skill_idx, 0);

        assert!(gradual.advance());
        assert_eq!(gradual.attributes().max_combo, 1);
        assert_eq!(gradual.skill_idx, 1);
        assert_eq!(gradual.len(), 0);
        assert!(!gradual.advance());
    }

    #[test]
    fn gradual_spinner_and_swell_are_non_hit_top_level_objects() {
        let hit_objects = vec![
            HitObject {
                pos: Pos::default(),
                start_time: 100.0,
                kind: HitObjectKind::Circle,
            },
            HitObject {
                pos: Pos::default(),
                start_time: 200.0,
                kind: HitObjectKind::Spinner(Spinner { duration: 800.0 }),
            },
            HitObject {
                pos: Pos::default(),
                start_time: 400.0,
                kind: HitObjectKind::Hold(HoldNote { duration: 1_000.0 }),
            },
        ];
        let map = Beatmap {
            mode: GameMode::Taiko,
            hit_sounds: vec![Default::default(); hit_objects.len()],
            hit_objects,
            ..Default::default()
        };
        let mut gradual = TaikoGradualDifficulty::new(Difficulty::new(), &map).unwrap();

        assert_eq!(gradual.skip_to_end(), 3);
        assert_eq!(gradual.idx, 3);
        assert_eq!(gradual.attributes().max_combo, 1);
        assert_eq!(gradual.skip_to_end(), 0);
    }

    #[test]
    fn gradual_slider_end_time_uses_taiko_precision_clamp() {
        assert_eq!(get_precision_adjusted_beat_len(0.001, 500.0), 50_000.0);
        assert_eq!(get_precision_adjusted_beat_len(100.0, 500.0), 50.0);
    }
}
