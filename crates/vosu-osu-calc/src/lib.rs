#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "unit-test fixtures write scratch beatmap files directly to a temp directory"
    )
)]

//! Source-compatible calculation and conversion utilities.
//!
//! These helpers mirror the upstream tosu source paths
//! `packages/tosu/src/utils/{calculators,osuMods,osuMods.types,hitResult,converters}.ts`.
//! Performance engines intentionally live elsewhere. Shared JS-compatible
//! rounding, mods, and grade/legacy-hit helpers live in `vosu_osu_model` so
//! that the reader does not depend on calculation (it only needs the
//! model); the live calculation backend used by the state reducer lives in
//! the root crate's `state::state_backend` so that `state` depends on
//! `vosu-osu-calc`, not the other way around.

pub mod beatmap;
pub mod engine;
pub mod progressive;
