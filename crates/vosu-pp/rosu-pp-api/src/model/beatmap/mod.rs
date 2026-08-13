use std::{borrow::Cow, io, path::Path, str::FromStr};

use rosu_map::{
    LATEST_FORMAT_VERSION,
    section::{general::GameMode, hit_objects::hit_samples::HitSoundType},
};

pub use rosu_map::section::events::BreakPeriod;

use crate::{
    Difficulty, GameMods, GradualDifficulty, GradualPerformance, Performance, catch::Catch,
    mania::Mania, taiko::Taiko, util::sort,
};

pub use self::{
    attributes::{
        AdjustedBeatmapAttributes, BeatmapAttribute, BeatmapAttributes, BeatmapAttributesBuilder,
        HitWindows,
    },
    decode::{BeatmapState, ParseBeatmapError},
    suspicious::TooSuspicious,
};

pub(crate) use self::attributes::BeatmapAttributesExt;

use super::{
    control_point::{
        DifficultyPoint, EffectPoint, TimingPoint, difficulty_point_at, effect_point_at,
        timing_point_at,
    },
    hit_object::{HitObject, HitObjectKind},
    mode::ConvertError,
};

pub(crate) mod attributes;
mod bpm;
mod decode;
mod suspicious;

/// All beatmap data that is relevant for difficulty and performance
/// calculation.
#[derive(Clone, Debug, PartialEq)]
pub struct Beatmap {
    pub version: i32,
    pub is_convert: bool,

    // General
    pub stack_leniency: f32,
    pub mode: GameMode,

    // Difficulty
    pub ar: f32,
    pub cs: f32,
    pub hp: f32,
    pub od: f32,
    pub slider_multiplier: f64,
    pub slider_tick_rate: f64,

    // Events
    pub breaks: Vec<BreakPeriod>,

    // TimingPoints
    pub timing_points: Vec<TimingPoint>,
    pub difficulty_points: Vec<DifficultyPoint>,
    pub effect_points: Vec<EffectPoint>,

    // HitObjects
    pub hit_objects: Vec<HitObject>,
    pub hit_sounds: Vec<HitSoundType>,
}

impl Beatmap {
    /// Parse a [`Beatmap`] by providing a path to a `.osu` file.
    pub fn from_path<P: AsRef<Path>>(path: P) -> Result<Self, io::Error> {
        rosu_map::from_path(path)
    }

