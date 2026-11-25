//! Bounded legacy applyFilters parsing and JavaScript key coercion.
use super::Filter;
use serde_json::Value;

const MAX_FILTER_NODES: usize = 256;
const MAX_FILTER_DEPTH: usize = 8;

pub fn parse_filters(source: &str) -> Option<Vec<Filter>> {
    let value = serde_json::from_str::<Value>(source).ok()?;
    let values = value.as_array()?;
    let mut nodes = 0;
    let mut filters = Vec::new();
    for value in values {
        if let Some(filter) = parse_filter(value, 0, &mut nodes)? {
            filters.push(filter);
        }
    }
    Some(filters)
}

fn parse_filter(value: &Value, depth: usize, nodes: &mut usize) -> Option<Option<Filter>> {
    *nodes += 1;
    if *nodes > MAX_FILTER_NODES || depth > MAX_FILTER_DEPTH {
        return None;
    }
    let filter = if let Some(field) = value.as_str() {
        Some(Filter::Field(field.to_owned()))
    } else {
        let Some(object) = value.as_object() else {
            return Some(None);
        };
        let (Some(field), Some(keys)) = (
            object.get("field").and_then(filter_field_key),
            object.get("keys").and_then(Value::as_array),
        ) else {
            return Some(None);
        };
        let mut parsed = Vec::new();
        for key in keys {
            if let Some(key) = parse_filter(key, depth + 1, nodes)? {
                parsed.push(key);
            }
        }
        Some(Filter::Nested {
            field,
            keys: parsed,
        })
    };
    Some(filter)
}

fn filter_field_key(value: &Value) -> Option<String> {
    match value {
        Value::Null | Value::Bool(false) => None,
        Value::Bool(true) => Some("true".into()),
        Value::Number(number) => {
            let value = number.as_f64()?;
            (value != 0.0).then(|| vosu_osu_model::js_json_number(value))
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
            vosu_osu_model::js_json_number(number.as_f64().expect("JSON numbers are finite"))
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
