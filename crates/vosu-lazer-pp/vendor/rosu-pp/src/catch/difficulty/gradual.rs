use rosu_map::section::{general::GameMode, hit_objects::CurveBuffers};

use crate::{
    Beatmap, Difficulty,
    any::{CalculateError, difficulty::skills::StrainSkill},
    catch::{
        CatchDifficultyAttributes, CatchStrains,
        attributes::{ObjectCount, ObjectCountBuilder},
        catcher::Catcher,
        convert::convert_objects,
        object::juice_stream::precision_adjusted_beat_len,
    },
    model::{
        control_point::{DifficultyPoint, TimingPoint},
        hit_object::{HitObject, HitObjectKind, HoldNote, Spinner},
        mode::ConvertError,
    },
};

use super::{
    CatchDifficultySetup, DifficultyValues, object::CatchDifficultyObject,
    skills::movement::Movement,
};

/// Gradually calculate the difficulty attributes of an osu!catch map.
///
/// Note that this struct implements [`Iterator`].
/// On every call of [`Iterator::next`], the map's next fruit or droplet
/// will be processed and the [`CatchDifficultyAttributes`] will be updated and
/// returned.
///
/// Note that it does not return attributes after a tiny droplet. Only for
/// fruits and droplets.
///
/// If you want to calculate performance attributes, use
/// [`CatchGradualPerformance`] instead.
///
/// # Example
///
/// ```
/// use rosu_pp::{Beatmap, Difficulty};
/// use rosu_pp::catch::{Catch, CatchGradualDifficulty};
///
/// let map = Beatmap::from_path("./resources/2118524.osu").unwrap();
///
/// let difficulty = Difficulty::new().mods(64); // DT
/// let mut iter = CatchGradualDifficulty::new(difficulty, &map).unwrap();
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
/// [`CatchGradualPerformance`]: crate::catch::CatchGradualPerformance
pub struct CatchGradualDifficulty {
    pub(crate) idx: usize,
    pub(crate) difficulty: Difficulty,
    attrs: CatchDifficultyAttributes,
    count: Vec<ObjectCount>,
    object_end_times: Box<[f64]>,
    diff_objects: Box<[CatchDifficultyObject]>,
    diff_idx: usize,
    movement: Movement,
}

impl CatchGradualDifficulty {
    /// Create a new difficulty attributes iterator for osu!catch maps.
    pub fn new(difficulty: Difficulty, map: &Beatmap) -> Result<Self, ConvertError> {
        let map = super::prepare_map(&difficulty, map)?;

        Ok(new(difficulty, &map))
    }

    /// Same as [`CatchGradualDifficulty::new`] but verifies that the map is not
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

    /// Current movement strain peaks, including the in-progress section.
    pub fn current_strains(&self) -> CatchStrains {
        CatchStrains {
            movement: self.movement.clone().into_current_strain_peaks(),
        }
    }
}

fn new(difficulty: Difficulty, map: &Beatmap) -> CatchGradualDifficulty {
    debug_assert_eq!(map.mode, GameMode::Catch);

    let clock_rate = difficulty.get_clock_rate();

    let CatchDifficultySetup { map_attrs, attrs } = CatchDifficultySetup::new(&difficulty, map);

    let hr_offsets = difficulty.get_hardrock_offsets();
    let reflection = difficulty.get_mods().reflection();
    let mut count = ObjectCountBuilder::new_gradual();

    let palpable_objects = convert_objects(map, &mut count, reflection, hr_offsets, map_attrs.cs());

    let mut half_catcher_width = Catcher::calculate_catch_width(map_attrs.cs()) * 0.5;
    half_catcher_width *= 1.0 - ((map_attrs.cs() - 5.5).max(0.0) * 0.0625);

    let diff_objects = DifficultyValues::create_difficulty_objects(
        clock_rate,
        half_catcher_width,
        palpable_objects.iter(),
    );

    let (_, count) = count.into_gradual();
    let movement = Movement::new(clock_rate);
    let mut curve_bufs = CurveBuffers::default();
    let object_end_times = map
        .hit_objects
        .iter()
        .map(|object| object_end_time(map, object, &mut curve_bufs))
        .collect();

    CatchGradualDifficulty {
        idx: 0,
        difficulty,
        attrs,
        count,
        object_end_times,
        diff_objects,
        diff_idx: 0,
        movement,
    }
}

