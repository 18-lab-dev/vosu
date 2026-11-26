//! `vosu.dashboard`: the bundled dashboard host (M7 D0). It owns the page
//! shells for `/`, `/settings`, `/available` and `/tools` and the
//! dashboard-only assets, registered on the dashboard listener through
//! `vosu-surface`. Every management operation, configuration value, status
//! read and the CSRF secret stay in core (A7.1); this crate depends on no core,
//! game, adapter or other host crate.
mod assets;
pub mod page;
mod plugin;
pub mod routes;
mod settings;

pub use plugin::vosu_plugin_vtable;

pub const PLUGIN_ID: &str = "vosu.dashboard";
pub const MANIFEST: &str = include_str!("../manifest.toml");

#[cfg(test)]
mod tests;
