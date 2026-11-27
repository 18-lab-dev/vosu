//! Host-side settings (`docs/v2/plugin-abi.md` §6.9, D19, A8.1, A8.3): the
//! [`SettingsStorage`] trait core owns, permanent per-plugin persistence
//! ([`PluginSettingsStore`]), [`PluginSettingsService`], and A8.1/A8.3
//! idempotent legacy-override import ([`ImportOutcome`]). The
//! `settings.schema.json` document itself is the SDK's
//! `vosu_plugin_api::manifest::settings::SettingsSchema`.
//!
//! §6.9 defines no plugin-initiated settings "read"/"write" wire opcode:
//! the plugin's initial effective settings arrive in `create`'s own
//! `init.settings` (no round trip needed), and every later change arrives
//! as `VOSU_OP_SETTINGS_CHANGED` (host -> plugin only). "Read" and "write"
//! here are therefore this module's own Rust API — `effective_settings`
//! (read, for `init.settings`), `dashboard_view` / `submit` (for the
//! dashboard plugin settings routes), `apply`, and `import_missing_overrides`
//! (for idempotent legacy migration).

use serde_json::{Map, Value};
use vosu_plugin_api::Message;
use vosu_plugin_api::manifest::settings::{SettingsSchema, SettingsSchemaError};

mod import;
mod service;
mod store;

#[cfg(test)]
mod tests;

pub use import::ImportOutcome;
pub use service::{PluginSettingsService, SettingsDeliveryTransform};
pub use store::{MAX_FILE_BYTES, PluginSettingsStore};

/// Every way settings storage, a `settings.schema.json` document or a
/// submitted value fails.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SettingsError {
    #[error(transparent)]
    Schema(#[from] SettingsSchemaError),
    #[error("{0}")]
    Storage(String),
}

/// Core-owned settings storage for one plugin id (§6.9: "core owns
/// storage"). [`PluginSettingsStore`] is the permanent per-plugin backing
/// store for every settings-bearing plugin, including `vosu.osu` and
/// `vosu.overlayhost`.
pub trait SettingsStorage: Send + Sync {
    /// The stored override values for `plugin_id` (may be a subset of the
    /// schema, or empty: unset keys fall back to the schema's own
    /// defaults in [`SettingsSchema::merge`]).
    fn load(&self, plugin_id: &str) -> Result<Map<String, Value>, SettingsError>;

    /// Persists `values` (already validated against the schema) as
    /// `plugin_id`'s stored overrides.
    fn store(&self, plugin_id: &str, values: &Map<String, Value>) -> Result<(), SettingsError>;
}

/// Where callers resolve a plugin id's current `settings.schema.json` from:
/// the live (or still-`Creating`) plugin instance's own [`SettingsSchema`]
/// (`registry::LoadedPlugin::settings_schema`), returning `None` when no
/// instance declares a schema.
pub trait SettingsSchemaSource: Send + Sync {
    fn schema_for(&self, plugin_id: &str) -> Option<SettingsSchema>;
}

/// Delivers a `VOSU_OP_SETTINGS_CHANGED` this layer built back to every
/// live instance of `plugin_id`: the host-service modules in
/// `crate::plugin` have no instance registry of their own (same reasoning
/// as `timers::TimerDelivery`), so a plugin-id-scoped broadcast like this
/// is this one narrow callback instead.
pub trait SettingsChangeDelivery: Send + Sync {
    fn settings_changed(&self, plugin_id: &str, message: Message);
}