fn object_end_time(map: &Beatmap, object: &HitObject, curve_bufs: &mut CurveBuffers) -> f64 {
    match &object.kind {
        HitObjectKind::Circle => object.start_time,
        HitObjectKind::Spinner(Spinner { duration })
        | HitObjectKind::Hold(HoldNote { duration }) => object.start_time + duration,
        HitObjectKind::Slider(slider) => {
            let dist = slider.borrowed_curve(GameMode::Catch, curve_bufs).dist();
            let beat_len = map
                .timing_point_at(object.start_time)
                .map_or(TimingPoint::DEFAULT_BEAT_LEN, |point| point.beat_len);
            let slider_velocity = map
                .difficulty_point_at(object.start_time)
                .map_or(DifficultyPoint::DEFAULT_SLIDER_VELOCITY, |point| {
                    point.slider_velocity
                });
            let beat_len = precision_adjusted_beat_len(slider_velocity, beat_len);
            let velocity = 100.0 * map.slider_multiplier / beat_len;

            object.start_time + slider.span_count() as f64 * dist / velocity
        }
    }
}

impl CatchGradualDifficulty {
    pub(crate) fn advance(&mut self) -> bool {
        let Some((&count, &end_time)) = self
            .count
            .get(self.idx)
            .zip(self.object_end_times.get(self.idx))
        else {
            return false;
        };

        self.attrs.add_top_level_count(count);

        while let Some(curr) = self.diff_objects.get(self.diff_idx) {
            if curr.base_end_time > end_time {
                break;
            }

            self.movement.process(curr, &self.diff_objects);
            self.diff_idx += 1;
        }

        self.idx += 1;

        true
    }

    /// Append up to `count` top-level objects as one source batch, then
    /// process flattened difficulty objects only through the final appended
    /// top-level object's end time.
    pub fn skip(&mut self, count: usize) -> usize {
        let take = count.min(self.count.len().saturating_sub(self.idx));
        let Some(final_idx) = self
            .idx
            .checked_add(take)
            .and_then(|end| end.checked_sub(1))
        else {
            return 0;
        };

        for _ in 0..take {
            self.attrs.add_top_level_count(self.count[self.idx]);
            self.idx += 1;
        }

        let end_time = self.object_end_times[final_idx];

        while let Some(curr) = self.diff_objects.get(self.diff_idx) {
            if curr.base_end_time > end_time {
                break;
            }

            self.movement.process(curr, &self.diff_objects);
            self.diff_idx += 1;
        }

        take
    }

    /// Append every remaining top-level object as one source batch.
    pub fn skip_to_end(&mut self) -> usize {
        Self::skip(self, self.count.len().saturating_sub(self.idx))
    }

    pub(crate) fn attributes(&self) -> CatchDifficultyAttributes {
        let mut attrs = self.attrs.clone();
        let movement = self.movement.cloned_difficulty_value();

        DifficultyValues::eval(&mut attrs, movement);

        attrs
    }
}

impl Iterator for CatchGradualDifficulty {
    type Item = CatchDifficultyAttributes;

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

impl ExactSizeIterator for CatchGradualDifficulty {
    fn len(&self) -> usize {
        self.count.len() - self.idx
    }
}

#[cfg(test)]
mod tests {
    use crate::{Beatmap, catch::Catch};
    use rosu_map::util::Pos;

    use super::*;

