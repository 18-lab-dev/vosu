#[test]
fn nested_filters_project_only_declared_fields() {
    let source = serialize_projection_value(&serde_json::json!({
        "menu": {"state": 2, "bm": {"id": 7, "title": "x"}},
        "play": {"score": 10}
    }))
    .unwrap();
    let projected = project_filters(
        &source,
        &[Filter::Nested {
            field: "menu".into(),
            keys: vec![
                Filter::Field("state".into()),
                Filter::Nested {
                    field: "bm".into(),
                    keys: vec![Filter::Field("id".into())],
                },
            ],
        }],
    );
    assert_eq!(
        encode_javascript_json(&projected).unwrap(),
        Bytes::from_static(br#"{"menu":{"state":2,"bm":{"id":7}}}"#)
    );
}

#[test]
fn filtered_json_preserves_requested_order_and_javascript_numbers() {
    let source = serialize_projection_value(&serde_json::json!({
        "menu": {
            "negativeZero": -0.0,
            "fixedUpper": 100_000_000_000_000_000_000.0,
            "exponentialUpper": 1.0e21,
            "fixedLower": 1.0e-6,
            "exponentialLower": 1.0e-7,
        },
        "play": 1,
    }))
    .unwrap();
    let projected = project_filters(
        &source,
        &[Filter::Nested {
            field: "menu".into(),
            keys: vec![
                Filter::Field("negativeZero".into()),
                Filter::Field("fixedUpper".into()),
                Filter::Field("exponentialUpper".into()),
                Filter::Field("fixedLower".into()),
                Filter::Field("exponentialLower".into()),
            ],
        }],
    );
    assert_eq!(
        encode_javascript_json(&projected).unwrap(),
        Bytes::from_static(
            br#"{"menu":{"negativeZero":0,"fixedUpper":100000000000000000000,"exponentialUpper":1e+21,"fixedLower":0.000001,"exponentialLower":1e-7}}"#,
        )
    );
}

#[test]
fn nested_filters_follow_javascript_primitive_array_and_index_order() {
    let source = serialize_projection_value(&serde_json::json!({
        "client": "stable",
        "nullish": null,
        "hits": {"300": 3, "50": 1, "100": 2, "geki": 4},
        "clients": [{"name": "one"}, {"name": "two"}],
    }))
    .unwrap();
    let projected = project_filters(
        &source,
        &[
            Filter::Nested {
                field: "client".into(),
                keys: vec![Filter::Field("x".into())],
            },
            Filter::Nested {
                field: "nullish".into(),
                keys: vec![Filter::Field("x".into())],
            },
            Filter::Nested {
                field: "hits".into(),
                keys: vec![
                    Filter::Field("300".into()),
                    Filter::Field("50".into()),
                    Filter::Field("100".into()),
                    Filter::Field("geki".into()),
                ],
            },
            Filter::Nested {
                field: "clients".into(),
                keys: vec![Filter::Nested {
                    field: "1".into(),
                    keys: vec![Filter::Field("name".into())],
                }],
            },
        ],
    );
    let projected = canonicalize_javascript_keys(projected);
    assert_eq!(
        encode_javascript_json(&projected).unwrap(),
        Bytes::from_static(
            br#"{"client":{},"hits":{"50":1,"100":2,"300":3,"geki":4},"clients":{"1":{"name":"two"}}}"#,
        )
    );
}

#[test]
fn javascript_properties_cover_array_and_utf16_string_access() {
    let source = serialize_projection_value(&serde_json::json!({
        "errors": [1, 2, 3],
        "label": "A💩B",
        "surrogate": "💩",
    }))
    .unwrap();
    let projected = canonicalize_javascript_keys(project_filters(
        &source,
        &[
            Filter::Nested {
                field: "errors".into(),
                keys: vec![Filter::Field("length".into()), Filter::Field("1".into())],
            },
            Filter::Nested {
                field: "label".into(),
                keys: vec![
                    Filter::Field("length".into()),
                    Filter::Field("0".into()),
                    Filter::Field("1".into()),
                    Filter::Field("2".into()),
                    Filter::Field("3".into()),
                ],
            },
            Filter::Nested {
                field: "surrogate".into(),
                keys: vec![Filter::Nested {
                    field: "0".into(),
                    keys: vec![Filter::Field("length".into()), Filter::Field("0".into())],
                }],
            },
        ],
    ));
    assert_eq!(
        encode_javascript_json(&projected).unwrap(),
        Bytes::from_static(
            br#"{"errors":{"1":2,"length":3},"label":{"0":"A","1":"\ud83d","2":"\udca9","3":"B","length":4},"surrogate":{"0":{"0":"\ud83d","length":1}}}"#,
        )
    );
}

#[test]
fn filtered_nonfinite_numbers_remain_values_until_json_encoding() {
    #[derive(Serialize)]
    struct NonFinite {
        direct_f32: f32,
        direct_f64: f64,
        nested_infinity: f64,
        nested_nan: f64,
    }

    let source = serialize_projection_value(&NonFinite {
        direct_f32: f32::NAN,
        direct_f64: f64::NEG_INFINITY,
        nested_infinity: f64::INFINITY,
        nested_nan: f64::NAN,
    })
    .unwrap();
    let projected = project_filters(
        &source,
        &[
            Filter::Field("direct_f32".into()),
            Filter::Field("direct_f64".into()),
            Filter::Nested {
                field: "nested_infinity".into(),
                keys: vec![Filter::Field("missing".into())],
            },
            Filter::Nested {
                field: "nested_nan".into(),
                keys: vec![Filter::Field("\u{0}vosu-nonfinite-number".into())],
            },
        ],
    );
    assert_eq!(
        encode_javascript_json(&projected).unwrap(),
        Bytes::from_static(
            br#"{"direct_f32":null,"direct_f64":null,"nested_infinity":{},"nested_nan":{}}"#,
        )
    );
}

#[test]
fn v2_custom_number_serialization_preserves_nonfinite_projection_semantics() {
    #[derive(Serialize)]
    struct Sessions {
        direct: crate::model::api::v2::Session,
        nested: crate::model::api::v2::Session,
    }

    let source = serialize_projection_value(&Sessions {
        direct: crate::model::api::v2::Session {
            play_time: f64::NAN,
            play_count: 0,
        },
        nested: crate::model::api::v2::Session {
            play_time: f64::INFINITY,
            play_count: 0,
        },
    })
    .unwrap();
    let projected = project_filters(
        &source,
        &[
            Filter::Nested {
                field: "direct".into(),
                keys: vec![Filter::Field("playTime".into())],
            },
            Filter::Nested {
                field: "nested".into(),
                keys: vec![Filter::Nested {
                    field: "playTime".into(),
                    keys: vec![Filter::Field("missing".into())],
                }],
            },
        ],
    );
    assert_eq!(
        encode_javascript_json(&projected).unwrap(),
        Bytes::from_static(br#"{"direct":{"playTime":null},"nested":{"playTime":{}}}"#)
    );
}

#[test]
fn projection_nonfinite_state_cannot_collide_with_source_values() {
    #[derive(Serialize)]
    struct Collision<'a> {
        ordinary: serde_json::Value,
        text: &'a str,
        nonfinite: f64,
    }

    let source = serialize_projection_value(&Collision {
        ordinary: serde_json::json!({"\u{0}vosu-nonfinite-number": true}),
        text: r#"{"\u0000vosu-nonfinite-number":true}"#,
        nonfinite: f64::NAN,
    })
    .unwrap();
    let projected = project_filters(
        &source,
        &[
            Filter::Field("ordinary".into()),
            Filter::Field("text".into()),
            Filter::Field("nonfinite".into()),
        ],
    );
    assert_eq!(
        encode_javascript_json(&projected).unwrap(),
        Bytes::from_static(
            br#"{"ordinary":{"\u0000vosu-nonfinite-number":true},"text":"{\"\\u0000vosu-nonfinite-number\":true}","nonfinite":null}"#,
        )
    );
}

