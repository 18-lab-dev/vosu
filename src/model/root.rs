//! Source-compatible calculation and conversion utilities shared by readers,
//! the calculation engines, and state reducers.
//!
//! These helpers mirror the upstream tosu source paths
//! `packages/tosu/src/utils/{calculators,osuMods,osuMods.types,hitResult,converters}.ts`.
//! They live under `model`, not `calculation`, so that `memory` (readers) can
//! depend on them without depending on `calculation` (module-boundaries.txt
//! would otherwise record a `memory -> calculation` cycle through
//! `calculation -> state -> memory`).

#[path = "root/helpers.rs"]
mod helpers;
#[path = "root/mods.rs"]
mod mods;
#[path = "root/scores.rs"]
mod scores;

pub use helpers::*;
pub use mods::*;
pub use scores::*;

#[cfg(test)]
use super::states::{Mod, Statistics};
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use indexmap::IndexMap;
#[cfg(test)]
use mods::js_to_string;
#[cfg(test)]
use serde_json::Value;

#[cfg(test)]
#[path = "root/tests.rs"]
mod tests;
