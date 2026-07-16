use std::mem;

use rosu_map::section::general::GameMode;

use crate::{
    Beatmap, Difficulty,
    any::CalculateError,
    model::mode::ConvertError,
    osu::{
        OsuStrains,
        convert::convert_objects,
        legacy_score_simulator::gradual::GradualLegacyScoreSimulator,
        object::{OsuObject, OsuObjectKind},
        utils::legacy_score::GradualNestedScorePerObject,
    },
};

use self::osu_objects::OsuObjects;

use super::{
    DifficultyValues, OsuDifficultyAttributes, OsuDifficultySetup, object::OsuDifficultyObject,
    skills::OsuSkills,
};

/// Gradually calculate the difficulty attributes of an osu!standard map.
///
/// Note that this struct implements [`Iterator`].
/// On every call of [`Iterator::next`], the map's next hit object will
/// be processed and the [`OsuDifficultyAttributes`] will be updated and
/// returned.
///
/// If you want to calculate performance attributes, use
/// [`OsuGradualPerformance`] instead.
///
/// # Example
///
/// ```
/// use rosu_pp::{Beatmap, Difficulty};
/// use rosu_pp::osu::{Osu, OsuGradualDifficulty};
///
/// let map = Beatmap::from_path("./resources/2785319.osu").unwrap();
///
/// let difficulty = Difficulty::new().mods(64); // DT
/// let mut iter = OsuGradualDifficulty::new(difficulty, &map).unwrap();
///
///  // the difficulty of the map after the first hit object
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
/// [`OsuGradualPerformance`]: crate::osu::OsuGradualPerformance
pub struct OsuGradualDifficulty {
    pub(crate) idx: usize,
    pub(crate) difficulty: Difficulty,
    attrs: OsuDifficultyAttributes,
    skills: OsuSkills,
    skill_idx: usize,
    // Lifetimes actually depend on `osu_objects` so this type is
    // self-referential. This field must be treated with great caution, moving
    // `osu_objects` will immediately invalidate `diff_objects`.
    diff_objects: Box<[OsuDifficultyObject<'static>]>,
    osu_objects: OsuObjects,
    object_end_times: Box<[f64]>,
    // Boxed to reduce the field's size
    score_simulator: Box<GradualLegacyScoreSimulator>,
    nested_score: GradualNestedScorePerObject,
    // Additional safety measure that this type can't be cloned as it would
    // invalidate `diff_objects`.
    _not_clonable: NotClonable,
}

struct NotClonable;

impl OsuGradualDifficulty {
    /// Create a new difficulty attributes iterator for osu!standard maps.
    pub fn new(difficulty: Difficulty, map: &Beatmap) -> Result<Self, ConvertError> {
        let map = super::prepare_map(&difficulty, map)?;

        Ok(new(difficulty, &map))
    }

    /// Same as [`OsuGradualDifficulty::new`] but verifies that the map is not
    /// suspicious.
    pub fn checked_new(difficulty: Difficulty, map: &Beatmap) -> Result<Self, CalculateError> {
        let map = super::prepare_map(&difficulty, map)?;
        map.check_suspicion()?;

        Ok(new(difficulty, &map))
    }