    #[test]
    fn empty() {
        let map = Beatmap::from_bytes(&[]).unwrap();
        let mut gradual = CatchGradualDifficulty::new(Difficulty::new(), &map).unwrap();
        assert!(gradual.next().is_none());
    }

    #[test]
    fn next_and_nth() {
        let map = Beatmap::from_path(crate::test_resource("2118524.osu")).unwrap();

        let difficulty = Difficulty::new();

        let mut gradual = CatchGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_2nd = CatchGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut gradual_3rd = CatchGradualDifficulty::new(difficulty.clone(), &map).unwrap();
        let mut nth_len = CatchGradualDifficulty::new(difficulty.clone(), &map).unwrap();
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
                assert_eq!(next_gradual, next_gradual_2nd);
            }

            if i % 3 == 0 {
                let next_gradual_3rd = gradual_3rd.nth(2).unwrap();
                assert_eq!(next_gradual, next_gradual_3rd);
            }

            let passed_objects = next_gradual.n_fruits + next_gradual.n_droplets;
            let expected = difficulty
                .clone()
                .passed_objects(passed_objects)
                .calculate_for_mode::<Catch>(&map)
                .unwrap();

            assert_eq!(next_gradual, expected);
        }
    }

    #[test]
    fn current_strains_include_the_in_progress_section() {
        let hit_objects = (0..12)
            .map(|idx| HitObject {
                pos: Pos::new(if idx % 2 == 0 { 0.0 } else { 512.0 }, 192.0),
                start_time: f64::from(idx) * 100.0,
                kind: HitObjectKind::Circle,
            })
            .collect::<Vec<_>>();
        let map = Beatmap {
            mode: GameMode::Catch,
            hit_sounds: vec![Default::default(); hit_objects.len()],
            hit_objects,
            ..Default::default()
        };
        let mut gradual = CatchGradualDifficulty::new(Difficulty::new(), &map).unwrap();

        assert_eq!(gradual.current_strains().movement, [0.0]);
        assert_eq!(gradual.object_end_times().len(), map.hit_objects.len());

        gradual.nth(10).unwrap();
        let strains = gradual.current_strains();

        assert!(strains.movement.last().is_some_and(|&strain| strain > 0.0));
        assert_eq!(strains, gradual.current_strains());
    }
    #[test]
    fn gradual_advances_once_per_top_level_object_without_observer_mutation() {
        let hit_objects = vec![
            HitObject {
                pos: Pos::new(64.0, 192.0),
                start_time: 100.0,
                kind: HitObjectKind::Circle,
            },
            HitObject {
                pos: Pos::new(256.0, 192.0),
                start_time: 200.0,
                kind: HitObjectKind::Spinner(Spinner { duration: 800.0 }),
            },
            HitObject {
                pos: Pos::new(448.0, 192.0),
                start_time: 1_100.0,
                kind: HitObjectKind::Circle,
            },
        ];
        let map = Beatmap {
            mode: GameMode::Catch,
            hit_sounds: vec![Default::default(); hit_objects.len()],
            hit_objects,
            ..Default::default()
        };
        let mut gradual = CatchGradualDifficulty::new(Difficulty::new(), &map).unwrap();

        assert_eq!(gradual.len(), 3);
        assert!(gradual.advance());
        assert_eq!(gradual.len(), 2);
        assert!(gradual.advance());
        assert_eq!(gradual.len(), 1);

        let diff_idx = gradual.diff_idx;
        let attrs = gradual.attributes();
        let strains = gradual.current_strains();
        assert_eq!(attrs, gradual.attributes());
        assert_eq!(strains, gradual.current_strains());
        assert_eq!(gradual.diff_idx, diff_idx);
        assert_eq!(gradual.len(), 1);

        assert!(gradual.advance());
        assert_eq!(gradual.len(), 0);
        assert!(!gradual.advance());
    }

    #[test]
    fn gradual_slider_advance_adds_all_nested_counts_once() {
        let map = Beatmap::from_bytes(
            b"osu file format v14

[General]
Mode: 2

[Difficulty]
CircleSize: 5
SliderMultiplier: 1.4
SliderTickRate: 1

[TimingPoints]
0,500,4,2,1,50,1,0

[HitObjects]
64,192,100,1,0,0:0:0:0:
256,192,200,2,0,B|448:192,2,384
448,192,4000,1,0,0:0:0:0:
",
        )
        .unwrap();
        let mut gradual = CatchGradualDifficulty::new(Difficulty::new(), &map).unwrap();

        let first = gradual.next().unwrap();
        assert_eq!(
            (first.n_fruits, first.n_droplets, first.n_tiny_droplets),
            (1, 0, 0)
        );

        let slider = gradual.next().unwrap();
        assert!(slider.n_fruits > first.n_fruits);
        assert!(slider.n_droplets > 0);
        let processed = gradual.diff_idx;
        let slider_end_time = gradual.object_end_times[1];
        assert!(processed > 1);
        assert!(
            gradual.diff_objects[..processed]
                .iter()
                .all(|object| object.base_end_time <= slider_end_time)
        );
        assert!(gradual.diff_objects[processed].base_end_time > slider_end_time);
        assert!(slider.n_tiny_droplets > 0);
        assert_eq!(gradual.idx, 2);

        let repeated = gradual.attributes();
        assert_eq!(slider, repeated);
        assert_eq!(gradual.idx, 2);

        let final_attrs = gradual.next().unwrap();
        assert_eq!(final_attrs.n_fruits, slider.n_fruits + 1);
        assert_eq!(final_attrs.n_droplets, slider.n_droplets);
        assert_eq!(final_attrs.n_tiny_droplets, slider.n_tiny_droplets);
        assert!(gradual.next().is_none());
    }

    #[test]
    fn overlapping_juice_streams_drain_flattened_objects_by_palpable_time() {
        let baseline = Beatmap::from_bytes(
            b"osu file format v14

[General]
Mode: 2

[Difficulty]
CircleSize: 5
SliderMultiplier: 1.4
SliderTickRate: 1

[TimingPoints]
0,500,4,2,1,50,1,0

[HitObjects]
64,192,100,2,0,B|448:192,2,384
448,192,5000,1,0,0:0:0:0:
",
        )
        .unwrap();
        let overlapping = Beatmap::from_bytes(
            b"osu file format v14

[General]
Mode: 2

[Difficulty]
CircleSize: 5
SliderMultiplier: 1.4
SliderTickRate: 1

[TimingPoints]
0,500,4,2,1,50,1,0

[HitObjects]
64,192,100,2,0,B|448:192,2,384
448,192,400,2,0,B|64:192,2,384
256,192,5000,1,0,0:0:0:0:
",
        )
        .unwrap();
        let mut baseline = CatchGradualDifficulty::new(Difficulty::new(), &baseline).unwrap();
        let mut overlapping = CatchGradualDifficulty::new(Difficulty::new(), &overlapping).unwrap();

        let baseline_attrs = baseline.next().unwrap();
        let overlapping_attrs = overlapping.next().unwrap();

        assert_eq!(baseline_attrs.n_fruits, overlapping_attrs.n_fruits);
        assert_eq!(baseline_attrs.n_droplets, overlapping_attrs.n_droplets);
        assert_eq!(
            baseline_attrs.n_tiny_droplets,
            overlapping_attrs.n_tiny_droplets
        );
        assert!(
            overlapping.diff_idx > baseline.diff_idx,
            "early flattened objects from the second stream must drain through the first stream's end"
        );
        assert!(
            overlapping.diff_objects[..overlapping.diff_idx]
                .iter()
                .all(|object| object.base_end_time <= overlapping.object_end_times[0])
        );
        assert_eq!(overlapping.idx, 1);
    }
}
