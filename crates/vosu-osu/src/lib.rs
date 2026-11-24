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
//! wraps. P4 adds the session map ([`session::OsuSession`]) and this
//! plugin's own 2-worker Tokio runtime (DG): every identified session gets
//! a `ProcessHandle`, and a lazer session's offset resolver is started on
//! that runtime (DH: cached under `data_dir`), never inline on a dispatch
//! thread. P5 owns the B1 publication/control bridge and session reader threads.
//! M2 adds per-session
//! state-bus fan-out and the union of legacy and bus sampling demand. Production
//! registration and its bundled namespace schema live in `src/plugins.rs`.

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!("the test-harness feature is forbidden in release builds");

pub mod actions;
pub mod bridge_b1;
pub mod capture;
pub mod classify;
pub mod demand;
pub mod memory;
pub mod plugin;
pub mod project;
pub mod publish;
pub mod resources;
pub mod scan;
pub mod session;
pub mod settings;
#[cfg(test)]
mod test_alloc;
#[cfg(test)]
mod test_support;
#[cfg(all(feature = "test-harness", not(test)))]
pub use session::test_support;

vosu_plugin_api::export_plugin!(bundled concurrent plugin::OsuPlugin);

/// The plugin's `manifest.toml`, embedded verbatim (`docs/v2/plugin-abi.md`
/// §8). Parse with `vosu_plugin_api::PluginManifest::parse`.
pub const MANIFEST: &str = include_str!("../manifest.toml");

/// The plugin's `settings.schema.json`, embedded verbatim (§6.9, D19): the
/// five keys bridge B2 served out of `vosu.toml` until this file replaced
/// it (plan P0).
pub const SETTINGS_SCHEMA: &str = include_str!("../settings.schema.json");

/// The bundled namespace schema; its version matches the manifest's provider.
pub const OSU_NAMESPACE_SCHEMA: &str =
    include_str!("../../../docs/v2/schemas/vosu.osu.schema.json");
