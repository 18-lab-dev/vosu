//! Host-side settings (`docs/v2/plugin-abi.md` §6.9, D19): the
//! `settings.json`/`settings.schema.json` shape (`docs/tools/protocol.md`,
//! the overlay/tool settings format §6.9 reuses verbatim), schema
//! validation, the `SettingsStorage` trait core owns, and **bridge B2**
//! (`docs/v2/implementation-plan.md` D19, M1→M8): serves `vosu.osu`'s five
//! osu!-specific keys from the existing `vosu.toml` values until the M8
//! migration moves them into real per-plugin settings storage and removes
//! them from `vosu.toml`. Delete this bridge whole in M8 — do not add a
//! second plugin id to it.
//!
//! §6.9 defines no plugin-initiated settings "read"/"write" wire opcode:
//! the plugin's initial effective settings arrive in `create`'s own
//! `init.settings` (no round trip needed), and every later change arrives
//! as `VOSU_OP_SETTINGS_CHANGED` (host -> plugin only). "Read" and "write"
//! here are therefore this module's own Rust API — `effective_settings`
//! (read, for `init.settings`) and `apply` (write, for the dashboard
//! editing UI's eventual caller, validating and storing before building
//! the `SETTINGS_CHANGED` message to broadcast) — not a decoded ABI
//! opcode; recorded as a spec gap in this phase's handoff notes.

use std::collections::HashSet;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value};
use vosu_plugin_api::{CborValue, Message, SettingsChanged};

use crate::config::ConfigStore;

/// One `settings.json`/`settings.schema.json` entry
/// (`docs/tools/protocol.md`, the overlay settings format): `uniqueID`,
/// `type`, and the entry's own default `value`. Other overlay fields
/// (`title`, `description`, `options`, `uniqueCheck`) are display-only for
/// this phase's validation and storage concerns, so they are accepted but
/// not modeled here.
#[derive(Debug, Clone, Deserialize)]
struct SettingEntry {
    #[serde(rename = "uniqueID")]
    unique_id: String,
    #[serde(rename = "type")]
    kind: SettingKind,
    #[serde(default)]
    value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SettingKind {
    Text,
    Color,
    Number,
    Checkbox,
    Button,
    Options,
    Commands,
    Textarea,
    Password,
    Header,
}

impl SettingKind {
    /// `button`/`header` are layout-only entries: the overlay settings
    /// format has no value for them, so they never appear in an effective
    /// settings object.
    fn is_value_bearing(self) -> bool {
        !matches!(self, Self::Button | Self::Header)
    }

    /// Whether `value`'s JSON type matches what this entry's `type`
    /// declares (§6.9: core validates submitted settings against the
    /// schema before storing them).
    fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Checkbox => value.is_boolean(),
            Self::Number => value.is_number(),
            Self::Text | Self::Color | Self::Textarea | Self::Password | Self::Options => {
                value.is_string()
            }
            Self::Commands => value.is_array(),
            Self::Button | Self::Header => true,
        }
    }
}

/// Every way a `settings.schema.json`/submitted value fails to parse or
/// validate.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SettingsError {
    #[error("settings_schema is not valid JSON: {0}")]
    Json(String),
    #[error("settings_schema must be a JSON array of setting entries")]
    NotArray,
    #[error("settings_schema entry {0:?} reuses an earlier entry's uniqueID")]
    DuplicateId(String),
    #[error("setting {0:?} is not declared in this plugin's settings_schema")]
    UnknownKey(String),
    #[error("setting {0:?}'s value does not match its declared type")]
    TypeMismatch(String),
    #[error("plugin {0:?} has no settings storage bridge")]
    NotBridged(String),
}

/// A parsed, validated `settings.schema.json` (§6.9, §8's `settings_schema`
/// manifest field).
#[derive(Debug, Clone)]
pub struct SettingsSchema {
    entries: Vec<SettingEntry>,
}

