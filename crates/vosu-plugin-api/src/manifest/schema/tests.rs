use super::*;
use serde_json::{Value, json};
fn document() -> Value {
    json!({"title":"test.schema","version":"2.3.0","properties":{}})
}
fn run(value: &Value) -> Result<CompiledSchema, SchemaError> {
    compile(
        "test.schema",
        &"2.3.0".parse().unwrap(),
        SchemaProvenance::Untrusted,
        &value.to_string(),
        &json!({}),
    )
}
fn lane(lane: &str, activity: &str) -> Value {
    json!({"x-sampling-lane":{"lane":lane,"activity":activity}})
}
#[test]
fn manifest_schema_canonical_mixed_siblings_wildcards() {
    let mut doc = document();
    doc["$defs"] = json!({"a/b~c":{"properties":{"n":lane("regular","always")}}});
    doc["properties"] = json!({
        "map":{"additionalProperties":lane("precise","gameplay")},
        "array":{"items":lane("regular","always")},
        "joined":{"$ref":"#/$defs/a~1b~0c", "properties":{"n":lane("precise","gameplay")},
                  "allOf":[{"properties":{"n":lane("regular","always")}}]},
        "a/b~c":lane("both","always")
    });
    let compiled = run(&doc).unwrap();
    let expected = vec![
        ("/array/*", "regular", "always"),
        ("/a~1b~0c", "both", "always"),
        ("/joined/n", "precise", "gameplay"),
        ("/joined/n", "regular", "always"),
        ("/map/*", "precise", "gameplay"),
    ]
    .into_iter()
    .map(|(pointer, lane, activity)| LaneRecord {
        pointer: pointer.into(),
        lane: lane.into(),
        activity: activity.into(),
    })
    .collect::<Vec<_>>();
    assert_eq!(compiled.lanes, expected);
    validate_records(&compiled.lanes).unwrap();
    assert_eq!(
        demand(&compiled.lanes, &["joined", "n"]).effective(false),
        (true, false)
    );
    assert_eq!(
        demand(&compiled.lanes, &["joined", "n"]).effective(true),
        (true, true)
    );
    assert_eq!(
        demand(&compiled.lanes, &["map", "any", "leaf"]).effective(true),
        (false, true)
    );
    assert_eq!(
        demand(&compiled.lanes, &["a/b~c"]).effective(false),
        (true, true)
    );
    assert_eq!(demand(&compiled.lanes, &["missing"]), DemandBits::default());
}
#[test]
fn manifest_schema_full_document_refs_and_cycles() {
    for reference in [
        "https://example.org/s",
        "file:///tmp/a",
        "#anchor",
        "#/$defs/missing",
        "#/$defs/a~2b",
    ] {
        let mut doc = document();
        doc["$defs"] = json!({"unused":{"$ref":reference}});
        assert!(run(&doc).is_err(), "{reference}");
    }
    let mut doc = document();
    doc["$defs"] = json!({"a":{"$ref":"#/$defs/b"},"b":{"$ref":"#/$defs/a"}});
    assert!(matches!(run(&doc), Err(SchemaError::CyclicRef(_))));
    doc["$defs"] = json!({"a":{"type":"number"}});
    assert!(run(&doc).is_ok());
}
#[test]
fn manifest_schema_version_title_and_id() {
    let mut doc = document();
    doc["$id"] = json!("https://untrusted.example/schema");
    assert!(run(&doc).is_ok());
    doc["version"] = json!("2.4.0");
    assert_eq!(run(&doc), Err(SchemaError::VersionMismatch));
    doc["version"] = json!("2.3.0");
    doc["title"] = json!("wrong.namespace");
    assert!(matches!(run(&doc), Err(SchemaError::TitleMismatch { .. })));
}
#[test]
fn manifest_schema_exact_byte_and_depth_limits() {
    let mut text = document().to_string();
    text.push_str(&" ".repeat(MAX_BYTES - text.len()));
    let version = "2.3.0".parse().unwrap();
    assert!(
        compile(
            "test.schema",
            &version,
            SchemaProvenance::Untrusted,
            &text,
            &json!({})
        )
        .is_ok()
    );
    text.push(' ');
    assert_eq!(
        compile(
            "test.schema",
            &version,
            SchemaProvenance::Untrusted,
            &text,
            &json!({})
        ),
        Err(SchemaError::TooLarge)
    );
    let mut doc = document();
    let mut nested = json!({});
    for _ in 0..29 {
        nested = json!({"nest":nested});
    }
    doc["$defs"] = json!({"unused":nested});
    assert!(run(&doc).is_ok());
    doc["$defs"]["unused"] = json!({"nest":doc["$defs"]["unused"].clone()});
    assert_eq!(run(&doc), Err(SchemaError::TooDeep));
}
#[test]
fn manifest_schema_visibility_requires_trusted_provenance() {
    let mut doc = json!({"title":"vosu.osu","version":"1.2.0","properties":{"internal":{"x-visibility":"plugins"}}});
    let version = "1.2.0".parse().unwrap();
    assert!(matches!(
        compile(
            "vosu.osu",
            &version,
            SchemaProvenance::Untrusted,
            &doc.to_string(),
            &json!({})
        ),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));
    let compiled = compile(
        "vosu.osu",
        &version,
        SchemaProvenance::TrustedOfficial,
        &doc.to_string(),
        &json!({}),
    )
    .unwrap();
    assert_eq!(compiled.hidden, ["/internal"]);
    doc["$defs"] = json!({"unused":{"x-visibility":"plugins"}});
    assert!(matches!(
        compile(
            "vosu.osu",
            &version,
            SchemaProvenance::TrustedOfficial,
            &doc.to_string(),
            &json!({})
        ),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));
}
#[test]
fn manifest_schema_unsupported_lanes_and_property_collision() {
    for keyword in [
        "not",
        "if",
        "then",
        "else",
        "contains",
        "unevaluatedProperties",
        "patternProperties",
        "prefixItems",
    ] {
        let mut doc = document();
        doc["properties"]["x"][keyword] = lane("regular", "always");
        assert!(
            matches!(run(&doc), Err(SchemaError::UnsupportedLane(_))),
            "{keyword}"
        );
        doc["properties"]["x"][keyword] = json!({"minimum":5});
        assert!(run(&doc).is_ok(), "ordinary constraint {keyword}");
    }
    let mut doc = document();
    doc["properties"]["*"] = lane("regular", "always");
    assert!(matches!(run(&doc), Err(SchemaError::InvalidPointer(_))));
    doc["properties"] = json!({"x":{"x-sampling-lane":{"lane":"fast","activity":"always"}}});
    assert!(matches!(run(&doc), Err(SchemaError::InvalidLane(_))));
}
#[test]
fn manifest_schema_reference_work_bound_and_root_lane() {
    let mut doc = document();
    doc["x-sampling-lane"] = json!({"lane":"precise","activity":"always"});
    assert_eq!(run(&doc).unwrap().lanes[0].pointer, "");
    for i in 0..30 {
        doc["$defs"][format!("a{i}")] = json!({"allOf":[{"$ref":format!("#/$defs/a{}",i+1)},{"$ref":format!("#/$defs/a{}",i+1)}]});
    }
    doc["$defs"]["a30"] = lane("regular", "always");
    doc["properties"]["x"] = json!({"$ref":"#/$defs/a0"});
    assert_eq!(run(&doc), Err(SchemaError::TooManyNodes));
}
#[test]
fn manifest_schema_common_defs_nested_and_required() {
    let mut doc = document();
    doc["required"] = json!(["number"]);
    doc["properties"]["number"] =
        json!({"$ref":"common-defs.schema.json#/$defs/nested/properties/value"});
    let common = json!({"nested":{"properties":{"value":lane("regular","gameplay")}}});
    let compiled = compile(
        "test.schema",
        &"2.3.0".parse().unwrap(),
        SchemaProvenance::Untrusted,
        &doc.to_string(),
        &common,
    )
    .unwrap();
    assert_eq!(compiled.required_top_level, ["number"]);
    assert_eq!(compiled.lanes[0].pointer, "/number");
}
#[test]
fn manifest_schema_mirror_rows_reject_malformed_order_duplicates() {
    let row = LaneRecord {
        pointer: "/a~0~1/*".into(),
        lane: "both".into(),
        activity: "always".into(),
    };
    assert!(validate_records(std::slice::from_ref(&row)).is_ok());
    assert!(validate_records(&[row.clone(), row.clone()]).is_err());
    let mut bad = row.clone();
    bad.pointer = "/~2".into();
    assert!(validate_records(&[bad]).is_err());
    let mut bad = row;
    bad.activity = "sometimes".into();
    assert!(validate_records(&[bad]).is_err());
}

#[test]
fn manifest_schema_fragment_encoding_and_unused_unsupported_lanes() {
    let mut doc = document();
    doc["$defs"] = json!({"a/b": lane("regular", "always")});
    doc["properties"]["x"] = json!({"$ref": "#/%24defs/a~1b"});
    assert_eq!(run(&doc).unwrap().lanes[0].pointer, "/x");
    doc["properties"]["x"] = json!({"$ref": "#/%GG"});
    assert!(matches!(run(&doc), Err(SchemaError::InvalidPointer(_))));
    doc["properties"] = json!({});
    doc["$defs"]["unused"] = json!({"not": lane("precise", "gameplay")});
    assert!(matches!(run(&doc), Err(SchemaError::UnsupportedLane(_))));
    doc["$defs"]["unused"] = json!({"not": {"minimum": 3}});
    assert!(run(&doc).is_ok());
    doc["properties"] = json!([lane("regular", "always")]);
    assert!(matches!(run(&doc), Err(SchemaError::UnsupportedLane(_))));
}
