//! Native live-calculation facade used by the vosu state engine.
//!
//! The wrapper surface, score simulation, and accuracy rules are a direct Rust
//! port of the upstream `tosuapp/lazer-calculator` project at
//! `9a22c4c74f8e584fa64133a79a204dd045646bac`. The four rulesets begin from the
//! MIT `rosu-pp` v4.0.1 source (`osu!` revision `28c846b4`) and forward-port
//! the authoritative osu! changes through revision
//! `97e94babde9d5ecbe574ecdd33509db27452cd0f`.

#![forbid(unsafe_code)]

mod data;
mod score_multiplier;
mod signed_osu;
mod signed_taiko;
pub mod source_map;

pub use data::{
    BeatmapDifficultyData, DifficultyAttrsData, PeakStrains, PerformanceAttrsData, StrainsData,
};
pub use score_multiplier::{ScoreMultiplierScoreContext, score_multiplier};

use std::{
    borrow::Cow,
    collections::{BTreeMap, hash_map::RandomState},
    hash::{BuildHasher, Hasher},
    sync::{Arc, Mutex, RwLock},
};

use rosu_map::section::general::GameMode;
use rosu_mods::serde::GameModsSeed;
use rosu_pp::{
    Beatmap, Difficulty, GameMods, GradualDifficulty as RosuGradualDifficulty, Performance,
    any::{DifficultyAttributes, PerformanceAttributes, ScoreState, Strains},
};
use serde::{Deserialize, Serialize, de::DeserializeSeed};
use thiserror::Error;

pub const ROSU_BASELINE: &str = "v4.0.1 / osu! 28c846b4";
pub const OSU_FORWARD_REVISION: &str = "97e94babde9d5ecbe574ecdd33509db27452cd0f";
pub const CALCULATOR_REVISION: &str = "9a22c4c74f8e584fa64133a79a204dd045646bac";

include!("model.rs");
include!("beatmap.rs");
include!("engine.rs");
include!("difficulty.rs");
include!("scoring.rs");
include!("gradual.rs");
include!("tests.rs");
