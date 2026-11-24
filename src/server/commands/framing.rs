use bytes::Bytes;
use serde_json::{Map, Value};

use super::CommandAction;

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
