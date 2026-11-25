use super::*;
use serde_json::json;

#[test]
fn filter_parser_preserves_coercion_and_bounds() {
    let parsed =
        parse_filters(r#"[null,false,12,"x",{"field":[],"keys":["a"]},{"field":true,"keys":[]}]"#)
            .unwrap();
    assert_eq!(
        parsed,
        vec![
            Filter::Field("x".into()),
            Filter::Nested {
                field: "".into(),
                keys: vec![Filter::Field("a".into())]
            },
            Filter::Nested {
                field: "true".into(),
                keys: vec![]
            }
        ]
    );
    assert!(parse_filters(&serde_json::to_string(&vec!["x"; 257]).unwrap()).is_none());
    let mut nested = serde_json::json!("x");
    for _ in 0..9 {
        nested = serde_json::json!({"field":"x","keys":[nested]});
    }
    assert!(parse_filters(&serde_json::to_string(&vec![nested]).unwrap()).is_none());
}

#[test]
fn repeated_overlapping_filters_preserve_last_write_and_null_absence() {
    let value = json!({"play": {"score": 12, "accuracy": 0.9}, "null": null});
    let mut document = SharedDocument::new(value);
    let filters = vec![
        Filter::Field("play".into()),
        Filter::Nested {
            field: "play".into(),
            keys: vec![Filter::Field("score".into())],
        },
        Filter::Field("missing".into()),
        Filter::Nested {
            field: "null".into(),
            keys: vec![],
        },
        Filter::Field("null".into()),
    ];
    for _ in 0..3 {
        assert_eq!(
            document.encode(&filters).unwrap().as_ref(),
            br#"{"play":{"score":12},"null":null}"#
        );
    }
}

#[test]
fn unicode_indexing_and_index_key_order_match_javascript() {
    let mut document = SharedDocument::new(json!({"text": "a🎵é"}));
    let filters = vec![Filter::Nested {
        field: "text".into(),
        keys: vec![
            Filter::Field("length".into()),
            Filter::Field("2".into()),
            Filter::Field("0".into()),
            Filter::Field("1".into()),
            Filter::Field("3".into()),
            Filter::Field("4".into()),
        ],
    }];
    assert_eq!(
        document.encode(&filters).unwrap().as_ref(),
        r#"{"text":{"0":"a","1":"\ud83c","2":"\udfb5","3":"é","length":4}}"#.as_bytes()
    );
}

#[test]
fn direct_dto_numbers_match_filtered_serialization() {
    #[derive(Serialize)]
    struct Numbers {
        values: Vec<f64>,
    }
    let values = crate::wire_fixtures::NUMBERS.to_vec();
    let expected = crate::json::encode(
        &Numbers {
            values: values.clone(),
        },
        0,
    )
    .unwrap();
    let mut document = SharedDocument::new(Numbers { values });
    assert_eq!(
        document
            .encode(&[Filter::Field("values".into())])
            .unwrap()
            .as_ref(),
        expected
    );
}
