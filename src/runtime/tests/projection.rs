#[test]
fn command_filters_preserve_last_write_precedence_at_each_depth() {
    use crate::server::commands::{CommandAction, CommandClient, Relay, RelaySink};

    struct UnusedProviders;
    impl SettingsProvider for UnusedProviders {
        fn get(&self, _: &str) -> Result<serde_json::Value, String> {
            panic!("filter commands must not read settings")
        }
        fn save(&self, _: &str, _: serde_json::Value) -> Result<(), String> {
            panic!("filter commands must not write settings")
        }
    }
    impl RelaySink for UnusedProviders {
        fn relay(&self, _: Relay) {
            panic!("filter commands must not relay settings")
        }
    }

    let providers = Arc::new(UnusedProviders);
    let processor = CommandProcessor::new(providers.clone(), providers);
    let client = CommandClient { id: 1, overlay: String::new() };
    let source = serialize_projection_value(&serde_json::json!({
        "play": {"accuracy": 0.9, "score": 10},
        "menu": {"beatmap": {"title": "x", "artist": "y"}}
    })).unwrap();
    for (filters, expected) in [
        (
            r#"["play",{"field":"play","keys":["accuracy"]},"play"]"#,
            serde_json::json!({"play": {"accuracy": 0.9, "score": 10}}),
        ),
        (
            r#"[{"field":"menu","keys":["beatmap",{"field":"beatmap","keys":["title"]},"beatmap"]}]"#,
            serde_json::json!({"menu": {"beatmap": {"title": "x", "artist": "y"}}}),
        ),
    ] {
        let CommandAction::ApplyFilters(filters) =
            processor.process(&client, &format!("applyFilters:any:{filters}"))
        else {
            panic!("valid filter command was rejected");
        };
        let bytes = encode_javascript_json(&project_filters(&source, &filters)).unwrap();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(), expected);
    }
}

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

/// Filter lists probing every property of `value` down to `depth`: each key
/// alone, array/string pseudo-properties and non-canonical indices beneath it,
/// `Field`/`Nested` precedence in both orders, and empty nested selections.
fn probing_filter_lists(value: &ProjectionValue, depth: usize) -> Vec<Vec<Filter>> {
    let mut keys: Vec<String> = match value {
        ProjectionValue::Object(entries) => entries.iter().map(|(key, _)| key.clone()).collect(),
        ProjectionValue::Array(values) => {
            let mut keys: Vec<_> = (0..values.len().min(3)).map(|i| i.to_string()).collect();
            if values.len() > 3 {
                keys.push((values.len() - 1).to_string());
            }
            keys
        }
        _ => vec!["0".into(), "1".into()],
    };
    keys.extend(["length", "01", "4294967295", "missing"].map(String::from));
    let probe = || {
        ["length", "0", "1", "01", "missing"]
            .map(|key| Filter::Field(key.into()))
            .to_vec()
    };
    let mut lists = Vec::new();
    for key in &keys {
        let field = || Filter::Field(key.clone());
        let nested = |keys| Filter::Nested {
            field: key.clone(),
            keys,
        };
        lists.push(vec![field()]);
        lists.push(vec![nested(probe())]);
        lists.push(vec![nested(Vec::new())]);
        lists.push(vec![nested(probe()), field()]);
        lists.push(vec![field(), nested(probe())]);
        if depth > 0
            && let Some(child) = javascript_property(value, key)
        {
            for list in probing_filter_lists(&child, depth - 1) {
                lists.push(vec![nested(list)]);
            }
        }
    }
    lists.push(keys.iter().map(|key| Filter::Field(key.clone())).collect());
    lists
}

/// Projects `value` both from the full reference serialization and from the
/// selected direct one and requires identical trees, which also tells
/// nonfinite values from `null`.
fn assert_selection_preserves_projection<T: Serialize>(value: &T, depth: usize) -> usize {
    let full = serialize_projection_value(value).unwrap();
    let lists = probing_filter_lists(&full, depth);
    for filters in &lists {
        let selected = serialize_selected(value, &Selected::from_filters(filters)).unwrap();
        assert_eq!(
            canonicalize_javascript_keys(project_filters(&selected, filters)),
            canonicalize_javascript_keys(project_filters(&full, filters)),
            "{}",
            serde_json::to_string(filters).unwrap()
        );
    }
    lists.len()
}

