//! Compile-time embedded, source-pinned daemon assets. The root serves no
//! `assets/server/` file: the dashboard set belongs to `vosu-dashboard`, the
//! design-system set to `vosu-tool-host` and `report.html` to core's report
//! generation (A7 asset inventory).

use rust_embed::RustEmbed;

/// Source-pinned offsets template used by process-memory tests and the live
/// reader benchmark. Production offset loading and the embedded generator
/// bundle are owned by `vosu-osu-reader`.
#[derive(RustEmbed)]
#[folder = "assets/"]
#[include = "offsets.json"]
pub struct DaemonAssets;
