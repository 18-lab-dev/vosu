use super::*;
use proptest::prelude::*;

fn paths(s: &StateBundleSnapshot) -> Value {
    let mut out = Collect::default();
    super::super::super::internal::project(s, &facts(), &mut out);
    out.0.remove("/internal/paths").unwrap()
}

fn typed_primary(s: &StateBundleSnapshot) -> Arc<[f64]> {
    let mut last = super::super::super::LastValues::default();
    let mut writer = PublishBatchWriter::new();
    writer.begin(1);
    super::super::super::performance::project(
        s,
        &mut super::super::super::DiffSink::new(&mut last, &mut writer),
    );
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    let batch = decode_publish_batch_ref(&bytes).unwrap();
    let graph = batch
        .entries()
        .find(|e| e.pointer == "/performance/graph")
        .unwrap();
    let V::Cbor(encoded) = graph.value else {
        panic!("graph must be a typed CBOR record")
    };
    let graph: vosu_osu_model::source::PerformanceGraph = vosu_plugin_api::decode(encoded).unwrap();
    let vosu_osu_model::source::Selected::Value(primary) = graph.primary else {
        panic!("published primary must be available")
    };
    primary
}

#[test]
fn current_names_preserve_every_absence_combination_and_empty_strings() {
    let mut s = fixture();
    for mask in 0_u8..16 {
        let m = &mut Arc::make_mut(&mut s.regular).menu;
        m.folder = (mask & 1 != 0).then(|| "folder".into());
        m.filename = (mask & 2 != 0).then(|| "map.osu".into());
        m.background_filename = (mask & 4 != 0).then(|| "背景.png".into());
        m.audio_filename = (mask & 8 != 0).then(String::new);
        let expected = json!({"beatmapFolder":m.folder,"beatmapFile":m.filename,"background":m.background_filename,"audio":m.audio_filename});
        let p = paths(&s);
        if mask == 0 {
            assert!(p["current"].is_null());
        } else {
            assert_eq!(p["current"], expected);
        }
    }
}

#[test]
fn primary_history_preserves_numeric_boundaries_and_is_not_first_series() {
    let mut s = fixture();
    let graph = Arc::make_mut(&mut Arc::make_mut(&mut s.regular).beatmap.graph);
    graph.primary = vec![
        -0.0,
        f64::from_bits(1),
        f64::MIN_POSITIVE,
        0.007,
        9007199254740992.0,
        f64::MAX,
    ];
    graph.series.push(vosu_osu_calc::beatmap::GraphSeries {
        name: "different".into(),
        data: vec![123.0],
    });
    let d = collect(&s);
    let actual = d["/performance/graph"]["primary"].as_array().unwrap();
    for (actual, expected) in actual.iter().zip(&s.regular.beatmap.graph.primary) {
        assert_eq!(actual.as_f64().unwrap().to_bits(), expected.to_bits());
    }
    assert_eq!(actual.len(), 6);
    assert_eq!(d["/performance/graph"]["series"][0]["data"], json!([123.0]));
    let typed: vosu_osu_model::source::PerformanceGraph =
        serde_json::from_value(d["/performance/graph"].clone()).unwrap();
    let vosu_osu_model::source::Selected::Value(primary) = typed.primary else {
        panic!("published primary must be available")
    };
    assert_eq!(primary[0].to_bits(), (-0.0_f64).to_bits());
    Arc::make_mut(&mut Arc::make_mut(&mut s.regular).beatmap.graph).primary =
        vec![f64::NAN, f64::INFINITY, f64::NEG_INFINITY];
    let primary = typed_primary(&s);
    for (actual, expected) in primary.iter().zip(&s.regular.beatmap.graph.primary) {
        assert_eq!(actual.to_bits(), expected.to_bits());
    }
}

proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]
    #[test]
    fn prop_primary_preserves_finite_source_values(bits in proptest::collection::vec(any::<u64>(), 0..17)) {
        prop_assume!(bits.iter().all(|b| f64::from_bits(*b).is_finite()));
        let mut s = fixture();
        Arc::make_mut(&mut Arc::make_mut(&mut s.regular).beatmap.graph).primary = bits.iter().map(|b| f64::from_bits(*b)).collect();
        let typed = typed_primary(&s);
        prop_assert_eq!(typed.len(), bits.len());
        for (actual, expected) in typed.iter().zip(&bits) {
            prop_assert_eq!(actual.to_bits(), *expected);
        }
        let d = collect(&s);
        let actual = d["/performance/graph"]["primary"].as_array().unwrap();
        prop_assert_eq!(actual.len(), bits.len());
        for (actual, expected) in actual.iter().zip(bits) {
            prop_assert_eq!(actual.as_f64().unwrap().to_bits(), expected);
        }
    }
    #[test]
    fn prop_background_and_audio_do_not_depend_on_chart_names(background in proptest::option::of(".{0,32}"), audio in proptest::option::of(".{0,32}")) {
        let mut s = fixture();
        let m = &mut Arc::make_mut(&mut s.regular).menu;
        m.folder = None; m.filename = None;
        m.background_filename = background.clone(); m.audio_filename = audio.clone();
        let p = paths(&s);
        prop_assert_eq!(&p["current"]["background"], &json!(background));
        prop_assert_eq!(&p["current"]["audio"], &json!(audio));
        prop_assert!(p["current"]["beatmapFolder"].is_null());
        prop_assert!(p["current"]["beatmapFile"].is_null());
    }
}
