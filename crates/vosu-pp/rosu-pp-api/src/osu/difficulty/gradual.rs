use std::mem;

use rosu_map::section::general::GameMode;

use crate::{
    Beatmap, Difficulty,
    any::{CalculateError, difficulty::skills::StrainSkill},
    model::mode::ConvertError,
    osu::{
        convert::convert_objects,
        legacy_score_simulator::gradual::GradualLegacyScoreSimulator,
        object::{OsuObject, OsuObjectKind},
        strains::OsuStrains,
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
    skill_idx: usize,
    pub(crate) difficulty: Difficulty,
    attrs: OsuDifficultyAttributes,
    skills: OsuSkills,
    // Lifetimes actually depend on `osu_objects` so this type is
    // self-referential. This field must be treated with great caution, moving
    // `osu_objects` will immediately invalidate `diff_objects`.
    diff_objects: Box<[OsuDifficultyObject<'static>]>,
    osu_objects: OsuObjects,
    // Boxed to reduce the field's size
    score_simulator: Box<GradualLegacyScoreSimulator>,
    nested_score: GradualNestedScorePerObject,
    object_end_times: Box<[f64]>,
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
    pub(crate) fn attributes(&self) -> OsuDifficultyAttributes {
        let mut attrs = self.attrs.clone();
        DifficultyValues::eval(&mut attrs, self.difficulty.get_mods(), &self.skills);

        attrs
    }

    pub(crate) fn current_strains(&self) -> OsuStrains {
        OsuStrains {
            aim: self.skills.aim.current_strain_peaks(),
            aim_no_sliders: self.skills.aim_no_sliders.current_strain_peaks(),
            speed: self.skills.speed.csharp_compat_strain_peaks(),
            flashlight: if self.difficulty.get_mods().fl() {
                self.skills.flashlight.current_strain_peaks()
            } else {
                Vec::new()
            },
            reading: self.skills.reading.current_strain_peaks(),
        }
    }
    pub(crate) fn advance(&mut self) -> bool {
        self.skip(1) == 1
    }

    pub(crate) fn skip(&mut self, count: usize) -> usize {
        let take = count.min(self.osu_objects.len().saturating_sub(self.idx));
        let end = self.idx + take;

        for object_idx in self.idx..end {
            let h = self
                .osu_objects
                .get(object_idx)
                .expect("object index is bounded by osu object length");
            let score_attrs = self.score_simulator.simulate_next(h);
            self.attrs.maximum_legacy_combo_score = score_attrs.combo_score as f64;
            self.attrs.legacy_score_base_multiplier =
                self.score_simulator.score_multiplier(h, false);
            self.attrs.nested_score_per_object = self.nested_score.calculate_next(h);
            Self::increment_combo(h, &mut self.attrs);
        }

        if let Some(&object_end_time) = self.object_end_times.get(end.saturating_sub(1)) {
            while let Some(curr) = self.diff_objects.get(self.skill_idx)
                && curr.base.end_time() <= object_end_time
            {
                self.skills.process(curr, &self.diff_objects);
                self.skill_idx += 1;
            }
        }

        self.idx = end;

        take
    }

    pub(crate) fn skip_to_end(&mut self) {
        self.skip(self.osu_objects.len().saturating_sub(self.idx));
    }

    pub(crate) fn object_end_times(&self) -> &[f64] {
        &self.object_end_times
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
        time_preempt,
        mods.fr(),
        map.hit_objects.len(),
        &mut attrs,
    );

    attrs.n_circles = 0;
    attrs.n_sliders = 0;
    attrs.n_large_ticks = 0;
    attrs.n_spinners = 0;
    attrs.max_combo = 0;

    let mut osu_objects = OsuObjects::new(osu_objects);
    let great_hit_window = map_attrs.hit_windows().od_great.unwrap_or(0.0);

    let diff_objects = DifficultyValues::create_difficulty_objects(
        &difficulty,
        &scaling_factor,
        osu_objects.iter_mut(),
        2.0 * great_hit_window,
    );

    let mut skills = OsuSkills::new(
        mods,
        &scaling_factor,
        great_hit_window,
        time_preempt,
        map.hit_objects.len(),
    );
    skills.speed.enable_csharp_compat_projection();
    let diff_objects = extend_lifetime(diff_objects.into_boxed_slice());

    let score_simulator = GradualLegacyScoreSimulator::new(map, map_attrs);
    let nested_score = GradualNestedScorePerObject::default();
    let object_end_times = osu_objects.iter().map(OsuObject::end_time).collect();

    OsuGradualDifficulty {
        idx: 0,
        skill_idx: 0,
        difficulty,
        attrs,
        skills,
        diff_objects,
        osu_objects,
        score_simulator: Box::new(score_simulator),
        nested_score,
        _not_clonable: NotClonable,
        object_end_times,
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
        let remaining = self.len();
        let take = n.saturating_add(1).min(remaining);
        (remaining > 0 && self.skip(take) == take).then(|| self.attributes())
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

        pub(super) fn len(&self) -> usize {
            self.objects.len()
        }

        pub(super) fn iter(&self) -> impl ExactSizeIterator<Item = &OsuObject> {
            self.objects.iter()
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
        let map = Beatmap::from_path("./resources/2785319.osu").unwrap();

        let difficulty = Difficulty::new();

        let mut gradual = OsuGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_2nd = OsuGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_3rd = OsuGradualDifficulty::new(difficulty.clone(), &map).unwrap();

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
}
