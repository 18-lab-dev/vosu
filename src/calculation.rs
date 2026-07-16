//! Source-compatible calculation and conversion utilities.
//!
//! These helpers mirror the upstream tosu source paths
//! `packages/tosu/src/utils/{calculators,osuMods,osuMods.types,hitResult,converters}.ts`.
//! Performance engines intentionally live elsewhere.

pub mod beatmap;
pub mod engine;
pub mod progressive;
pub mod state_backend;

#[path = "calculation/root/helpers.rs"]
mod helpers;
#[path = "calculation/root/mods.rs"]
mod mods;
#[path = "calculation/root/scores.rs"]
mod scores;

pub use helpers::*;
pub use mods::*;
pub use scores::*;

#[cfg(test)]
use crate::model::states::{Mod, Statistics};
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use indexmap::IndexMap;
#[cfg(test)]
use mods::js_to_string;
#[cfg(test)]
use serde_json::Value;

#[cfg(test)]
#[path = "calculation/root/tests.rs"]
mod tests;
