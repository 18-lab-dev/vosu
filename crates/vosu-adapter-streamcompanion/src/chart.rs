//! C1 supplies these accurate chart-derived values from the captured chart resource.
//! `None` at the builder boundary means the pinned unparsed-chart state, not a private-path fallback.
use indexmap::IndexMap;
use crate::dto::{MapBreak, MapTimingPoint};
#[derive(Clone, Debug, Default)]
pub struct ChartFacts {
    pub original: Difficulty,
    pub converted: Difficulty,
    pub difficulty: Difficulty,
    pub bpm: Bpm,
    pub times: Times,
    pub objects: Objects,
    pub strains: IndexMap<String, f64>,
    pub breaks: Vec<MapBreak>,
    pub timing_points: Vec<MapTimingPoint>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Difficulty { pub approach_rate: f64, pub overall_difficulty: f64, pub circle_size: f64, pub drain_rate: f64, pub stars: f64, pub max_combo: u32 }
#[derive(Clone, Copy, Debug, Default)]
pub struct Bpm { pub common: f64, pub minimum: f64, pub maximum: f64 }
#[derive(Clone, Copy, Debug, Default)]
pub struct Times { pub full: f64, pub first_object: i32, pub preview: i32 }
#[derive(Clone, Copy, Debug, Default)]
pub struct Objects { pub circles: u32, pub sliders: u32, pub spinners: u32 }
