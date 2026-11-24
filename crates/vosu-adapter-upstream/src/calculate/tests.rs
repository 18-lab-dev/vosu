use super::*;
#[test]
fn pp_query_maps_every_documented_knob() {
    let query = PpQuery {
        path: None,
        lazer: Some(true),
        mode: Some(3),
        ar: Some(9.1),
        cs: Some(4.2),
        hp: Some(6.3),
        od: Some(8.4),
        clock_rate: Some(1.25),
        passed_objects: Some(99),
        combo: Some(88),
        n_misses: Some(1),
        n100: Some(2),
        n300: Some(3),
        n50: Some(4),
        n_geki: Some(5),
        n_katu: Some(6),
        mods: Some(ModsInput::Acronyms("HD, DT".into())),
        accuracy: Some(97.5),
        slider_end_hits: Some(7),
        small_tick_hits: Some(8),
        large_tick_hits: Some(9),
        hitresult_priority: Some(1),
    };
    let request = performance_request(&query).unwrap();
    assert_eq!(request.mode, Some(vosu_osu_calc::engine::Ruleset::Mania));
    assert_eq!(request.lazer, Some(true));
    assert_eq!(request.clock_rate, Some(1.25));
    assert_eq!(
        (request.ar, request.cs, request.hp, request.od),
        (Some(9.1), Some(4.2), Some(6.3), Some(8.4))
    );
    assert_eq!(
        (request.passed_objects, request.combo),
        (Some(99), Some(88))
    );
    assert_eq!(
        (request.n_misses, request.n100, request.n300, request.n50),
        (Some(1), Some(2), Some(3), Some(4))
    );
    assert_eq!((request.n_geki, request.n_katu), (Some(5), Some(6)));
    assert_eq!(
        (
            request.slider_end_hits,
            request.small_tick_hits,
            request.large_tick_hits
        ),
        (Some(7), Some(8), Some(9))
    );
    assert_eq!(request.accuracy, Some(97.5));
    assert_eq!(request.hitresult_priority, HitResultPriority::WorstCase);
    let LegacyMods::Structured(mods) = request.mods else {
        panic!("expected structured acronyms")
    };
    assert_eq!(
        mods.iter()
            .map(|item| item.acronym.as_str())
            .collect::<Vec<_>>(),
        ["HD", "DT"]
    );
}

#[test]
fn query_parser_preserves_js_counts_and_mod_shapes() {
    let q = PpQuery::parse(Some(
        "combo=0x10&n100=2e1&mods=%5B%7B%22acronym%22%3A%22HD%22%7D%5D&unknown=ignored",
    ))
    .unwrap();
    assert_eq!(q.combo, Some(16));
    assert_eq!(q.n100, Some(20));
    assert!(matches!(q.mods, Some(ModsInput::Json(Value::Array(_)))));
    for query in [
        "combo=-1",
        "combo=1.5",
        "combo=4294967296",
        "combo=NaN",
        "mods=%5B",
    ] {
        assert!(PpQuery::parse(Some(query)).is_err(), "{query}");
    }
    for q in [
        PpQuery {
            clock_rate: Some(0.0),
            ..Default::default()
        },
        PpQuery {
            mode: Some(4),
            ..Default::default()
        },
        PpQuery {
            hitresult_priority: Some(2),
            ..Default::default()
        },
        PpQuery {
            mods: Some(ModsInput::Acronyms("XYZ".into())),
            ..Default::default()
        },
    ] {
        assert!(performance_request(&q).is_err());
    }
}
