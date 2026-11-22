//! Source-compatible calculation and conversion utilities.
//!
//! These helpers mirror the upstream tosu source paths
//! `packages/tosu/src/utils/{calculators,osuMods,osuMods.types,hitResult,converters}.ts`.
//! Performance engines intentionally live elsewhere. Shared JS-compatible
//! rounding, mods, and grade/legacy-hit helpers live in `crate::model` so
//! that readers (`crate::memory`) do not depend on calculation (they only
//! need the model); the live calculation backend used by the state reducer
//! lives in `crate::state::state_backend` so that `state` depends on
//! `calculation`, not the other way around.

pub mod beatmap;
pub mod engine;
pub mod progressive;
