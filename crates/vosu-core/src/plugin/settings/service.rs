//! Bridge-neutral [`PluginSettingsService`]: builds `init.settings` (§6.9),
//! serves the dashboard's redacted view (`GET /api/plugins/<id>/settings`),
//! validates and persists submissions/patches/imports, and builds complete
//! `VOSU_OP_SETTINGS_CHANGED` messages only after durable storage succeeds.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value, json};
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_plugin_api::{CborValue, Message, SettingsChanged};

use super::import::{ImportOutcome, plan_import};
use super::{SettingsError, SettingsStorage};

pub(super) fn json_to_cbor(value: Value) -> CborValue {
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

pub(super) fn json_map_to_cbor(map: Map<String, Value>) -> CborValue {
    CborValue::Map(
        map.into_iter()
            .map(|(key, value)| (CborValue::Text(key), json_to_cbor(value)))
            .collect(),
    )
}

/// The stored overrides `schema` still accepts, each key checked on its
/// own: a value an earlier package version stored for a key this schema
/// no longer declares, or now declares with another type, falls back to
/// the default instead of invalidating every other value.
pub(super) fn valid_overrides(
    schema: &SettingsSchema,
    stored: Map<String, Value>,
) -> Map<String, Value> {
    stored
        .into_iter()
        .filter(|(key, value)| {
            schema
                .merge(&Map::from_iter([(key.clone(), value.clone())]))
                .is_ok()
        })
        .collect()
}

/// Builds a plugin instance's effective settings object (§6.9's
/// `init.settings`) and, later, validated `SETTINGS_CHANGED` broadcasts,
/// over one shared [`SettingsStorage`].
///
/// ## Lock order
/// `writes` serializes read-modify-write mutations (`submit`, `apply`,
/// `import_missing_overrides`) within this service. It is never held across
/// plugin callbacks, [`super::SettingsChangeDelivery::settings_changed`], or
/// `.await` points. When backed by [`super::PluginSettingsStore`], lock order
/// is always `PluginSettingsService::writes -> PluginSettingsStore::plugins`.
pub struct PluginSettingsService {
    storage: Arc<dyn SettingsStorage>,
    /// One read-modify-write mutation at a time: load, validate, and store
    /// never interleave across concurrent submissions, patches, or imports.
    writes: Mutex<()>,
}

impl PluginSettingsService {
    pub fn new(storage: Arc<dyn SettingsStorage>) -> Self {
        Self {
            storage,
            writes: Mutex::new(()),
        }
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
        let stored = valid_overrides(schema, self.storage.load(plugin_id)?);
        let effective = schema.merge(&stored)?;
        Ok(json_map_to_cbor(effective))
    }

    /// `GET /api/plugins/<id>/settings` (A7.1 `PluginSettings`): the
    /// validated schema's entries, the effective values with defaults
    /// applied, and a `secrets` presence map. Every `password` value, and
    /// every `password` entry's own default, is `""`: a secret never
    /// leaves core.
    pub fn dashboard_view(
        &self,
        plugin_id: &str,
        schema: Option<&SettingsSchema>,
    ) -> Result<Value, SettingsError> {
        let Some(schema) = schema else {
            return Ok(json!({"schema": [], "values": {}, "secrets": {}}));
        };
        let stored = valid_overrides(schema, self.storage.load(plugin_id)?);
        let mut values = schema.merge(&stored)?;
        let mut secrets = Map::new();
        for key in schema.password_keys() {
            let configured = values
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty());
            secrets.insert(key.to_owned(), Value::Bool(configured));
            values.insert(key.to_owned(), Value::String(String::new()));
        }
        let document: Vec<Value> = schema
            .document()
            .iter()
            .map(|entry| {
                let mut entry = entry.clone();
                let password = entry
                    .get("uniqueID")
                    .and_then(Value::as_str)
                    .is_some_and(|key| schema.is_password(key));
                if password && let Some(object) = entry.as_object_mut() {
                    object.insert("value".into(), Value::String(String::new()));
                }
                entry
            })
            .collect();
        Ok(json!({"schema": document, "values": values, "secrets": secrets}))
    }

    /// `POST /api/plugins/<id>/settings` `[{"uniqueID","value"}]`: a
    /// `password` value of `""` keeps the stored secret and `null` clears
    /// it. The whole resulting document is validated before anything is
    /// stored, so an unknown key or a wrong type changes nothing. Returns
    /// the one `SETTINGS_CHANGED` carrying the complete effective object
    /// read back from storage (§6.9).
    pub fn submit(
        &self,
        plugin_id: &str,
        schema: &SettingsSchema,
        inputs: Vec<(String, Value)>,
    ) -> Result<Message, SettingsError> {
        if !self.storage.editable(plugin_id) {
            return Err(SettingsError::ManagedElsewhere(plugin_id.to_owned()));
        }
        let _write = self.writes.lock().unwrap_or_else(PoisonError::into_inner);
        let mut overrides = valid_overrides(schema, self.storage.load(plugin_id)?);
        for (key, value) in inputs {
            let value = if schema.is_password(&key) {
                match value {
                    Value::Null => Value::String(String::new()),
                    Value::String(text) if text.is_empty() => continue,
                    value => value,
                }
            } else {
                value
            };
            overrides.insert(key, value);
        }
        schema.merge(&overrides)?;
        self.storage.store(plugin_id, &overrides)?;
        let stored = valid_overrides(schema, self.storage.load(plugin_id)?);
        Ok(Message::SettingsChanged(SettingsChanged {
            values: json_map_to_cbor(schema.merge(&stored)?),
        }))
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
        let _write = self.writes.lock().unwrap_or_else(PoisonError::into_inner);
        let mut merged = self.storage.load(plugin_id)?;
        for (key, value) in patch {
            merged.insert(key.clone(), value.clone());
        }
        let effective = schema.merge(&merged)?;
        self.storage.store(plugin_id, &effective)?;
        // Storage may adjust what was actually saved (bridge B2's
        // `ConfigStore::update` clamps `POLL_RATE`/`PRECISE_DATA_POLL_RATE`
        // to their schema minimums, for instance): rebuild the broadcast
        // from what storage now reports is actually stored, never from
        // what this call merely asked it to store, so a plugin is never
        // told a value it did not get.
        let stored = self.storage.load(plugin_id)?;
        let actual_effective = schema.merge(&stored)?;
        Ok(Message::SettingsChanged(SettingsChanged {
            values: json_map_to_cbor(actual_effective),
        }))
    }

    /// Idempotently imports explicitly-present `legacy_overrides` for
    /// `plugin_id` (A8.1 / A8.3).
    ///
    /// - Existing valid per-plugin overrides in storage always win per key.
    /// - Only keys absent from valid stored overrides are imported.
    /// - Every candidate legacy value is validated against `schema` before any
    ///   write; malformed input fails closed without modifying storage.
    /// - Durable storage succeeds before returning [`ImportOutcome`]; callers
    ///   may deliver [`ImportOutcome::settings_changed_message`] afterwards
    ///   with no service lock held.
    pub fn import_missing_overrides(
        &self,
        plugin_id: &str,
        schema: &SettingsSchema,
        legacy_overrides: &Map<String, Value>,
    ) -> Result<ImportOutcome, SettingsError> {
        let _write = self.writes.lock().unwrap_or_else(PoisonError::into_inner);
        let existing = self.storage.load(plugin_id)?;
        let plan = plan_import(schema, existing, legacy_overrides)?;
        if !plan.imported_keys.is_empty() {
            self.storage.store(plugin_id, &plan.next_overrides)?;
        }
        let stored = valid_overrides(schema, self.storage.load(plugin_id)?);
        let effective = schema.merge(&stored)?;
        Ok(ImportOutcome {
            imported_keys: plan.imported_keys,
            preserved_existing_keys: plan.preserved_existing_keys,
            effective,
        })
    }
}
