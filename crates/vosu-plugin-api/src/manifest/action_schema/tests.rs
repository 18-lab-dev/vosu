use super::*;
use serde_json::json;

#[test]
fn action_schema_nested_errors_are_private_and_path_aware() {
    let schema = CompiledActionSchema::compile(r#"{"type":"object","properties":{"a/b":{"type":"array","items":{"type":"integer"}}},"required":["a/b"],"additionalProperties":false}"#).unwrap();
    assert!(schema.validate_arguments(&json!({"a/b":[7,9]})).is_ok());
    let error = schema
        .validate_arguments(&json!({"a/b":[7,"PRIVATE_SENTINEL"]}))
        .unwrap_err();
    assert_eq!(error.instance_pointer, "/a~1b/1");
    assert_eq!(error.schema_pointer, "/properties/a~1b/items");
    assert!(!error.to_string().contains("PRIVATE_SENTINEL"));
    assert!(schema.validate_arguments(&json!({})).is_err());
    assert!(
        schema
            .validate_result(&json!({"a/b":[],"unexpected":1}))
            .is_err()
    );
}

#[test]
fn action_schema_rejects_invalid_and_unsupported_schemas() {
    for text in [
        r#"{"pattern":"x"}"#,
        r#"{"type":[]}"#,
        r#"{"type":["string","string"]}"#,
        r#"{"enum":[]}"#,
        r#"{"enum":[1,1.0]}"#,
        r#"{"required":["x","x"]}"#,
        r#"{"minLength":-1}"#,
        r#"{"items":[]}"#,
        r#"{"$ref":"https://example.invalid"}"#,
        r##"{"$ref":"#/$defs/missing"}"##,
        r##"{"$defs":{"loop":{"$ref":"#/$defs/loop"}}}"##,
        r#"{"$defs":{"unused":{"format":"uri"}}}"#,
    ] {
        assert!(
            CompiledActionSchema::compile(text).is_err(),
            "accepted {text}"
        );
    }
}

#[test]
fn action_schema_reference_siblings_unicode_and_applicability() {
    let schema = CompiledActionSchema::compile(r##"{"$defs":{"a/b":{"type":"string","minLength":2}},"$ref":"#/$defs/a~1b","maxLength":3,"enum":["é🦀","abc"]}"##).unwrap();
    assert!(schema.validate_arguments(&json!("é🦀")).is_ok());
    for value in [json!("a"), json!("abcd"), json!("xyz"), json!(3)] {
        assert!(schema.validate_arguments(&value).is_err());
    }
    let schema = CompiledActionSchema::compile(
        r#"{"properties":{"x":false},"items":false,"minLength":5,"minimum":4}"#,
    )
    .unwrap();
    assert!(schema.validate_arguments(&Value::Null).is_ok());
    assert!(schema.validate_arguments(&json!({"x":1})).is_err());
    assert!(
        CompiledActionSchema::compile("false")
            .unwrap()
            .validate_result(&json!(1))
            .is_err()
    );
}

#[test]
fn action_schema_numeric_equality_is_not_lossy() {
    let schema = CompiledActionSchema::compile(r#"{"const":9007199254740993}"#).unwrap();
    assert!(
        schema
            .validate_arguments(&json!(9007199254740993u64))
            .is_ok()
    );
    assert!(
        schema
            .validate_arguments(&json!(9007199254740992.0))
            .is_err()
    );
    assert!(
        CompiledActionSchema::compile(r#"{"const":{"a":[1]}}"#)
            .unwrap()
            .validate_result(&json!({"a":[1.0]}))
            .is_ok()
    );
}

#[test]
fn action_schema_all_budgets_fail_closed() {
    assert!(CompiledActionSchema::compile(" ".repeat(MAX_SCHEMA_BYTES + 1)).is_err());
    let mut deep = json!(true);
    for _ in 0..MAX_DEPTH {
        deep = json!({"items":deep});
    }
    assert!(CompiledActionSchema::compile(deep.to_string()).is_ok());
    deep = json!({"items":deep});
    assert!(CompiledActionSchema::compile(deep.to_string()).is_err());
    let properties: serde_json::Map<String, Value> = (0..MAX_SCHEMA_NODES)
        .map(|i| (i.to_string(), json!(true)))
        .collect();
    assert!(CompiledActionSchema::compile(json!({"properties":properties}).to_string()).is_err());
    let schema = CompiledActionSchema::compile("true").unwrap();
    assert!(
        schema
            .validate_arguments(&json!(vec![0; MAX_VALUE_NODES]))
            .is_err()
    );
    let mut deep = json!(0);
    for _ in 0..=MAX_DEPTH {
        deep = json!([deep]);
    }
    assert!(schema.validate_result(&deep).is_err());
    let schema = CompiledActionSchema::compile(r#"{"minLength":1}"#).unwrap();
    assert!(
        schema
            .validate_arguments(&json!("x".repeat(MAX_WORK)))
            .is_err()
    );
    let mut definitions = serde_json::Map::new();
    definitions.insert("n0".into(), json!(true));
    for i in 1..18 {
        definitions.insert(format!("n{i}"), json!({"properties":{"a":{"$ref":format!("#/$defs/n{}",i-1)},"b":{"$ref":format!("#/$defs/n{}",i-1)}}}));
    }
    assert_eq!(
        CompiledActionSchema::compile(json!({"$defs":definitions}).to_string())
            .unwrap_err()
            .reason,
        "schema compilation budget exceeded"
    );
}

#[test]
fn action_schema_reference_depth_and_decoded_fragments() {
    let mut definitions = serde_json::Map::new();
    definitions.insert("n0".into(), json!(true));
    for i in 1..=MAX_REFERENCE_DEPTH + 1 {
        definitions.insert(format!("n{i}"), json!({"$ref":format!("#/$defs/n{}",i-1)}));
    }
    let text = json!({"$defs":definitions}).to_string();
    assert_eq!(
        CompiledActionSchema::compile(text).unwrap_err().reason,
        "reference cycle or depth exceeded"
    );
    let schema = CompiledActionSchema::compile(
        r##"{"$defs":{"a b":{"const":7}},"$ref":"#/%24defs/a%20b"}"##,
    )
    .unwrap();
    assert!(schema.validate_arguments(&json!(7)).is_ok());
    assert!(schema.validate_arguments(&json!(8)).is_err());
}
