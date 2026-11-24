//! Compile-time embedded, source-pinned daemon assets.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/server/"]
pub struct ServerAssets;

/// Compile-time, source-pinned `assets/offsets.json`. The lazer offset
/// generator's own embedded bundle lives in `vosu-osu-reader`'s build
/// script; this crate only needs the offsets template directly to build
/// the `LazerOffsetLoader` at `src/runtime/lifecycle.rs`'s instance
/// construction site.
#[derive(RustEmbed)]
#[folder = "assets/"]
#[include = "offsets.json"]
pub struct DaemonAssets;
