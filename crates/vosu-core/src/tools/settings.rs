//! Tool `settings.json` schemas and stored values, core-side (A7.1: core owns
//! tool settings; the tool host only receives the password-filtered values).
//! Documents are accepted only through the SDK's one settings-schema parser;
//! this module keeps the unchanged 1.x presentation shape and value coercion.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
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

/// One schema entry in the 1.x overlay/tool settings format.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Setting {
    #[serde(rename = "uniqueID")]
    unique_id: String,
    #[serde(
        rename = "uniqueCheck",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    unique_check: Option<String>,
    #[serde(rename = "type")]
    kind: Kind,
    #[serde(default)]
    title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    options: Option<Value>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    value: Value,
}

/// One submitted value (`POST /api/tools/<id>/settings`).
#[derive(Deserialize)]
pub(super) struct Input {
    #[serde(rename = "uniqueID")]
    unique_id: String,
    value: Value,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Schema(Vec<Setting>);

fn coerce_number(value: Value) -> Value {
    match value {
        Value::Number(_) | Value::Null => value,
        Value::String(text) => text
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| Value::from(0), Value::Number),
        Value::Bool(value) => Value::from(i32::from(value)),
        _ => Value::from(0),
    }
}
fn coerce_checkbox(value: Value) -> Value {
    match value {
        Value::String(text) => Value::Bool(text == "true"),
        other => other,
    }
}

impl Schema {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
        vosu_plugin_api::manifest::settings::SettingsSchema::parse(text)
            .map_err(|error| error.to_string())?;
        let mut settings: Vec<Setting> =
            serde_json::from_str(text).map_err(|error| error.to_string())?;
        for setting in &mut settings {
            setting.value = match setting.kind {
                Kind::Number => coerce_number(setting.value.take()),
                Kind::Checkbox => coerce_checkbox(setting.value.take()),
                _ => setting.value.take(),
            };
        }
        Ok(Self(settings))
    }

    /// Defaults overlaid with the stored values of declared, value-bearing keys.
    pub(super) fn effective(&self, stored: Option<Map<String, Value>>) -> Map<String, Value> {
        let mut values: Map<String, Value> = self
            .0
            .iter()
            .filter(|setting| setting.kind != Kind::Header)
            .map(|setting| (setting.unique_id.clone(), setting.value.clone()))
            .collect();
        for (id, value) in stored.into_iter().flatten() {
            if let Some(slot) = values.get_mut(&id) {
                *slot = value;
            }
        }
        values
    }

    fn passwords(&self) -> impl Iterator<Item = &str> {
        self.0
            .iter()
            .filter(|setting| setting.kind == Kind::Password)
            .map(|setting| setting.unique_id.as_str())
    }

    /// The `_settings` view: every `password` value omitted.
    pub(super) fn public(&self, mut values: Map<String, Value>) -> Map<String, Value> {
        for id in self.passwords() {
            values.remove(id);
        }
        values
    }

    /// `GET /api/tools/<id>/settings`: password values are write-only; the
    /// dashboard learns only whether one is configured.
    pub(super) fn dashboard(&self, mut values: Map<String, Value>) -> Value {
        let mut secrets = Map::new();
        for id in self.passwords() {
            let configured = values
                .get(id)
                .is_some_and(|value| !value.is_null() && value.as_str() != Some(""));
            secrets.insert(id.to_owned(), json!(configured));
            values.insert(id.to_owned(), json!(""));
        }
        let settings: Vec<Setting> = self
            .0
            .iter()
            .cloned()
            .map(|mut setting| {
                if setting.kind == Kind::Password {
                    setting.value = json!("");
                }
                setting
            })
            .collect();
        json!({"settings": settings, "values": values, "secrets": secrets})
    }

    /// Applies submitted values: a blank password keeps the stored secret and
    /// `null` clears it; undeclared ids are ignored.
    pub(super) fn apply(&self, values: &mut Map<String, Value>, inputs: Vec<Input>) {
        for input in inputs {
            let Some(setting) = self.0.iter().find(|s| s.unique_id == input.unique_id) else {
                continue;
            };
            let value = match setting.kind {
                Kind::Number => coerce_number(input.value),
                Kind::Checkbox => coerce_checkbox(input.value),
                Kind::Password => match input.value {
                    Value::Null => json!(""),
                    Value::String(text) if text.is_empty() => continue,
                    value => value,
                },
                _ => input.value,
            };
            values.insert(setting.unique_id.clone(), value);
        }
    }
}
