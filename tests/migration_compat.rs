//! M8-L2 Unit X0: Adversarial 1.x -> 2.0 migration corpus and native recovery.
//!
//! Exercises the production migration engine (`vosu_core::migration`),
//! `ConfigStore`, `PluginSettingsStore`, `SourceStore`, `ToolRegistry`,
//! `CounterService`, `vosu_sys::platform::Paths`, and `vosu::BUNDLE` end to end.
#![expect(
    clippy::disallowed_methods,
    reason = "integration test fixtures write scratch files and seed temporary roots"
)]

#[path = "migration/config.rs"]
mod config;
#[path = "migration/installed.rs"]
mod installed;
#[path = "migration/layouts.rs"]
mod layouts;
#[path = "migration/recovery.rs"]
mod recovery;
#[path = "migration/settings.rs"]
mod settings;
#[path = "migration/sources.rs"]
mod sources;
#[path = "migration/support.rs"]
mod support;
