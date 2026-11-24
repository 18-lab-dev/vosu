//! `vosu-osu`: the bundled osu! game plugin (`docs/v2/plugin-abi.md`,
//! `docs/v2/implementation-plan.md` M1→M4a). P1 provides the crate's
//! identity: its `manifest.toml` (embedded as [`MANIFEST`]), its
//! `settings.schema.json` (embedded as [`SETTINGS_SCHEMA`]), and the D19
//! settings shape ([`settings::OsuSettings`]). P2 adds attach
//! classification/decline/identify ([`classify`]) and the
//! `ConcurrentPlugin` entry point ([`plugin::OsuPlugin`]).
//! [`memory::PlanProcessMemory`] and
//! [`scan::PlanScanBackend`]/[`scan::ScanWaiters`] (P3) are the
//! `ProcessMemory`/`ScanBackend` backends a session's `ProcessHandle`
//! wraps. The session map, reader threads, and lazer offset-resolver
//! lifecycle are later M1 units (P4-P5); this crate is not yet registered
//! in `src/plugins.rs`'s `BUNDLED` array (unit C3).

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!("the test-harness feature is forbidden in release builds");

pub mod classify;
pub mod memory;
pub mod plugin;
pub mod scan;
pub mod settings;
#[cfg(test)]
mod test_support;

vosu_plugin_api::export_plugin!(bundled concurrent plugin::OsuPlugin);

/// The plugin's `manifest.toml`, embedded verbatim (`docs/v2/plugin-abi.md`
/// §8). Parse with `vosu_plugin_api::PluginManifest::parse`.
pub const MANIFEST: &str = include_str!("../manifest.toml");

/// The plugin's `settings.schema.json`, embedded verbatim (§6.9, D19): the
/// five keys bridge B2 served out of `vosu.toml` until this file replaced
/// it (plan P0).
pub const SETTINGS_SCHEMA: &str = include_str!("../settings.schema.json");
