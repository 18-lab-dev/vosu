//! The unrestricted engine remains a dev-only, offline oracle, not a core dependency.
use serde_json::{Value, json};
use vosu_core::plugin::action_schema::CompiledActionSchema;

#[test]
fn action_schema_differential_supported_profile() {
    let schemas = [
        json!(true),
        json!(false),
        json!({"type":["integer","null"],"minimum":-2,"exclusiveMaximum":4}),
        json!({"type":"number","exclusiveMinimum":0,"maximum":7}),
        json!({"type":"string","minLength":2,"maxLength":3}),
        json!({"type":"array","items":{"type":"integer"},"minItems":1,"maxItems":2}),
        json!({"properties":{"x":{"enum":[1,"é🦀",null]}},"required":["x"],"additionalProperties":false}),
        json!({"additionalProperties":{"type":"boolean"}}),
        json!({"const":{"x":[1,null]}}),
        json!({"$defs":{"a/b":{"type":"integer"}},"$ref":"#/$defs/a~1b","minimum":1}),
        json!({"minItems":3,"maxItems":1}),
        json!({"minLength":2.0,"maxLength":1e30}),
        json!({"$defs":{"a b":{"type":"integer"}},"$ref":"#/%24defs/a%20b"}),
    ];
    let mut values = vec![
        Value::Null,
        json!(true),
        json!(false),
        json!(""),
        json!("é"),
        json!("é🦀"),
        json!("abc"),
        json!("abcd"),
        json!([]),
        json!([1]),
        json!([1, 2]),
        json!([1, "bad"]),
        json!([1, 2, 3]),
        json!({}),
        json!({"x":1}),
        json!({"x":1.0}),
        json!({"x":[1.0,null]}),
        json!({"x":"é🦀"}),
        json!({"x":true}),
        json!({"x":1,"extra":2}),
        json!({"flag":false}),
    ];
    for n in -8..16 {
        values.push(json!(n));
        values.push(json!(f64::from(n) / 2.0));
    }
    for schema in schemas {
        let compiled = CompiledActionSchema::compile(schema.to_string()).unwrap();
        // All refs are document-local; this oracle never retrieves external resources.
        let oracle = jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .build(&schema)
            .unwrap();
        for value in &values {
            assert_eq!(
                compiled.validate_arguments(value).is_ok(),
                oracle.is_valid(value),
                "schema={schema}, value={value}"
            );
            assert_eq!(
                compiled.validate_result(value).is_ok(),
                oracle.is_valid(value),
                "schema={schema}, value={value}"
            );
        }
    }
}
