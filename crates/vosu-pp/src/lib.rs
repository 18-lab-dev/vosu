//! Native Rust live-calculation facade used by the vosu state engine.
//! Source-provenance review is recorded in `docs/provenance-audit.md`.
//!
//! Public behavior is implemented against `rosu-pp` 4.0.1 and the pinned
//! MIT-licensed osu! source documented in `THIRD_PARTY_LICENSES.txt`.

#![forbid(unsafe_code)]

mod beatmap;
mod data;
mod difficulty;
mod gradual;
mod hit_windows;
mod model;
mod mods;
mod performance;
mod score_multiplier;
mod scoring;
pub mod source_map;

pub use beatmap::*;
pub use data::{
    BeatmapDifficultyData, DifficultyAttrsData, PeakStrains, PerformanceAttrsData, StrainsData,
};
pub use gradual::*;
pub use hit_windows::*;
pub use model::*;
pub use mods::*;
pub use score_multiplier::{ScoreMultiplierScoreContext, score_multiplier};
pub use scoring::*;

/// Public `rosu-pp` baseline identity retained by the calculator contract.
pub const ROSU_BASELINE: &str = "v4.0.1 / osu! 28c846b4";
/// Exact MIT `rosu-pp` source revision used for the local fork.
pub const ROSU_SOURCE_REVISION: &str = "27a67242cad155a2767f5a25a423ca4ec3e3c274";
/// Baseline osu! revision for the original forward port and retained NUnit evidence.
pub const OSU_FORWARD_REVISION: &str = "97e94babde9d5ecbe574ecdd33509db27452cd0f";
