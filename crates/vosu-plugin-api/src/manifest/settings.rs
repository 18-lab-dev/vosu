//! The `settings.schema.json` document a plugin's `settings_schema` names
//! (`docs/v2/plugin-abi.md` §6.9; the overlay/tool settings format of
//! `docs/tools/protocol.md`, which §6.9 reuses verbatim), and validation of
//! submitted values against it.
//!
//! This is the one parser: vosu's catalog install and plugin load paths and
//! `vosu-package-check` all accept a settings schema only through
//! [`SettingsSchema::parse`], so the checker cannot pass a schema the host
//! refuses. Storage and delivery of settings stay in the host.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Map, Value};

/// Upper bound on a `settings.schema.json` document, the same bound as a
/// namespace schema.
pub const MAX_SETTINGS_SCHEMA_BYTES: usize = super::schema::MAX_BYTES;

/// One `settings.json`/`settings.schema.json` entry
/// (`docs/tools/protocol.md`, the overlay settings format): `uniqueID`,
/// `type`, and the entry's own default `value`. Other overlay fields
/// (`title`, `description`, `options`, `uniqueCheck`) are display-only for
/// validation and storage, so they are accepted but not modeled here.
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

/// Every way a `settings.schema.json` document or a submitted value fails
/// to parse or validate.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SettingsSchemaError {
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
}

/// A parsed, validated `settings.schema.json` (§6.9, §8's `settings_schema`
/// manifest field).
#[derive(Debug, Clone)]
pub struct SettingsSchema {
    entries: Vec<SettingEntry>,
    /// Each entry object exactly as written, display fields included, for a
    /// settings editor (`document`).
    document: Vec<Value>,
}

impl SettingsSchema {
    /// Parses and validates `text` (the `settings.schema.json` file's
    /// contents): an array of entries, every `uniqueID` distinct.
    pub fn parse(text: &str) -> Result<Self, SettingsSchemaError> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| SettingsSchemaError::Json(e.to_string()))?;
        let Value::Array(raw) = value else {
            return Err(SettingsSchemaError::NotArray);
        };
        let mut entries = Vec::with_capacity(raw.len());
        let mut seen = HashSet::new();
        for item in &raw {
            let entry: SettingEntry = serde_json::from_value(item.clone())
                .map_err(|e| SettingsSchemaError::Json(e.to_string()))?;
            if !seen.insert(entry.unique_id.clone()) {
                return Err(SettingsSchemaError::DuplicateId(entry.unique_id));
            }
            entries.push(entry);
        }
        Ok(Self {
            entries,
            document: raw,
        })
    }

    /// The validated entries exactly as the document wrote them, including
    /// the display-only fields (`title`, `description`, `options`, …) a
    /// settings editor renders. A `password` entry's own `value` is its
    /// default secret: an editor must not show it.
    pub fn document(&self) -> &[Value] {
        &self.document
    }

    /// The `uniqueID` of every `password` entry (§6.9: write-only values).
    pub fn password_keys(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(|entry| entry.kind == SettingKind::Password)
            .map(|entry| entry.unique_id.as_str())
    }

    /// Whether `key` names a `password` entry.
    pub fn is_password(&self, key: &str) -> bool {
        self.password_keys().any(|password| password == key)
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
    ) -> Result<Map<String, Value>, SettingsSchemaError> {
        let mut effective = self.defaults();
        for (key, value) in overrides {
            let entry = self
                .entries
                .iter()
                .find(|entry| entry.kind.is_value_bearing() && &entry.unique_id == key)
                .ok_or_else(|| SettingsSchemaError::UnknownKey(key.clone()))?;
            if !entry.kind.accepts(value) {
                return Err(SettingsSchemaError::TypeMismatch(key.clone()));
            }
            effective.insert(key.clone(), value.clone());
        }
        Ok(effective)
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
            SettingsSchemaError::UnknownKey("NOPE".into())
        );
    }

    #[test]
    fn merge_rejects_type_mismatches() {
        let mut overrides = Map::new();
        overrides.insert("ENABLED".into(), Value::from("not a bool"));
        assert_eq!(
            schema().merge(&overrides).unwrap_err(),
            SettingsSchemaError::TypeMismatch("ENABLED".into())
        );
    }

    #[test]
    fn parse_rejects_duplicate_unique_ids() {
        let text = r#"[{"uniqueID":"A","type":"checkbox","value":true},{"uniqueID":"A","type":"text","value":"x"}]"#;
        assert_eq!(
            SettingsSchema::parse(text).unwrap_err(),
            SettingsSchemaError::DuplicateId("A".into())
        );
    }

    #[test]
    fn parse_rejects_a_non_array_document() {
        assert_eq!(
            SettingsSchema::parse(r#"{"not": "an array"}"#).unwrap_err(),
            SettingsSchemaError::NotArray
        );
    }

    #[test]
    fn parse_rejects_unknown_entry_types() {
        assert!(matches!(
            SettingsSchema::parse(r#"[{"uniqueID":"A","type":"slider","value":1}]"#),
            Err(SettingsSchemaError::Json(_))
        ));
    }

    #[test]
    fn document_keeps_display_fields_and_password_keys_are_listed() {
        let schema = SettingsSchema::parse(
            r#"[{"uniqueID":"TOKEN","type":"password","title":"Token","value":"default"},
                {"uniqueID":"LABEL","type":"text","title":"Label","description":"Shown","value":"x"}]"#,
        )
        .unwrap();
        assert_eq!(schema.document().len(), 2);
        assert_eq!(schema.document()[1]["description"], "Shown");
        assert_eq!(schema.password_keys().collect::<Vec<_>>(), ["TOKEN"]);
        assert!(schema.is_password("TOKEN") && !schema.is_password("LABEL"));
    }
}