    fn increment_combo(h: &OsuObject, attrs: &mut OsuDifficultyAttributes) {
        attrs.max_combo += 1;

        match &h.kind {
            OsuObjectKind::Circle => attrs.n_circles += 1,
            OsuObjectKind::Slider(slider) => {
                attrs.n_sliders += 1;
                attrs.n_large_ticks += slider.large_tick_count() as u32;
                attrs.max_combo += slider.nested_objects.len() as u32;
            }
            OsuObjectKind::Spinner { .. } => attrs.n_spinners += 1,
        }
    }
}

fn new(difficulty: Difficulty, map: &Beatmap) -> OsuGradualDifficulty {
    debug_assert_eq!(map.mode, GameMode::Osu);

    let mods = difficulty.get_mods();

    let OsuDifficultySetup {
        scaling_factor,
        map_attrs,
        mut attrs,
        time_preempt,
    } = OsuDifficultySetup::new(&difficulty, map);

    let osu_objects = convert_objects(
        map,
        &scaling_factor,
        mods.reflection(),
        mods.freeze_frame(),
        time_preempt,
        map.hit_objects.len(),
        &mut attrs,
    );

    attrs.n_circles = 0;
    attrs.n_sliders = 0;
    attrs.n_large_ticks = 0;
    attrs.n_spinners = 0;
    attrs.max_combo = 0;

    let object_end_times = osu_objects
        .iter()
        .map(OsuObject::end_time)
        .collect::<Box<[_]>>();

    let mut osu_objects = OsuObjects::new(osu_objects);

    let diff_objects = DifficultyValues::create_difficulty_objects(
        &difficulty,
        &scaling_factor,
        osu_objects.iter_mut(),
    );

    let great_hit_window = map_attrs.hit_windows().od_great.unwrap_or(0.0);

    let skills = OsuSkills::new(
        mods,
        &scaling_factor,
        great_hit_window,
        time_preempt,
        difficulty.get_clock_rate(),
        map.hit_objects.len() as u32,
    );
    let diff_objects = extend_lifetime(diff_objects.into_boxed_slice());

    let score_simulator = GradualLegacyScoreSimulator::new(map, map_attrs);
    let nested_score = GradualNestedScorePerObject::default();

    OsuGradualDifficulty {
        idx: 0,
        difficulty,
        attrs,
        skill_idx: 0,
        skills,
        diff_objects,
        osu_objects,
        object_end_times,
        score_simulator: Box::new(score_simulator),
        nested_score,
        _not_clonable: NotClonable,
    }
}

fn extend_lifetime(
    diff_objects: Box<[OsuDifficultyObject<'_>]>,
) -> Box<[OsuDifficultyObject<'static>]> {
    // SAFETY: Owned values of the references will be contained in the same
    // struct (same lifetime). Also, the only mutable access wraps them in
    // `Pin` to ensure that they won't move.
    unsafe { mem::transmute(diff_objects) }
}

impl OsuGradualDifficulty {
    pub fn advance(&mut self) -> bool {
        let Some(h) = self.osu_objects.get(self.idx) else {
            return false;
        };

        let score_attrs = self.score_simulator.simulate_next(h);
        self.attrs.maximum_legacy_combo_score = score_attrs.combo_score as f64;
        self.attrs.legacy_score_base_multiplier = self.score_simulator.score_multiplier(h, false);
        self.attrs.nested_score_per_object = self.nested_score.calculate_next(h);
        Self::increment_combo(h, &mut self.attrs);

        let end_time = h.end_time();

        while let Some(curr) = self.diff_objects.get(self.skill_idx) {
            if curr.base.end_time() > end_time {
                break;
            }

            self.skills.process(curr, &self.diff_objects);
            self.skill_idx += 1;
        }

        self.idx += 1;

        true
    }

    /// Append up to `count` top-level objects as one source batch, then
    /// process difficulty objects only through the final appended end time.
    pub fn skip(&mut self, count: usize) -> usize {
        let take = count.min(self.osu_objects.len().saturating_sub(self.idx));
        let Some(final_idx) = self
            .idx
            .checked_add(take)
            .and_then(|end| end.checked_sub(1))
        else {
            return 0;
        };

        for _ in 0..take {
            let h = self
                .osu_objects
                .get(self.idx)
                .expect("batch index is in bounds");
            let score_attrs = self.score_simulator.simulate_next(h);
            self.attrs.maximum_legacy_combo_score = score_attrs.combo_score as f64;
            self.attrs.legacy_score_base_multiplier =
                self.score_simulator.score_multiplier(h, false);
            self.attrs.nested_score_per_object = self.nested_score.calculate_next(h);
            Self::increment_combo(h, &mut self.attrs);
            self.idx += 1;
        }

        let end_time = self
            .osu_objects
            .get(final_idx)
            .expect("final batch index is in bounds")
            .end_time();

        while let Some(curr) = self.diff_objects.get(self.skill_idx) {
            if curr.base.end_time() > end_time {
                break;
            }

            self.skills.process(curr, &self.diff_objects);
            self.skill_idx += 1;
        }

        take
    }

    /// Skip all remaining top-level objects using the pinned calculator's
    /// `GradualDifficultyEnumerator.SkipToEnd` cutoff semantics.
    ///
    /// The source adds every top-level object first and then processes
    /// difficulty objects only through the end time of the final top-level
    /// object. Consequently, an earlier slider that ends after the final
    /// object blocks itself and all following difficulty objects. This is
    /// intentionally distinct from repeatedly calling [`Self::advance`].
    pub fn skip_to_end(&mut self) {
        while let Some(h) = self.osu_objects.get(self.idx) {
            let score_attrs = self.score_simulator.simulate_next(h);
            self.attrs.maximum_legacy_combo_score = score_attrs.combo_score as f64;
            self.attrs.legacy_score_base_multiplier =
                self.score_simulator.score_multiplier(h, false);
            self.attrs.nested_score_per_object = self.nested_score.calculate_next(h);
            Self::increment_combo(h, &mut self.attrs);
            self.idx += 1;
        }

        let Some(&end_time) = self.object_end_times.last() else {
            return;
        };

        while let Some(curr) = self.diff_objects.get(self.skill_idx) {
            if curr.base.end_time() > end_time {
                break;
            }

            self.skills.process(curr, &self.diff_objects);
            self.skill_idx += 1;
        }
    }

    /// End times of all prepared, converted top-level playable objects.
    pub fn object_end_times(&self) -> &[f64] {
        &self.object_end_times
    }

    /// Current strain state for the already processed prefix.
    ///
    /// Unlike `Difficulty::strains`, this does not reconstruct or process the
    /// prefix again. Flashlight is empty when the mod is absent, matching the
    /// source calculator's omitted Flashlight skill.
    pub fn current_strains(&self) -> OsuStrains {
        let object_count = self.skill_idx;
        let clock_rate = self.difficulty.get_clock_rate();
        let object_start_times = self.diff_objects[..object_count]
            .iter()
            .map(|object| object.start_time)
            .collect();
        let previous_object_start_times = self.diff_objects[..object_count]
            .iter()
            .map(|object| {
                self.osu_objects
                    .get(object.idx)
                    .map_or(0.0, |previous| previous.start_time / clock_rate)
            })
            .collect();
        let flashlight = if self.difficulty.get_mods().fl() {
            self.skills.flashlight.current_strain_peaks()
        } else {
            Vec::new()
        };

        OsuStrains {
            aim: self.skills.aim.object_difficulties().to_vec(),
            aim_no_sliders: self.skills.aim_no_sliders.object_difficulties().to_vec(),
            speed: self.skills.speed.object_difficulties().to_vec(),
            reading: self.skills.reading.object_difficulties().to_vec(),
            flashlight,
            object_start_times,
            previous_object_start_times,
        }
    }

    pub fn attributes(&mut self) -> OsuDifficultyAttributes {
        let mut attrs = self.attrs.clone();
        DifficultyValues::eval(&mut attrs, self.difficulty.get_mods(), &mut self.skills);

        attrs
    }
}

impl Iterator for OsuGradualDifficulty {
    type Item = OsuDifficultyAttributes;

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

impl ExactSizeIterator for OsuGradualDifficulty {
    fn len(&self) -> usize {
        self.osu_objects.len() - self.idx
    }
}

mod osu_objects {
    use std::pin::Pin;

    use crate::osu::object::OsuObject;

    /// Wrapper to ensure that the data will not be moved
    pub(super) struct OsuObjects {
        objects: Box<[OsuObject]>,
    }

    impl OsuObjects {
        pub(super) const fn new(objects: Box<[OsuObject]>) -> Self {
            Self { objects }
        }

        pub(super) fn get(&self, idx: usize) -> Option<&OsuObject> {
            self.objects.get(idx)
        }

        pub(super) const fn len(&self) -> usize {
            self.objects.len()
        }

        pub(super) const fn is_empty(&self) -> bool {
            self.objects.is_empty()
        }

        pub(super) fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = Pin<&mut OsuObject>> {
            self.objects.iter_mut().map(Pin::new)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Beatmap, osu::Osu};

    use super::*;

    #[test]
    fn empty() {
        let map = Beatmap::from_bytes(&[]).unwrap();
        let mut gradual = OsuGradualDifficulty::new(Difficulty::new(), &map).unwrap();
        assert!(gradual.next().is_none());
    }

    #[test]
    fn next_and_nth() {
        let map = Beatmap::from_path(crate::test_resource("2785319.osu")).unwrap();

        let difficulty = Difficulty::new();

        let mut gradual = OsuGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_2nd = OsuGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_3rd = OsuGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut nth_len = OsuGradualDifficulty::new(difficulty.clone(), &map).unwrap();
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

            let expected = difficulty
                .clone()
                .passed_objects(i as u32)
                .calculate_for_mode::<Osu>(&map)
                .unwrap();

            assert_eq!(next_gradual, expected);
        }
    }
    #[test]
    fn exposes_prepared_end_times_and_live_skill_state() {
        let map = Beatmap::from_bytes(include_bytes!("../../../../../tests/fixture.osu")).unwrap();
        let mut gradual = OsuGradualDifficulty::new(Difficulty::new(), &map).unwrap();

        assert_eq!(gradual.object_end_times().len(), map.hit_objects.len());
        assert!(
            gradual
                .object_end_times()
                .iter()
                .zip(&map.hit_objects)
                .any(|(&end_time, object)| end_time > object.start_time)
        );

        let empty = gradual.current_strains();
        assert!(empty.aim.is_empty());
        assert!(empty.aim_no_sliders.is_empty());
        assert!(empty.speed.is_empty());
        assert!(empty.reading.is_empty());
        assert!(empty.flashlight.is_empty());

        assert!(gradual.advance());
        assert!(gradual.current_strains().aim.is_empty());
        assert!(gradual.advance());
        let current = gradual.current_strains();
        assert_eq!(current.aim.len(), 1);
        assert_eq!(current.aim_no_sliders.len(), 1);
        assert_eq!(current.speed.len(), 1);
        assert_eq!(current.reading.len(), 1);
        assert_eq!(current.object_start_times.len(), 1);
        assert_eq!(current.previous_object_start_times.len(), 1);
        assert!(current.flashlight.is_empty());
    }

    #[test]
    fn flashlight_skill_state_exists_only_with_flashlight() {
        let map = Beatmap::from_bytes(include_bytes!("../../../../../tests/fixture.osu")).unwrap();
        let mut gradual = OsuGradualDifficulty::new(Difficulty::new().mods(1024), &map).unwrap();
        assert!(gradual.advance());
        assert!(gradual.advance());
        assert!(!gradual.current_strains().flashlight.is_empty());
    }

    #[test]
    fn repeated_projection_is_idempotent() {
        let map = Beatmap::from_bytes(include_bytes!("../../../../../tests/fixture.osu")).unwrap();
        let difficulty = Difficulty::new();
        let expected = difficulty.calculate_for_mode::<Osu>(&map).unwrap();
        let mut gradual = OsuGradualDifficulty::new(difficulty, &map).unwrap();

        while gradual.advance() {}

        let first = gradual.attributes();
        let repeated = gradual.attributes();
        let third = gradual.attributes();

        assert_eq!(first, expected);
        assert_eq!(first, repeated);
        assert_eq!(repeated, third);
    }

    #[test]
    fn skip_to_end_stops_at_final_top_level_end() {
        let map = Beatmap::from_bytes(
            br#"osu file format v14

[General]
Mode:0

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
SliderMultiplier:1
SliderTickRate:1

[TimingPoints]
0,500,4,2,0,100,1,0

[HitObjects]
64,64,500,1,0,0:0:0:0:
128,128,1000,2,0,B|256:128,1,1000
192,192,2000,1,0,0:0:0:0:
256,64,3000,1,0,0:0:0:0:
320,192,4000,1,0,0:0:0:0:
"#,
        )
        .unwrap();
        let difficulty = Difficulty::new();
        let mut skipped = OsuGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut advanced = OsuGradualDifficulty::new(difficulty, &map).unwrap();

        skipped.skip_to_end();
        while advanced.advance() {}

        assert_eq!(skipped.len(), 0);
        assert_eq!(skipped.skill_idx, 0);
        assert!(advanced.skill_idx > skipped.skill_idx);
        assert_eq!(skipped.attributes().aim, 0.0);
        assert!(advanced.attributes().aim > 0.0);
    }

    #[test]
    fn advance_drains_difficulty_objects_through_appended_object_end() {
        let map = Beatmap::from_bytes(
            br#"osu file format v14

[General]
Mode:0

[Difficulty]
HPDrainRate:5
CircleSize:4
OverallDifficulty:8
ApproachRate:9
SliderMultiplier:1
SliderTickRate:1

[TimingPoints]
0,500,4,2,0,100,1,0

[HitObjects]
64,64,500,1,0,0:0:0:0:
128,128,1000,2,0,B|256:128,1,1000
192,192,2000,1,0,0:0:0:0:
256,64,3000,1,0,0:0:0:0:
320,192,4000,1,0,0:0:0:0:
"#,
        )
        .unwrap();
        let mut gradual = OsuGradualDifficulty::new(Difficulty::new(), &map).unwrap();

        assert!(gradual.advance());
        assert_eq!(gradual.skill_idx, 0);
        assert!(gradual.advance());

        // The second appended object is the long slider ending after every
        // later circle. Target Advance therefore drains all corresponding
        // difficulty objects even though only two top-level objects are in
        // the progressive beatmap.
        assert_eq!(gradual.idx, 2);
        assert_eq!(gradual.skill_idx, gradual.diff_objects.len());
        let attrs = gradual.attributes();
        assert_eq!(attrs.n_circles, 1);
        assert_eq!(attrs.n_sliders, 1);
        assert!(attrs.aim > 0.0);

        let processed_skills = gradual.skill_idx;
        assert!(gradual.advance());
        assert_eq!(gradual.skill_idx, processed_skills);
    }
}
