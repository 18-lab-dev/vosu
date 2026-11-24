//! Compile-time embedded, source-pinned daemon assets.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/server/"]
pub struct ServerAssets;

/// Source-pinned offsets template used by process-memory tests and the live
/// reader benchmark. Production offset loading and the embedded generator
/// bundle are owned by `vosu-osu-reader`.
#[derive(RustEmbed)]
#[folder = "assets/"]
#[include = "offsets.json"]
pub struct DaemonAssets;
