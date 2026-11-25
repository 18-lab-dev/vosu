//! Compiled A3 action schemas. No I/O, callbacks or runtime schema-engine dependency.
//!
//! This is the one bounded-profile check: vosu's catalog install and plugin
//! load paths and `vosu-package-check` all compile action schemas here, so
//! the checker cannot pass a schema the host refuses.
mod compile;
mod validate;

use serde_json::Value;

pub const MAX_SCHEMA_BYTES: usize = 256 * 1024;
pub const MAX_DEPTH: usize = 32;
pub const MAX_SCHEMA_NODES: usize = 4_096;
pub const MAX_COMPILED_NODES: usize = 65_536;
pub const MAX_REFERENCE_DEPTH: usize = 32;
pub const MAX_VALUE_NODES: usize = 65_536;
pub const MAX_WORK: usize = 1_048_576;

/// Contains only pointers and a fixed diagnostic, never an input value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{reason} (schema {schema_pointer}, instance {instance_pointer})")]
pub struct SchemaError {
    pub schema_pointer: String,
    pub instance_pointer: String,
    pub reason: &'static str,
}

/// Value validation failures never retain the rejected value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{reason} (schema {schema_pointer}, instance {instance_pointer})")]
pub struct ValidationError {
    pub schema_pointer: String,
    pub instance_pointer: String,
    pub reason: &'static str,
}

fn error(schema: &str, instance: &str, reason: &'static str) -> SchemaError {
    SchemaError {
        schema_pointer: schema.to_owned(),
        instance_pointer: instance.to_owned(),
        reason,
    }
}

fn pointer(parent: &str, key: &str) -> String {
    format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1"))
}

#[derive(Debug)]
pub struct CompiledActionSchema {
    root: Node,
}

#[derive(Debug)]
struct Node {
    path: String,
    schema: Rule,
}

#[derive(Debug)]
enum Rule {
    Boolean(bool),
    Object(Box<Constraints>),
}

#[derive(Debug, Default)]
struct Constraints {
    types: Vec<String>,
    properties: std::collections::BTreeMap<String, Node>,
    required: Vec<String>,
    additional: Option<Node>,
    items: Option<Node>,
    reference: Option<Node>,
    enumeration: Option<Vec<Value>>,
    constant: Option<Value>,
    min_items: Option<serde_json::Number>,
    max_items: Option<serde_json::Number>,
    min_length: Option<serde_json::Number>,
    max_length: Option<serde_json::Number>,
    minimum: Option<serde_json::Number>,
    maximum: Option<serde_json::Number>,
    exclusive_minimum: Option<serde_json::Number>,
    exclusive_maximum: Option<serde_json::Number>,
}

#[cfg(test)]
mod tests;
