//! `vosu-osu`: the bundled osu! game plugin (`docs/v2/plugin-abi.md`,
//! `docs/v2/implementation-plan.md` M1→M4a). This unit (P1) provides the
//! crate's identity: its `manifest.toml` (embedded as [`MANIFEST`]), its
//! `settings.schema.json` (embedded as [`SETTINGS_SCHEMA`]), and the D19
//! settings shape ([`settings::OsuSettings`]). The `ConcurrentPlugin`
//! entry point, matcher wiring, and session runtime are later M1 units
//! (P2-P5); this crate is not yet registered in `src/plugins.rs`'s
//! `BUNDLED` array (unit C3).

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!("the test-harness feature is forbidden in release builds");

pub mod settings;

/// The plugin's `manifest.toml`, embedded verbatim (`docs/v2/plugin-abi.md`
/// §8). Parse with `vosu_plugin_api::PluginManifest::parse`.
pub const MANIFEST: &str = include_str!("../manifest.toml");

/// The plugin's `settings.schema.json`, embedded verbatim (§6.9, D19): the
/// five keys bridge B2 served out of `vosu.toml` until this file replaced
/// it (plan P0).
pub const SETTINGS_SCHEMA: &str = include_str!("../settings.schema.json");
