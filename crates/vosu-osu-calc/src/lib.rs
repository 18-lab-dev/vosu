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
//! Shared JS-compatible rounding, mods, and grade/legacy-hit helpers live in
//! `vosu_osu_model` so the reader does not depend on calculation. The pure
//! chart engine lives here; the state reducer owns its orchestration backend.

pub mod beatmap;
pub mod engine;
pub mod live_beatmap;
pub mod progressive;
