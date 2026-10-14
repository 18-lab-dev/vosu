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
        let bytes = encode_projection(&source, &filters).unwrap();
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
    let projected = encode_projection(
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
        projected.unwrap(),
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
    let projected = encode_projection(
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
        projected.unwrap(),
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
    let projected = encode_projection(
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
    assert_eq!(
        projected.unwrap(),
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
    let projected = encode_projection(
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
    );
    assert_eq!(
        projected.unwrap(),
        Bytes::from_static(
            br#"{"errors":{"1":2,"length":3},"label":{"0":"A","1":"\ud83d","2":"\udca9","3":"B","length":4},"surrogate":{"0":{"0":"\ud83d","length":1}}}"#,
        )
    );
}

#[test]
fn only_canonical_uint32_spellings_are_javascript_array_indices() {
    for (key, index) in [
        ("0", Some(0)),
        ("7", Some(7)),
        ("4294967294", Some(u32::MAX - 1)),
        ("4294967295", None),
        ("4294967296", None),
        ("01", None),
        ("00", None),
        ("+1", None),
        ("-1", None),
        (" 1", None),
        ("1 ", None),
        ("1e3", None),
        ("1.0", None),
        ("٣", None),
        ("", None),
    ] {
        assert_eq!(javascript_array_index(key), index, "{key:?}");
    }
}

#[test]
fn projected_strings_escape_exactly_like_serde_json() {
    for text in [
        "",
        "plain name",
        "\u{1f}",
        " ",
        "\u{7f}",
        "quote \" inside",
        "back\\slash",
        "slash / kept",
        "狐 tail 😀",
        "line\nbreak\ttab\r",
    ] {
        let projected = encode_projection(
            &ProjectionValue::Object(vec![(text.to_owned().into(), ProjectionValue::String(text.into()))]),
            &[Filter::Field(text.into())],
        )
        .unwrap();
        let quoted = serde_json::to_string(text).unwrap();
        assert_eq!(projected, format!("{{{quoted}:{quoted}}}"), "{text:?}");
    }
}

#[test]
fn projected_numbers_print_like_javascript() {
    let write = |number| encode_javascript_json(&ProjectionValue::Number(number)).unwrap();
    let largest_exact = 9_007_199_254_740_991.0;
    for value in [
        0.0,
        -0.0,
        1.0,
        -1.0,
        37.0,
        1.5,
        -2.5,
        largest_exact,
        -largest_exact,
        largest_exact + 1.0,
        largest_exact + 3.0,
        -(largest_exact + 3.0),
        1e20,
        1e21,
        -1e21,
        1e-7,
    ] {
        let number = serde_json::Number::from_f64(value).unwrap();
        assert_eq!(write(number), crate::calculation::js_json_number(value), "{value:?}");
    }
    for value in [i64::MIN, -1, 0, 1, i64::MAX] {
        assert_eq!(write(value.into()), value.to_string(), "{value}");
    }
    assert_eq!(write(u64::MAX.into()), u64::MAX.to_string());
}

#[test]
fn whole_objects_write_index_keys_ascending_before_other_keys_at_any_count() {
    for count in [1, 8, 9, 20] {
        // Descending indices interleaved with names, so neither insertion
        // nor lexicographic order is numeric order.
        let mut object = serde_json::Map::new();
        for index in (0..count).rev() {
            object.insert(index.to_string(), serde_json::json!(index));
            object.insert(format!("k{index}"), serde_json::json!(index));
        }
        let source =
            serialize_projection_value(&serde_json::json!({ "object": object })).unwrap();
        let indices = (0..count).map(|index| format!("\"{index}\":{index}"));
        let names = (0..count).rev().map(|index| format!("\"k{index}\":{index}"));
        let expected = format!(
            "{{\"object\":{{{}}}}}",
            indices.chain(names).collect::<Vec<_>>().join(",")
        );
        assert_eq!(
            encode_projection(&source, &[Filter::Field("object".into())]).unwrap(),
            Bytes::from(expected),
            "{count} index keys"
        );
    }
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
    let projected = encode_projection(
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
        projected.unwrap(),
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
    let projected = encode_projection(
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
        projected.unwrap(),
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
    let projected = encode_projection(
        &source,
        &[
            Filter::Field("ordinary".into()),
            Filter::Field("text".into()),
            Filter::Field("nonfinite".into()),
        ],
    );
    assert_eq!(
        projected.unwrap(),
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
        ProjectionValue::Object(entries) => entries.iter().map(|(key, _)| key.to_string()).collect(),
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
        // The last of repeated nested filters wins, so the selection must
        // cover the keys of every one, not just the first.
        let probed = probe();
        let (first, last) = probed.split_at(2);
        lists.push(vec![nested(first.to_vec()), nested(last.to_vec())]);
        lists.push(vec![nested(last.to_vec()), nested(first.to_vec())]);
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
/// nonfinite values from `null`, then requires the production writer to
/// produce exactly the bytes of the reference projection.
fn assert_selection_preserves_projection<T: Serialize>(value: &T, depth: usize) -> usize {
    let full = serialize_projection_value(value).unwrap();
    let lists = probing_filter_lists(&full, depth);
    for filters in &lists {
        let selected = serialize_selected(value, &Selected::from_filters(filters)).unwrap();
        let reference = canonicalize_javascript_keys(project_filters(&full, filters));
        assert_eq!(
            canonicalize_javascript_keys(project_filters(&selected, filters)),
            reference,
            "{}",
            serde_json::to_string(filters).unwrap()
        );
        assert_eq!(
            encode_projection(&selected, filters).unwrap(),
            encode_javascript_json(&reference).unwrap(),
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

/// A focused stable snapshot whose four documents contain strings, nonfinite
/// floats and tourney instances; `title` distinguishes publications.
fn all_schema_snapshot(title: &str) -> CombinedSnapshot {
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
    regular.menu.title = Some(title.into());
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
    CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        2,
        Some(7),
        true,
        ApiRuntimeSettings::default(),
        [instance(7, None), instance(8, Some(f64::NAN)), instance(9, Some(2.5))],
    )))
}

#[test]
fn selected_serialization_projects_like_the_full_document_for_all_schemas() {
    let snapshot = all_schema_snapshot("狐 \"kitsune\"\n\\尾😀");
    assert_selection_preserves_projection(&crate::api::v1::build(&snapshot).unwrap(), 3);
    assert_selection_preserves_projection(&crate::api::v2::build(&snapshot).unwrap(), 3);
    assert_selection_preserves_projection(&crate::api::sc::build(&snapshot).unwrap(), 3);
    assert_selection_preserves_projection(&crate::api::precise::build(&snapshot).unwrap(), 3);
}

#[test]
fn distinct_plans_in_one_pass_encode_as_they_do_alone_for_all_schemas() {
    use crate::server::websocket::BroadcastPass;

    let snapshot = all_schema_snapshot("狐 \"kitsune\"\n\\尾😀");
    for (schema, document) in [
        (ApiSchema::V1, serialize_projection_value(&crate::api::v1::build(&snapshot).unwrap())),
        (ApiSchema::V2, serialize_projection_value(&crate::api::v2::build(&snapshot).unwrap())),
        (
            ApiSchema::StreamCompanion,
            serialize_projection_value(&crate::api::sc::build(&snapshot).unwrap()),
        ),
        (
            ApiSchema::V2Precise,
            serialize_projection_value(&crate::api::precise::build(&snapshot).unwrap()),
        ),
    ] {
        // Repeated rounds keep reaching every property, so later plans in the
        // pass project from properties serialized whole once enough partial
        // walks have gone into them.
        let lists = probing_filter_lists(&document.unwrap(), 2);
        let encode = |filters: &[Filter]| {
            ProductionSnapshotEncoder
                .encode(&snapshot, schema, filters)
                .unwrap()
        };
        let alone: Vec<_> = lists.iter().map(|filters| encode(filters)).collect();
        let _pass = BroadcastPass::begin();
        for round in 0..4 {
            for (filters, alone) in lists.iter().zip(&alone) {
                assert_eq!(
                    &encode(filters),
                    alone,
                    "{schema:?} round {round}: {}",
                    serde_json::to_string(filters).unwrap()
                );
            }
        }
    }
}

#[test]
fn properties_are_shared_whole_only_once_their_walks_have_cost_as_much() {
    #[derive(Serialize)]
    struct Document {
        parent: Parent,
    }
    #[derive(Serialize)]
    struct Parent {
        small: u64,
        items: Vec<u64>,
    }

    let mut document = SharedDocument::new(Document {
        parent: Parent {
            small: 7,
            items: (0..10_000).collect(),
        },
    });
    let nested = |key: &str, keys| Filter::Nested {
        field: key.into(),
        keys,
    };
    // Each walk opens only `parent`, never the large array beside `small`.
    let small = [nested("parent", vec![Filter::Field("small".into())])];
    for _ in 0..1_000 {
        assert_eq!(
            document.encode(&small).unwrap(),
            Bytes::from_static(br#"{"parent":{"small":7}}"#)
        );
    }
    assert!(
        !document.is_shared("parent"),
        "cheap walks must not pay for serializing the large array whole"
    );

    // A walk into the array already iterates every element.
    let last = [nested(
        "parent",
        vec![nested("items", vec![Filter::Field("9999".into())])],
    )];
    for _ in 0..2 {
        assert_eq!(
            document.encode(&last).unwrap(),
            Bytes::from_static(br#"{"parent":{"items":{"9999":9999}}}"#)
        );
    }
    assert!(document.is_shared("parent"));
    assert_eq!(
        document.encode(&small).unwrap(),
        Bytes::from_static(br#"{"parent":{"small":7}}"#)
    );
}

#[test]
fn large_text_beside_a_cheap_selection_is_never_shared_whole() {
    use std::collections::BTreeMap;

    #[derive(Serialize)]
    struct Document<T> {
        parent: Parent<T>,
    }
    #[derive(Serialize)]
    struct Parent<T> {
        small: u64,
        large: T,
    }

    fn assert_not_shared<T: Serialize>(kind: &str, large: T) {
        let mut document = SharedDocument::new(Document {
            parent: Parent { small: 7, large },
        });
        let small = [Filter::Nested {
            field: "parent".into(),
            keys: vec![Filter::Field("small".into())],
        }];
        for _ in 0..1_000 {
            assert_eq!(
                document.encode(&small).unwrap(),
                Bytes::from_static(br#"{"parent":{"small":7}}"#),
                "{kind}"
            );
        }
        assert!(
            !document.is_shared("parent"),
            "cheap walks must not pay for copying the large {kind} whole"
        );
    }

    let text = "x".repeat(1 << 20);
    assert_not_shared("string", text.clone());
    assert_not_shared("map key", BTreeMap::from([(text, 0u8)]));
    let raw = format!("[{}0]", "0,".repeat(1 << 19));
    assert_not_shared(
        "raw value",
        serde_json::value::RawValue::from_string(raw).unwrap(),
    );
}

#[test]
fn broadcast_pass_reuses_documents_only_for_the_same_publication_and_schema() {
    use crate::server::websocket::BroadcastPass;

    let first = all_schema_snapshot("first");
    let second = all_schema_snapshot("second");
    // A distinct snapshot object over the same immutable publication.
    let first_again = CombinedSnapshot::from_manager(Arc::clone(first.manager()));
    let filters = [Filter::Field("beatmap".into()), Filter::Field("menu".into())];
    let schemas = [ApiSchema::V1, ApiSchema::V2];
    let encode = |snapshot: &CombinedSnapshot, schema| {
        ProductionSnapshotEncoder
            .encode(snapshot, schema, &filters)
            .unwrap()
    };
    // Encoded outside any pass, so every document is built afresh.
    let expected = |snapshot: &CombinedSnapshot| schemas.map(|schema| encode(snapshot, schema));
    let (first_expected, second_expected) = (expected(&first), expected(&second));
    for (first, second) in first_expected.iter().zip(&second_expected) {
        assert_ne!(first, second, "the publications must be distinguishable");
    }
    let references = Arc::strong_count(first.manager());

    let pass = BroadcastPass::begin();
    for (snapshot, expected) in [
        (&first, &first_expected),
        (&second, &second_expected),
        (&first_again, &first_expected),
        (&first, &first_expected),
    ] {
        for (schema, expected) in schemas.into_iter().zip(expected) {
            assert_eq!(&encode(snapshot, schema), expected, "{schema:?}");
        }
    }
    drop(pass);
    assert_eq!(
        Arc::strong_count(first.manager()),
        references,
        "nothing derived from a publication outlives its broadcast pass"
    );
}

