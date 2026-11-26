//! Compile-time embedded, source-pinned daemon assets.

use rust_embed::RustEmbed;

/// The shared `/assets/` files this executable still serves itself on both
/// listeners. The dashboard set and its page template are embedded once by
/// `vosu-dashboard` (dashboard listener only), and `report.html` only by
/// core's report generation (A7 asset inventory).
#[derive(RustEmbed)]
#[folder = "assets/server/"]
#[exclude = "homepage*"]
#[exclude = "dashboard-*"]
#[exclude = "update-notice.css"]
#[exclude = "report.html"]
pub struct ServerAssets;

/// Source-pinned offsets template used by process-memory tests and the live
/// reader benchmark. Production offset loading and the embedded generator
/// bundle are owned by `vosu-osu-reader`.
#[derive(RustEmbed)]
#[folder = "assets/"]
#[include = "offsets.json"]
pub struct DaemonAssets;