#[test]
fn selected_serialization_projects_like_the_full_document_for_every_serde_shape() {
    use std::collections::BTreeMap;

    #[derive(Serialize)]
    struct Newtype(Option<f64>);
    #[derive(Serialize)]
    struct TupleStruct(f64, String);
    #[derive(Serialize)]
    enum Variant {
        Unit,
        Newtype(f64),
        Tuple(f64, String),
        Struct { value: f64, list: Vec<f64> },
    }
    #[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
    enum VariantKey {
        First,
        Second,
    }
    #[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
    struct NewtypeKey(String);
    struct RawBytes(Vec<u8>);
    impl Serialize for RawBytes {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(&self.0)
        }
    }
    /// A struct repeating a field name: JSON keeps the first position and
    /// the last value.
    struct Repeated(f64, f64);
    impl Serialize for Repeated {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeStruct as _;
            let mut object = serializer.serialize_struct("Repeated", 3)?;
            object.serialize_field("dup", &self.0)?;
            object.serialize_field("other", &1)?;
            object.serialize_field("dup", &self.1)?;
            object.end()
        }
    }
    #[derive(Serialize)]
    struct Adversarial {
        #[serde(rename = "0")]
        zero: Vec<Vec<f64>>,
        #[serde(rename = "10")]
        ten: Newtype,
        length: f32,
        text: String,
        tuple: (f64, &'static str),
        tuple_struct: TupleStruct,
        unit: (),
        none: Option<Vec<f64>>,
        some: Option<Vec<f64>>,
        strings: indexmap::IndexMap<String, f64>,
        integers: BTreeMap<u32, f64>,
        nested_integers: BTreeMap<i64, BTreeMap<String, Vec<f64>>>,
        chars: BTreeMap<char, f64>,
        bools: BTreeMap<bool, f64>,
        variant_keys: BTreeMap<VariantKey, BTreeMap<String, f64>>,
        newtype_keys: BTreeMap<NewtypeKey, f64>,
        variants: Vec<Variant>,
        bytes: RawBytes,
        repeated: Vec<Repeated>,
        raw: Box<serde_json::value::RawValue>,
        json: serde_json::Value,
    }

    let value = Adversarial {
        zero: vec![vec![1.0, f64::NAN], Vec::new(), vec![f64::INFINITY]],
        ten: Newtype(Some(f64::NAN)),
        length: f32::NAN,
        text: "😀a\u{0}".into(),
        tuple: (f64::NEG_INFINITY, "t"),
        tuple_struct: TupleStruct(f64::NAN, "u".into()),
        unit: (),
        none: None,
        some: Some(vec![f64::NAN, 2.0, 3.0, 4.0, 5.0]),
        strings: [("a", f64::NAN), ("0", 1.5), ("length", 2.0)]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
        integers: [(3, f64::NAN), (0, 1.0)].into_iter().collect(),
        nested_integers: [(-1, [("n".to_owned(), vec![f64::NAN])].into_iter().collect())]
            .into_iter()
            .collect(),
        chars: [('x', f64::NAN)].into_iter().collect(),
        bools: [(true, f64::NAN)].into_iter().collect(),
        variant_keys: [(
            VariantKey::Second,
            [("v".to_owned(), f64::INFINITY)].into_iter().collect(),
        )]
        .into_iter()
        .chain([(VariantKey::First, BTreeMap::new())])
        .collect(),
        newtype_keys: [(NewtypeKey("k".into()), f64::NAN)].into_iter().collect(),
        bytes: RawBytes(vec![0, 7, 255]),
        repeated: vec![
            Repeated(f64::NAN, 1.0),
            Repeated(1.0, f64::NAN),
            Repeated(f64::NAN, f64::NAN),
        ],
        variants: vec![
            Variant::Unit,
            Variant::Newtype(f64::NAN),
            Variant::Tuple(f64::INFINITY, "t".into()),
            Variant::Struct {
                value: f64::NAN,
                list: vec![f64::NAN, 1.0],
            },
        ],
        raw: serde_json::value::RawValue::from_string(
            r#"{"b":[1,2.5,{"c":null}],"0":"z","b":3}"#.into(),
        )
        .unwrap(),
        json: serde_json::json!({"k": [1, {"m": "n"}], "5": true, "1": null}),
    };
    assert!(assert_selection_preserves_projection(&value, 4) > 1_000);
}

#[test]
fn selected_serialization_projects_like_the_full_document_for_all_schemas() {
    use crate::model::states::{HitErrorArray, KeypressButton};
    use crate::state::{PreciseState, RegularState};

    let mut regular = RegularState {
        global: Default::default(),
        settings: Default::default(),
        beatmap: Default::default(),
        menu: Default::default(),
        bass_density: Default::default(),
        gameplay: Default::default(),
        result_screen: Default::default(),
        tourney: Default::default(),
        user: Default::default(),
        lazer_multi_spectating: Default::default(),
        ranked_play: Default::default(),
        room: Default::default(),
    };
    regular.menu.title = Some("狐 \"kitsune\"\n\\尾😀".into());
    let precise = PreciseState {
        status: 2,
        play_time: f64::NAN,
        mode: 0,
        key_overlay: Arc::new(Vec::new()),
        keypresses: Arc::new(
            (1..=4)
                .map(|index| KeypressButton {
                    name: Arc::from(format!("K{index}")),
                    is_pressed: index % 2 == 0,
                    count: index,
                    pressed_at: (index % 2 == 0).then_some(f64::INFINITY),
                    released_at: None,
                })
                .collect(),
        ),
        hit_errors: Arc::new(HitErrorArray {
            all: vec![1.0, f64::NAN, -2.5, f64::NEG_INFINITY, 7.0],
            press: vec![f64::NAN],
            release: Vec::new(),
        }),
        total_hit_errors: f64::INFINITY,
        committed_hit_error_index: 0,
        unstable_rate: f64::NAN,
    };
    let state = Arc::new(StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(regular),
        precise: Arc::new(precise),
    });
    let instance = |pid, ipc: Option<f64>| {
        ManagerInstanceSnapshot::new(
            pid,
            crate::instance::InstanceSettings {
                tournament_spectator: ipc.is_some(),
                tournament_ipc_id: ipc,
                devserver_endpoint: None,
            },
            ipc.is_none(),
            Arc::clone(&state),
        )
    };
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        2,
        Some(7),
        true,
        ApiRuntimeSettings::default(),
        [instance(7, None), instance(8, Some(f64::NAN)), instance(9, Some(2.5))],
    )));

    assert_selection_preserves_projection(&crate::api::v1::build(&snapshot).unwrap(), 3);
    assert_selection_preserves_projection(&crate::api::v2::build(&snapshot).unwrap(), 3);
    assert_selection_preserves_projection(&crate::api::sc::build(&snapshot).unwrap(), 3);
    assert_selection_preserves_projection(&crate::api::precise::build(&snapshot).unwrap(), 3);
}

