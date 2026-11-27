//! Interim **bridge B2** (`docs/v2/implementation-plan.md` D19, M1→M8): serves
//! `vosu.osu`'s five osu!-specific keys from the existing `vosu.toml` values
//! until M8-L2 Unit M0 moves them into [`super::PluginSettingsStore`] and
//! deletes this module whole. Do not add a second plugin id to it.

use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::runtime::Handle;
use tokio::sync::broadcast;
use vosu_plugin_api::{Message, SettingsChanged};

use crate::config::{Config, ConfigChanged, ConfigStore};

use super::{
    PluginSettingsService, SettingsChangeDelivery, SettingsError, SettingsSchemaSource,
    SettingsStorage,
};

/// The one plugin id bridge B2 serves (D19): `CALCULATE_PP`, `POLL_RATE`,
/// `PRECISE_DATA_POLL_RATE`, `SHOW_MP_COMMANDS`, and
/// `READ_MANIA_SCROLL_SPEED` become its `vosu.osu` settings.
pub const BRIDGE_B2_PLUGIN_ID: &str = "vosu.osu";

/// **Interim bridge B2** (`docs/v2/implementation-plan.md` D19, M1→M8),
/// deleted whole in M8. Its own settings storage, not a schema: `vosu.osu`'s
/// real `settings.schema.json` (P1) is what `create_instance`/
/// `watch_config_changes` validate and merge these stored overrides
/// against, resolved through [`SettingsSchemaSource`] from the live
/// `vosu.osu` plugin instance rather than a schema this bridge embeds
/// itself.
pub struct ConfigSettingsBridgeB2 {
    config: Arc<ConfigStore>,
}

impl ConfigSettingsBridgeB2 {
    pub fn new(config: Arc<ConfigStore>) -> Self {
        Self { config }
    }

    /// Spawns a background task watching every `ConfigStore` change (§6.9:
    /// "every later change arrives as `SETTINGS_CHANGED`") — a dashboard
    /// settings-page write, a `vosu.toml` hot reload, or a plain
    /// `ConfigStore::update` call, not only a write through this bridge's
    /// own `store` — and, whenever one actually touches any of its five
    /// keys, rebuilds the effective settings object and hands it to
    /// `delivery` for `BRIDGE_B2_PLUGIN_ID`. Returns the task's
    /// `JoinHandle` so the caller can stop it explicitly
    /// (`ProductionRuntime::shutdown`): this task holds `service`, which
    /// holds `self` (`Arc<ConfigSettingsBridgeB2>` -> `Arc<ConfigStore>`),
    /// and `ConfigStore::shutdown` does not drop its own `changed`
    /// sender, so `RecvError::Closed` never actually arrives in
    /// practice — the task does not stop "on its own".
    pub fn watch_config_changes(
        self: &Arc<Self>,
        runtime: &Handle,
        service: Arc<PluginSettingsService>,
        schema_source: Arc<dyn SettingsSchemaSource>,
        delivery: Arc<dyn SettingsChangeDelivery>,
    ) -> tokio::task::JoinHandle<()> {
        let mut changes = self.config.subscribe();
        runtime.spawn(async move {
            let deliver_current = |values_service: &PluginSettingsService| {
                // No schema resolves until `vosu.osu` is at least
                // registered `Creating` (`PluginHost::schema_for`):
                // skip delivery rather than broadcasting the empty map
                // `effective_settings(.., None)` would otherwise hand
                // back, which would violate §6.9's "always the complete
                // effective settings object" once replayed to `Live`.
                let Some(schema) = schema_source.schema_for(BRIDGE_B2_PLUGIN_ID) else {
                    return;
                };
                if let Ok(values) =
                    values_service.effective_settings(BRIDGE_B2_PLUGIN_ID, Some(&schema))
                {
                    delivery.settings_changed(
                        BRIDGE_B2_PLUGIN_ID,
                        Message::SettingsChanged(SettingsChanged { values }),
                    );
                }
            };
            loop {
                match changes.recv().await {
                    Ok(ConfigChanged { old, new }) => {
                        if bridge_b2_keys_changed(&old, &new) {
                            deliver_current(&service);
                        }
                    }
                    // A lagged receiver has already dropped one or more
                    // change events: whether any of them touched a B2
                    // key is unknowable from here, so resend the current
                    // effective settings unconditionally rather than
                    // risk the plugin silently keeping a stale value.
                    Err(broadcast::error::RecvError::Lagged(_)) => deliver_current(&service),
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    }
}

/// Whether any of bridge B2's five `vosu.toml` keys actually changed
/// value between `old` and `new`.
fn bridge_b2_keys_changed(old: &Config, new: &Config) -> bool {
    old.calculate_pp != new.calculate_pp
        || old.poll_rate != new.poll_rate
        || old.precise_data_poll_rate != new.precise_data_poll_rate
        || old.show_mp_commands != new.show_mp_commands
        || old.read_mania_scroll_speed != new.read_mania_scroll_speed
}

fn json_number(value: f64) -> Value {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

impl SettingsStorage for ConfigSettingsBridgeB2 {
    fn load(&self, plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
        if plugin_id != BRIDGE_B2_PLUGIN_ID {
            return Err(SettingsError::NotBridged(plugin_id.to_owned()));
        }
        let config = self.config.current();
        let mut values = Map::new();
        values.insert("CALCULATE_PP".into(), Value::Bool(config.calculate_pp));
        values.insert("POLL_RATE".into(), json_number(config.poll_rate));
        values.insert(
            "PRECISE_DATA_POLL_RATE".into(),
            json_number(config.precise_data_poll_rate),
        );
        values.insert(
            "SHOW_MP_COMMANDS".into(),
            Value::Bool(config.show_mp_commands),
        );
        values.insert(
            "READ_MANIA_SCROLL_SPEED".into(),
            Value::Bool(config.read_mania_scroll_speed),
        );
        Ok(values)
    }

    fn store(&self, plugin_id: &str, values: &Map<String, Value>) -> Result<(), SettingsError> {
        if plugin_id != BRIDGE_B2_PLUGIN_ID {
            return Err(SettingsError::NotBridged(plugin_id.to_owned()));
        }
        self.config.update(|config| {
            if let Some(value) = values.get("CALCULATE_PP").and_then(Value::as_bool) {
                config.calculate_pp = value;
            }
            if let Some(value) = values.get("POLL_RATE").and_then(Value::as_f64) {
                config.poll_rate = value;
            }
            if let Some(value) = values.get("PRECISE_DATA_POLL_RATE").and_then(Value::as_f64) {
                config.precise_data_poll_rate = value;
            }
            if let Some(value) = values.get("SHOW_MP_COMMANDS").and_then(Value::as_bool) {
                config.show_mp_commands = value;
            }
            if let Some(value) = values
                .get("READ_MANIA_SCROLL_SPEED")
                .and_then(Value::as_bool)
            {
                config.read_mania_scroll_speed = value;
            }
        });
        Ok(())
    }
}
