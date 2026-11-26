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

#[test]
fn parses_every_pp_input_and_both_structured_mod_shapes() {
    let raw = "path=%2Ftmp%2Fa.osu&lazer=true&mode=3&ar=9.1&cs=4&hp=6&od=8&clockRate=1.5&passedObjects=10&combo=20&nMisses=1&n100=2&n300=3&n50=4&nGeki=5&nKatu=6&mods=%5B%7B%22acronym%22%3A%22DT%22%7D%5D&acc=98.5&sliderEndHits=7&smallTickHits=8&largeTickHits=9&hitresultPriority=1";
    let q = PpQuery::parse(Some(raw)).unwrap();
    assert_eq!(q.path.as_deref(), Some("/tmp/a.osu"));
    assert_eq!(q.lazer, Some(true));
    assert_eq!(q.mode, Some(3));
    assert_eq!(
        (q.ar, q.cs, q.hp, q.od, q.clock_rate),
        (Some(9.1), Some(4.0), Some(6.0), Some(8.0), Some(1.5))
    );
    assert_eq!(
        (
            q.passed_objects,
            q.combo,
            q.n_misses,
            q.n100,
            q.n300,
            q.n50,
            q.n_geki,
            q.n_katu
        ),
        (
            Some(10),
            Some(20),
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(6)
        )
    );
    assert!(matches!(q.mods, Some(ModsInput::Json(Value::Array(_)))));
    assert_eq!(q.accuracy, Some(98.5));
    assert_eq!(
        (q.slider_end_hits, q.small_tick_hits, q.large_tick_hits),
        (Some(7), Some(8), Some(9))
    );
    assert_eq!(q.hitresult_priority, Some(1));
    assert!(matches!(
        PpQuery::parse(Some("mods=%7B%22acronym%22%3A%22HD%22%7D"))
            .unwrap()
            .mods,
        Some(ModsInput::Json(Value::Object(_)))
    ));
}

#[test]
fn count_inputs_use_bounded_javascript_numeric_coercion() {
    let query = PpQuery::parse(Some(
        "passedObjects=10.0&combo=1e2&nMisses=%2B3&n100=0x10&n300=0b11&n50=0o7&nGeki=&nKatu=4.0&sliderEndHits=5e1&smallTickHits=6.0&largeTickHits=7",
    )).unwrap();
    assert_eq!(
        (
            query.passed_objects,
            query.combo,
            query.n_misses,
            query.n100,
            query.n300,
            query.n50
        ),
        (Some(10), Some(100), Some(3), Some(16), Some(3), Some(7)),
    );
    assert_eq!(
        (
            query.n_geki,
            query.n_katu,
            query.slider_end_hits,
            query.small_tick_hits,
            query.large_tick_hits
        ),
        (Some(0), Some(4), Some(50), Some(6), Some(7)),
    );

    for value in ["10.5", "-1", "NaN", "Infinity", "4294967296", "1e100"] {
        assert!(
            PpQuery::parse(Some(&format!("combo={value}"))).is_err(),
            "{value}"
        );
    }
    assert_eq!(
        PpQuery::parse(Some("combo=4294967295")).unwrap().combo,
        Some(u32::MAX),
    );
}