impl SettingsSchema {
    /// Parses and validates `text` (the `settings.schema.json` file's
    /// contents): an array of entries, every `uniqueID` distinct.
    pub fn parse(text: &str) -> Result<Self, SettingsError> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| SettingsError::Json(e.to_string()))?;
        let Value::Array(raw) = value else {
            return Err(SettingsError::NotArray);
        };
        let mut entries = Vec::with_capacity(raw.len());
        let mut seen = HashSet::new();
        for item in raw {
            let entry: SettingEntry =
                serde_json::from_value(item).map_err(|e| SettingsError::Json(e.to_string()))?;
            if !seen.insert(entry.unique_id.clone()) {
                return Err(SettingsError::DuplicateId(entry.unique_id));
            }
            entries.push(entry);
        }
        Ok(Self { entries })
    }

    /// Every value-bearing entry's own declared default.
    pub fn defaults(&self) -> Map<String, Value> {
        self.entries
            .iter()
            .filter(|entry| entry.kind.is_value_bearing())
            .map(|entry| (entry.unique_id.clone(), entry.value.clone()))
            .collect()
    }

    /// Validates `overrides` against this schema and returns the merged
    /// effective settings object: defaults with `overrides` applied.
    /// Every key in `overrides` must name a value-bearing entry and match
    /// its declared type, matching §6.9's "core owns storage" validation.
    pub fn merge(
        &self,
        overrides: &Map<String, Value>,
    ) -> Result<Map<String, Value>, SettingsError> {
        let mut effective = self.defaults();
        for (key, value) in overrides {
            let entry = self
                .entries
                .iter()
                .find(|entry| entry.kind.is_value_bearing() && &entry.unique_id == key)
                .ok_or_else(|| SettingsError::UnknownKey(key.clone()))?;
            if !entry.kind.accepts(value) {
                return Err(SettingsError::TypeMismatch(key.clone()));
            }
            effective.insert(key.clone(), value.clone());
        }
        Ok(effective)
    }
}

/// Core-owned settings storage for one plugin id (§6.9: "core owns
/// storage"). A real implementation lands in M8; until then, every
/// `settings_schema`-declaring plugin needs a `SettingsStorage` impl that
/// serves it — for M1 that is only [`ConfigSettingsBridgeB2`].
pub trait SettingsStorage: Send + Sync {
    /// The stored override values for `plugin_id` (may be a subset of the
    /// schema, or empty: unset keys fall back to the schema's own
    /// defaults in [`SettingsSchema::merge`]).
    fn load(&self, plugin_id: &str) -> Result<Map<String, Value>, SettingsError>;

    /// Persists `values` (already validated against the schema) as
    /// `plugin_id`'s stored overrides.
    fn store(&self, plugin_id: &str, values: &Map<String, Value>) -> Result<(), SettingsError>;
}

/// The one plugin id bridge B2 serves (D19): `CALCULATE_PP`, `POLL_RATE`,
/// `PRECISE_DATA_POLL_RATE`, `SHOW_MP_COMMANDS`, and
/// `READ_MANIA_SCROLL_SPEED` become its `vosu.osu` settings.
pub const BRIDGE_B2_PLUGIN_ID: &str = "vosu.osu";

/// **Interim bridge B2** (`docs/v2/implementation-plan.md` D19, M1→M8).
/// `vosu-osu` (the bundled osu! plugin, built in a parallel M1 slice) does
/// not yet ship its own `settings.schema.json` in this tree, so
/// [`ConfigSettingsBridgeB2::schema`] is the schema this bridge itself
/// enforces for `vosu.osu` until that plugin's real file replaces it and
/// this whole bridge is deleted in M8.
pub struct ConfigSettingsBridgeB2 {
    config: Arc<ConfigStore>,
}

impl ConfigSettingsBridgeB2 {
    pub fn new(config: Arc<ConfigStore>) -> Self {
        Self { config }
    }

    /// The five `vosu.toml` keys D19 moves into `vosu.osu` settings,
    /// mirroring `crates/vosu-core/src/config/schema.rs`'s own defaults so
    /// an unset `vosu.toml` and an unset plugin setting agree.
    pub fn schema() -> SettingsSchema {
        SettingsSchema::parse(
            r#"[
                {"uniqueID":"CALCULATE_PP","type":"checkbox","title":"Calculate pp","value":true},
                {"uniqueID":"POLL_RATE","type":"number","title":"Poll rate","value":150.0},
                {"uniqueID":"PRECISE_DATA_POLL_RATE","type":"number","title":"Precise data poll rate","value":10.0},
                {"uniqueID":"SHOW_MP_COMMANDS","type":"checkbox","title":"Show multiplayer commands","value":false},
                {"uniqueID":"READ_MANIA_SCROLL_SPEED","type":"checkbox","title":"Read mania scroll speed","value":true}
            ]"#,
        )
        .expect("the bridge's own embedded schema is valid JSON")
    }
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

