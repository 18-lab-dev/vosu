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
//! thread. The plugin owns state-bus publication and session reader threads.
//! Per-session publication and selector-driven sampling stay plugin-owned;
//! bundled registration and its namespace schema live in `src/plugins.rs`.
#![cfg_attr(
    not(feature = "test-harness"),
    doc = r"
The daemon's parity ingress is absent even when only `bench-harness` is enabled:

```compile_fail
use vosu_osu::fixtures::FixtureOrProducer;
```
"
)]

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!("the test-harness feature is forbidden in release builds");

pub mod actions;
#[cfg(feature = "bench-harness")]
#[doc(hidden)]
pub mod bench_capture;
pub mod capture;
pub mod classify;
pub mod demand;
#[cfg(feature = "bench-harness")]
pub mod fixtures;
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

#[cfg(all(feature = "test-harness", debug_assertions))]
vosu_plugin_internal::export_plugin!(bundled concurrent fixtures::FixtureOrProducer);

#[cfg(not(all(feature = "test-harness", debug_assertions)))]
vosu_plugin_internal::export_plugin!(bundled concurrent plugin::OsuPlugin);

/// The plugin's `manifest.toml`, embedded verbatim (`docs/v2/plugin-abi.md`
/// §8). Parse with `vosu_plugin_api::manifest::PackageManifest::parse`.
pub const MANIFEST: &str = include_str!("../manifest.toml");

/// The plugin's `settings.schema.json`, embedded verbatim (§6.9, D19): the
/// five keys bridge B2 served out of `vosu.toml` until this file replaced
/// it (plan P0).
pub const SETTINGS_SCHEMA: &str = include_str!("../settings.schema.json");

/// The bundled namespace schema; its version matches the manifest's provider.
pub const OSU_NAMESPACE_SCHEMA: &str =
    include_str!("../../../docs/v2/schemas/vosu.osu.schema.json");

/// Bundled beatmap-import action schemas, resolved without filesystem access.
pub const ACTION_SCHEMAS: &[(&str, &str)] = &[
    (
        "schemas/beatmap-import.args.json",
        include_str!("../schemas/beatmap-import.args.json"),
    ),
    (
        "schemas/beatmap-import.result.json",
        include_str!("../schemas/beatmap-import.result.json"),
    ),
];
