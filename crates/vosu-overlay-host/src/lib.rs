//! `vosu.overlayhost` (M7 O0): counter discovery, the community catalog and
//! its cards, download/update/delete, names and aliases, settings and
//! profile storage, `/websocket/commands` command processing and the
//! preview colour scheme.
//!
//! The crate depends only on `vosu-surface`, `vosu-plugin-api`,
//! `vosu-plugin-internal` and `vosu-net` (A7.1). It learns the overlay root
//! only from its `static_folder_path` setting ([`root`], A7.1-OR) and writes
//! under it only through `vosu-net`'s hardened ZIP extraction and
//! `vosu_net::publish` primitives. Key verification, CSRF, folder opening,
//! configuration and credentials stay in core.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "unit-test fixtures write scratch overlay trees and settings files directly"
    )
)]

pub mod cards;
pub mod catalog;
pub mod commands;
mod model;
mod names;
mod plugin;
pub mod presentation;
pub mod preview_scheme;
pub mod root;
mod service;
mod settings;
#[cfg(test)]
mod tests;

pub use model::{
    BrokenOverlay, CompactValues, Counter, CounterSetting, DownloadRequest, LifecycleReply,
    LocalOverlay, SettingInput, SettingType,
};
pub use names::{OverlayName, OverlayNames};
pub use plugin::vosu_plugin_vtable;
pub use service::{
    CounterDownloader, CounterError, CounterService, HardenedCounterDownloader,
    InstalledPackages, ReloadableCounterService, parse_metadata,
};
pub use settings::{
    Profiles, SettingsError, SettingsStore, coerce_checkbox, coerce_number, normalize_settings,
    qualified_counter, split_counter, validate_component,
};

/// The bundled manifest `BUNDLED` registers (catalog-v2.md "Bundled host
/// manifests (A7)").
pub const MANIFEST: &str = include_str!("../manifest.toml");
/// Exactly one `text` setting, `static_folder_path` (A7.1-OR).
pub const SETTINGS_SCHEMA: &str = include_str!("../settings.schema.json");

/// The daemon version: the bundled manifest's `version`, which catalog-v2
/// "Bundled host manifests (A7)" requires to equal the vosu package
/// version. Overlay manifests validate against it, and downloads name it in
/// their user agent.
pub fn host_version() -> &'static semver::Version {
    static VERSION: std::sync::LazyLock<semver::Version> = std::sync::LazyLock::new(|| {
        let manifest: toml::Table = toml::from_str(MANIFEST).expect("bundled manifest is TOML");
        manifest
            .get("version")
            .and_then(toml::Value::as_str)
            .and_then(|version| semver::Version::parse(version).ok())
            .expect("bundled manifest names a semantic version")
    });
    &VERSION
}
