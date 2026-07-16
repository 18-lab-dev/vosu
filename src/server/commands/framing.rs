use bytes::Bytes;
use serde_json::{Map, Value};

use super::{CommandAction, Filter};

pub(super) fn parse_filters(source: &str) -> Option<Vec<Filter>> {
    let value = serde_json::from_str::<Value>(source).ok()?;
    let values = value.as_array()?;
    Some(values.iter().filter_map(parse_filter).collect())
}

fn parse_filter(value: &Value) -> Option<Filter> {
    if let Some(field) = value.as_str() {
        return Some(Filter::Field(field.to_owned()));
    }
    let object = value.as_object()?;
    let field = filter_field_key(object.get("field")?)?;
    let keys = object.get("keys")?.as_array()?;
    Some(Filter::Nested {
        field,
        keys: keys.iter().filter_map(parse_filter).collect(),
    })
}

fn filter_field_key(value: &Value) -> Option<String> {
    match value {
        Value::Null | Value::Bool(false) => None,
        Value::Bool(true) => Some("true".into()),
        Value::Number(number) => {
            let value = number.as_f64()?;
            (value != 0.0).then(|| crate::calculation::js_json_number(value))
        }
        Value::String(value) => (!value.is_empty()).then(|| value.clone()),
        // Arrays and objects are truthy in JavaScript even when empty.
        Value::Array(values) => Some(
            values
                .iter()
                .map(js_filter_value_to_string)
                .collect::<Vec<_>>()
                .join(","),
        ),
        Value::Object(_) => Some("[object Object]".into()),
    }
}

fn js_filter_value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(value) => value.to_string(),
        Value::Number(number) => {
            crate::calculation::js_json_number(number.as_f64().expect("JSON numbers are finite"))
        }
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(js_filter_value_to_string)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

pub(super) fn reply(command: &str, message: Option<Value>) -> CommandAction {
    let mut object = Map::new();
    object.insert("command".into(), Value::String(command.into()));
    if let Some(message) = message {
        object.insert("message".into(), message);
    }
    CommandAction::Reply(Bytes::from(
        serde_json::to_vec(&Value::Object(object)).expect("JSON value serializes"),
    ))
}

pub(super) fn error_value(error: impl Into<String>) -> Value {
    let mut object = Map::new();
    object.insert("error".into(), Value::String(error.into()));
    Value::Object(object)
}