fn json_to_cbor(value: Value) -> CborValue {
    match value {
        Value::Null => CborValue::Null,
        Value::Bool(b) => CborValue::Bool(b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                CborValue::Integer(i.into())
            } else if let Some(u) = n.as_u64() {
                CborValue::Integer(u.into())
            } else {
                CborValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => CborValue::Text(s),
        Value::Array(items) => CborValue::Array(items.into_iter().map(json_to_cbor).collect()),
        Value::Object(map) => CborValue::Map(
            map.into_iter()
                .map(|(key, value)| (CborValue::Text(key), json_to_cbor(value)))
                .collect(),
        ),
    }
}

fn json_map_to_cbor(map: Map<String, Value>) -> CborValue {
    CborValue::Map(
        map.into_iter()
            .map(|(key, value)| (CborValue::Text(key), json_to_cbor(value)))
            .collect(),
    )
}

/// Builds a plugin instance's effective settings object (§6.9's
/// `init.settings`) and, later, validated `SETTINGS_CHANGED` broadcasts,
/// over one shared [`SettingsStorage`].
pub struct PluginSettingsService {
    storage: Arc<dyn SettingsStorage>,
}

impl PluginSettingsService {
    pub fn new(storage: Arc<dyn SettingsStorage>) -> Self {
        Self { storage }
    }

    /// The CBOR value for `create`'s `init.settings` (§6.9: "no round trip
    /// is needed for the initial value"). A plugin with no
    /// `settings_schema` gets an empty map.
    pub fn effective_settings(
        &self,
        plugin_id: &str,
        schema: Option<&SettingsSchema>,
    ) -> Result<CborValue, SettingsError> {
        let Some(schema) = schema else {
            return Ok(CborValue::Map(Vec::new()));
        };
        let stored = self.storage.load(plugin_id)?;
        let effective = schema.merge(&stored)?;
        Ok(json_map_to_cbor(effective))
    }

    /// Validates `patch` against `schema`, stores the merged effective
    /// object, and returns the `VOSU_OP_SETTINGS_CHANGED` message to
    /// dispatch to every one of `plugin_id`'s instances (§6.9: "always the
    /// complete effective settings object").
    pub fn apply(
        &self,
        plugin_id: &str,
        schema: &SettingsSchema,
        patch: &Map<String, Value>,
    ) -> Result<Message, SettingsError> {
        let mut merged = self.storage.load(plugin_id)?;
        for (key, value) in patch {
            merged.insert(key.clone(), value.clone());
        }
        let effective = schema.merge(&merged)?;
        self.storage.store(plugin_id, &effective)?;
        Ok(Message::SettingsChanged(SettingsChanged {
            values: json_map_to_cbor(effective),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> SettingsSchema {
        SettingsSchema::parse(
            r#"[
                {"uniqueID":"ENABLED","type":"checkbox","value":true},
                {"uniqueID":"LABEL","type":"text","value":"hello"},
                {"uniqueID":"RATE","type":"number","value":150.0},
                {"uniqueID":"NOTE","type":"header"}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn defaults_exclude_header_and_button_entries() {
        let defaults = schema().defaults();
        assert_eq!(defaults.len(), 3);
        assert!(!defaults.contains_key("NOTE"));
    }

    #[test]
    fn merge_applies_overrides_on_top_of_defaults() {
        let mut overrides = Map::new();
        overrides.insert("RATE".into(), Value::from(42.0));
        let effective = schema().merge(&overrides).unwrap();
        assert_eq!(effective.get("RATE"), Some(&Value::from(42.0)));
        assert_eq!(effective.get("ENABLED"), Some(&Value::Bool(true)));
    }

    #[test]
    fn merge_rejects_unknown_keys() {
        let mut overrides = Map::new();
        overrides.insert("NOPE".into(), Value::from(1));
        assert_eq!(
            schema().merge(&overrides).unwrap_err(),
            SettingsError::UnknownKey("NOPE".into())
        );
    }

    #[test]
    fn merge_rejects_type_mismatches() {
        let mut overrides = Map::new();
        overrides.insert("ENABLED".into(), Value::from("not a bool"));
        assert_eq!(
            schema().merge(&overrides).unwrap_err(),
            SettingsError::TypeMismatch("ENABLED".into())
        );
    }

    #[test]
    fn parse_rejects_duplicate_unique_ids() {
        let text = r#"[{"uniqueID":"A","type":"checkbox","value":true},{"uniqueID":"A","type":"text","value":"x"}]"#;
        assert_eq!(
            SettingsSchema::parse(text).unwrap_err(),
            SettingsError::DuplicateId("A".into())
        );
    }

    #[test]
    fn parse_rejects_a_non_array_document() {
        assert_eq!(
            SettingsSchema::parse(r#"{"not": "an array"}"#).unwrap_err(),
            SettingsError::NotArray
        );
    }

    #[test]
    fn bridge_b2_schema_matches_config_defaults() {
        let defaults = ConfigSettingsBridgeB2::schema().defaults();
        assert_eq!(defaults.get("CALCULATE_PP"), Some(&Value::Bool(true)));
        assert_eq!(defaults.get("POLL_RATE"), Some(&Value::from(150.0)));
        assert_eq!(
            defaults.get("PRECISE_DATA_POLL_RATE"),
            Some(&Value::from(10.0))
        );
        assert_eq!(defaults.get("SHOW_MP_COMMANDS"), Some(&Value::Bool(false)));
        assert_eq!(
            defaults.get("READ_MANIA_SCROLL_SPEED"),
            Some(&Value::Bool(true))
        );
    }

    async fn config_store() -> Arc<ConfigStore> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vosu.toml");
        let store = ConfigStore::initialize(&path).await.unwrap();
        std::mem::forget(dir);
        Arc::new(store)
    }

    #[tokio::test]
    async fn bridge_b2_loads_current_config_values() {
        let config = config_store().await;
        config.update(|c| {
            c.poll_rate = 200.0;
            c.calculate_pp = false;
        });
        let bridge = ConfigSettingsBridgeB2::new(config);
        let values = bridge.load(BRIDGE_B2_PLUGIN_ID).unwrap();
        assert_eq!(values.get("POLL_RATE"), Some(&Value::from(200.0)));
        assert_eq!(values.get("CALCULATE_PP"), Some(&Value::Bool(false)));
    }

    #[tokio::test]
    async fn bridge_b2_store_writes_through_to_the_config() {
        let config = config_store().await;
        let bridge = ConfigSettingsBridgeB2::new(Arc::clone(&config));
        let mut values = Map::new();
        values.insert("POLL_RATE".into(), Value::from(300.0));
        values.insert("SHOW_MP_COMMANDS".into(), Value::Bool(true));
        bridge.store(BRIDGE_B2_PLUGIN_ID, &values).unwrap();
        assert_eq!(config.current().poll_rate, 300.0);
        assert!(config.current().show_mp_commands);
    }

    #[tokio::test]
    async fn bridge_b2_refuses_an_unbridged_plugin_id() {
        let config = config_store().await;
        let bridge = ConfigSettingsBridgeB2::new(config);
        assert_eq!(
            bridge.load("some.other.plugin").unwrap_err(),
            SettingsError::NotBridged("some.other.plugin".into())
        );
    }

    #[tokio::test]
    async fn apply_validates_stores_and_builds_settings_changed() {
        let config = config_store().await;
        let storage: Arc<dyn SettingsStorage> = Arc::new(ConfigSettingsBridgeB2::new(config));
        let service = PluginSettingsService::new(storage);
        let mut patch = Map::new();
        patch.insert("POLL_RATE".into(), Value::from(250.0));
        let message = service
            .apply(
                BRIDGE_B2_PLUGIN_ID,
                &ConfigSettingsBridgeB2::schema(),
                &patch,
            )
            .unwrap();
        let Message::SettingsChanged(changed) = message else {
            panic!("expected SettingsChanged");
        };
        let CborValue::Map(entries) = changed.values else {
            panic!("expected a CBOR map");
        };
        assert!(entries.iter().any(|(key, value)| {
            *key == CborValue::Text("POLL_RATE".into()) && *value == CborValue::Float(250.0)
        }));
    }

    #[tokio::test]
    async fn effective_settings_is_an_empty_map_without_a_schema() {
        let config = config_store().await;
        let storage: Arc<dyn SettingsStorage> = Arc::new(ConfigSettingsBridgeB2::new(config));
        let service = PluginSettingsService::new(storage);
        let value = service
            .effective_settings(BRIDGE_B2_PLUGIN_ID, None)
            .unwrap();
        assert_eq!(value, CborValue::Map(Vec::new()));
    }
}