    /// Parse a [`Beatmap`] by providing the content of a `.osu` file as a
    /// slice of bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, io::Error> {
        rosu_map::from_bytes(bytes)
    }

    /// Returns a [`BeatmapAttributesBuilder`] to calculate modified beatmap
    /// attributes.
    pub fn attributes(&self) -> BeatmapAttributesBuilder {
        BeatmapAttributesBuilder::from(self)
    }

    /// The beats per minute of the map.
    pub fn bpm(&self) -> f64 {
        bpm::bpm(self.hit_objects.last(), &self.timing_points)
    }

    /// Create a performance calculator for this [`Beatmap`].
    pub fn performance(&self) -> Performance<'_> {
        Performance::new(self)
    }

    /// Create a gradual difficulty calculator for this [`Beatmap`].
    pub fn gradual_difficulty(&self, difficulty: Difficulty) -> GradualDifficulty {
        GradualDifficulty::new(difficulty, self)
    }

    /// Create a gradual performance calculator for this [`Beatmap`].
    pub fn gradual_performance(&self, difficulty: Difficulty) -> GradualPerformance {
        GradualPerformance::new(difficulty, self)
    }

    /// Finds the [`TimingPoint`] that is active at the given time.
    pub(crate) fn timing_point_at(&self, time: f64) -> Option<&TimingPoint> {
        timing_point_at(&self.timing_points, time)
    }
    /// Returns the calculated osu! slider path, including the path's absolute
    /// end position, bounds, and centre of mass.
    pub fn osu_slider_geometry(
        &self,
        index: usize,
    ) -> Option<(
        Vec<rosu_map::util::Pos>,
        rosu_map::util::Pos,
        [f32; 4],
        rosu_map::util::Pos,
    )> {
        let object = self.hit_objects.get(index)?;
        let HitObjectKind::Slider(slider) = &object.kind else {
            return None;
        };
        let mut bufs = rosu_map::section::hit_objects::CurveBuffers::default();
        let curve = slider.curve(
            GameMode::Osu,
            crate::model::mods::Reflection::None,
            &mut bufs,
        );
        let path = curve.path().to_vec();
        let span_count = slider.span_count();
        let end_progress = if span_count % 2 == 0 { 0.0 } else { 1.0 };
        let end = curve.position_at(end_progress);
        let mut min_x = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for pos in &path {
            min_x = min_x.min(pos.x);
            max_x = max_x.max(pos.x);
            min_y = min_y.min(pos.y);
            max_y = max_y.max(pos.y);
        }
        let distance = curve.dist();
        let centroid = if distance <= 50.0 {
            let pos = curve.position_at(1.0);
            rosu_map::util::Pos::new(pos.x / 2.0, pos.y / 2.0)
        } else {
            let mut count = 0.0;
            let mut sum = rosu_map::util::Pos::default();
            let mut travelled = 0.0;
            while travelled < distance {
                let pos = curve.position_at(travelled / distance);
                sum.x += pos.x;
                sum.y += pos.y;
                count += 1.0;
                travelled += 50.0;
            }
            rosu_map::util::Pos::new(sum.x / count, sum.y / count)
        };
        Some((path, end, [min_x, max_x, min_y, max_y], centroid))
    }
    /// Rotates an osu! slider around its start position and returns its
    /// resulting geometry.
    pub fn osu_slider_rotate(
        &mut self,
        index: usize,
        rotation: f32,
    ) -> Option<(
        Vec<rosu_map::util::Pos>,
        rosu_map::util::Pos,
        [f32; 4],
        rosu_map::util::Pos,
    )> {
        let object = self.hit_objects.get_mut(index)?;
        let HitObjectKind::Slider(slider) = &mut object.kind else {
            return None;
        };
        let (sin, cos) = rotation.sin_cos();
        for point in slider.control_points.iter_mut() {
            let x = point.pos.x;
            let y = point.pos.y;
            point.pos.x = x * cos - y * sin;
            point.pos.y = x * sin + y * cos;
        }
        self.osu_slider_geometry(index)
    }
    /// Returns the angle of an osu! slider's path endpoint relative to its head.
    pub fn osu_slider_rotation(&self, index: usize) -> Option<f32> {
        let (path, _, _, _) = self.osu_slider_geometry(index)?;
        Some(path.last()?.y.atan2(path.last()?.x))
    }

    /// Flips an osu! slider's control points horizontally around its head.
    pub fn osu_slider_flip_horizontal(&mut self, index: usize) -> bool {
        let Some(object) = self.hit_objects.get_mut(index) else {
            return false;
        };
        let HitObjectKind::Slider(slider) = &mut object.kind else {
            return false;
        };
        for point in slider.control_points.iter_mut() {
            point.pos.x = -point.pos.x;
        }
        true
    }

    /// Finds the [`DifficultyPoint`] that is active at the given time.
    pub(crate) fn difficulty_point_at(&self, time: f64) -> Option<&DifficultyPoint> {
        difficulty_point_at(&self.difficulty_points, time)
    }

    /// Finds the [`EffectPoint`] that is active at the given time.
    pub(crate) fn effect_point_at(&self, time: f64) -> Option<&EffectPoint> {
        effect_point_at(&self.effect_points, time)
    }

    /// Sum up the duration of all breaks (in milliseconds).
    pub fn total_break_time(&self) -> f64 {
        self.breaks.iter().map(BreakPeriod::duration).sum()
    }

    /// Attempt to convert a [`Beatmap`] to the specified mode.
    pub fn convert(mut self, mode: GameMode, mods: &GameMods) -> Result<Self, ConvertError> {
        self.convert_mut(mode, mods)?;

        Ok(self)
    }

    /// Attempt to convert a [`&Beatmap`] to the specified mode.
    ///
    /// [`&Beatmap`]: Beatmap
    pub fn convert_ref(
        &self,
        mode: GameMode,
        mods: &GameMods,
    ) -> Result<Cow<'_, Self>, ConvertError> {
        if self.mode == mode {
            return Ok(Cow::Borrowed(self));
        } else if self.is_convert {
            return Err(ConvertError::AlreadyConverted);
        } else if self.mode != GameMode::Osu {
            return Err(ConvertError::Convert {
                from: self.mode,
                to: mode,
            });
        }

        let mut map = self.to_owned();

        match mode {
            GameMode::Taiko => Taiko::convert(&mut map),
            GameMode::Catch => Catch::convert(&mut map),
            GameMode::Mania => Mania::convert(&mut map, mods),
            GameMode::Osu => unreachable!(),
        }

        Ok(Cow::Owned(map))
    }

    /// Attempt to convert a [`&mut Beatmap`] to the specified mode.
    ///
    /// [`&mut Beatmap`]: Beatmap
    pub fn convert_mut(&mut self, mode: GameMode, mods: &GameMods) -> Result<(), ConvertError> {
        if self.mode == mode {
            return Ok(());
        } else if self.is_convert {
            return Err(ConvertError::AlreadyConverted);
        } else if self.mode != GameMode::Osu {
            return Err(ConvertError::Convert {
                from: self.mode,
                to: mode,
            });
        }

        match mode {
            GameMode::Taiko => Taiko::convert(self),
            GameMode::Catch => Catch::convert(self),
            GameMode::Mania => Mania::convert(self, mods),
            GameMode::Osu => unreachable!(),
        }

        Ok(())
    }

    /// Sort hitobjects via [`sort::osu_legacy`] with a specific comparator.
    ///
    /// Mania's difficulty calculation applies this sorting *after* all mod
    /// conversions have been applied to the hitobjects.
    ///
    /// <https://github.com/ppy/osu/blob/28c846b4d9366484792e27f4729cd1afa2cdeb66/osu.Game.Rulesets.Mania/Difficulty/ManiaDifficultyCalculator.cs#L70>
    pub(crate) fn mania_hitobjects_legacy_sort(&mut self) {
        sort::osu_legacy(&mut self.hit_objects, |a, b| {
            (f64::round_ties_even(a.start_time) as i32)
                .cmp(&(f64::round_ties_even(b.start_time) as i32))
        });
    }

    /// Check whether hitobjects appear too suspicious for further calculation.
    ///
    /// Sometimes a [`Beatmap`] isn't created for gameplay but rather to test
    /// the limits of osu! itself. Difficulty- and/or performance calculation
    /// should likely be avoided on these maps due to potential performance
    /// issues.
    pub fn check_suspicion(&self) -> Result<(), TooSuspicious> {
        match TooSuspicious::new(self) {
            None => Ok(()),
            Some(err) => Err(err),
        }
    }
}

impl FromStr for Beatmap {
    type Err = io::Error;

    /// Parse a [`Beatmap`] by providing the content of a `.osu` file as a
    /// string.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        rosu_map::from_str(s)
    }
}

const DEFAULT_SLIDER_LENIENCY: f32 = 0.7;

impl Default for Beatmap {
    fn default() -> Self {
        Self {
            version: LATEST_FORMAT_VERSION,
            is_convert: false,
            stack_leniency: DEFAULT_SLIDER_LENIENCY,
            mode: GameMode::default(),
            ar: 5.0,
            cs: 5.0,
            hp: 5.0,
            od: 5.0,
            slider_multiplier: 1.4,
            slider_tick_rate: 1.0,
            breaks: Vec::default(),
            timing_points: Vec::default(),
            difficulty_points: Vec::default(),
            effect_points: Vec::default(),
            hit_objects: Vec::default(),
            hit_sounds: Vec::default(),
        }
    }
}
