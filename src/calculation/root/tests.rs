use super::*;
use chrono::TimeZone;
use serde_json::json;
fn mod_(acronym: &str) -> Mod {
    Mod {
        acronym: acronym.into(),
        settings: None,
    }
}
fn stats(perfect: i64, great: i64, good: i64, ok: i64, meh: i64, miss: i64) -> Statistics {
    Statistics {
        perfect,
        great,
        good,
        ok,
        meh,
        miss,
        ..empty_statistics()
    }
}

#[test]
fn bit_table_aliases_and_source_order_match() {
    for (bit, expected) in BIT_VALUES.iter().enumerate() {
        assert_eq!(mods_name(1_i64 << bit, false), *expected);
    }
    assert_eq!(
        mods_name(osu_mods::DOUBLE_TIME | osu_mods::NIGHTCORE, false),
        "NC"
    );
    assert_eq!(
        mods_name(osu_mods::SUDDEN_DEATH | osu_mods::PERFECT, false),
        "PF"
    );
    assert_eq!(
        mods_name(osu_mods::AUTOPLAY | osu_mods::CINEMA, false),
        "CN"
    );
    assert_eq!(
        mods_name(osu_mods::HIDDEN | osu_mods::DOUBLE_TIME, true),
        "HDDT"
    );
    assert_eq!(
        mods_name(osu_mods::NO_FAIL | osu_mods::EASY | osu_mods::HIDDEN, true),
        "EZHDNF"
    );
    assert_eq!(mods_name(-1, false), "");
    assert_eq!(mods_name(i64::from(i32::MAX) + 1, false), "");
    let nc = calculate_mods(
        ModsInput::Number(osu_mods::DOUBLE_TIME | osu_mods::NIGHTCORE),
        false,
    )
    .unwrap();
    assert_eq!(
        (nc.number, nc.name.as_str(), nc.rate, nc.checksum.as_str()),
        (576, "NC", 1.5, "5a8e4c03754edab6654c46d466c47571")
    );
    assert_eq!(nc.array, vec![mod_("NC")]);
}

#[test]
fn every_legacy_name_and_structured_alias_maps_exactly() {
    let cases = [
        ("nf", 1, "NF"),
        ("ez", 2, "EZ"),
        ("td", 4, "TD"),
        ("hd", 8, "HD"),
        ("hr", 16, "HR"),
        ("sd", 32, "SD"),
        ("dt", 64, "DT"),
        ("rx", 128, "RX"),
        ("ht", 256, "HT"),
        ("nc", 576, "NC"),
        ("fl", 1024, "FL"),
        ("at", 2048, "AT"),
        ("so", 4096, "SO"),
        ("ap", 8192, "AP"),
        ("pf", 16416, "PF"),
        ("4k", 32768, "4K"),
        ("5k", 65536, "5K"),
        ("6k", 131072, "6K"),
        ("7k", 262144, "7K"),
        ("8k", 524288, "8K"),
        ("fi", 1048576, "FI"),
        ("rd", 2097152, "RD"),
        ("cn", 4194304, "CN"),
        ("target", 8388608, "TG"),
        ("9k", 16777216, "9K"),
        ("keycoop", 33554432, "10K"),
        ("1k", 67108864, "1K"),
        ("3k", 134217728, "3K"),
        ("2k", 268435456, "2K"),
        ("scorev2", 536870912, "v2"),
        ("mr", 1073741824, "MR"),
    ];
    for (input, number, name) in cases {
        let result = calculate_mods(ModsInput::Structured(vec![mod_(input)]), false).unwrap();
        assert_eq!(
            (result.number, result.name.as_str()),
            (number, name),
            "{input}"
        );
        assert_eq!(result.array[0].acronym, input);
    }
    let unknown = calculate_mods(ModsInput::Structured(vec![mod_("ZZ")]), true).unwrap();
    assert_eq!((unknown.number, unknown.name.as_str()), (0, "ZZ"));
    assert_eq!(
        calculate_mods(ModsInput::Name(String::new()), false).unwrap(),
        default_calculated_mods()
    );
    assert_eq!(
        calculate_mods_optional(None, false),
        Err(ModsError::Missing)
    );
}

#[test]
fn calculate_mods_preserves_mutation_coercion_and_rate_precedence() {
    let mut speed = IndexMap::new();
    speed.insert("speed_change".into(), json!(1.249));
    speed.insert("adjust_pitch".into(), json!(false));
    let result = calculate_mods(
        ModsInput::Structured(vec![
            Mod {
                acronym: "DT".into(),
                settings: Some(speed),
            },
            mod_("HT"),
        ]),
        false,
    )
    .unwrap();
    assert_eq!(result.number, 320);
    assert_eq!(result.name, "DTHT");
    assert_eq!(result.rate, 1.25);
    assert_eq!(
        result.array[0].settings.as_ref().unwrap()["speed_change"],
        json!(1.25)
    );

    let mut zero = IndexMap::new();
    zero.insert("speed_change".into(), json!(0));
    let result = calculate_mods(
        ModsInput::Structured(vec![
            Mod {
                acronym: "ZZ".into(),
                settings: Some(zero),
            },
            mod_("DT"),
        ]),
        false,
    )
    .unwrap();
    assert_eq!(result.rate, 1.5);

    let mut strings = IndexMap::new();
    strings.insert("accuracy_judge_mode".into(), json!(2));
    strings.insert("reflection".into(), json!(true));
    strings.insert("direction".into(), Value::Null);
    strings.insert("style".into(), json!(["a", 2]));
    let result = calculate_mods(
        ModsInput::Structured(vec![Mod {
            acronym: "AC".into(),
            settings: Some(strings),
        }]),
        false,
    )
    .unwrap();
    let settings = result.array[0].settings.as_ref().unwrap();
    assert_eq!(settings["accuracy_judge_mode"], "2");
    assert_eq!(settings["reflection"], "true");
    assert_eq!(settings["direction"], "null");
    assert_eq!(settings["style"], "a,2");
}

#[test]
fn checksum_bytes_match_json_stringify_goldens() {
    assert_eq!(mods_checksum_bytes(&[]), b"[]");
    assert_eq!(mods_checksum(&[]), "d751713988987e9331980363e24189ce");
    assert_eq!(
        mods_checksum(&[mod_("HD")]),
        "507e8cb23289620636c1396b149d01d2"
    );

    let mut settings = IndexMap::new();
    settings.insert("speed_change".into(), json!(1.25));
    settings.insert("adjust_pitch".into(), json!(false));
    let mods = vec![Mod {
        acronym: "DT".into(),
        settings: Some(settings),
    }];
    assert_eq!(
        mods_checksum_bytes(&mods),
        br#"[{"acronym":"DT","settings":{"speed_change":1.25,"adjust_pitch":false}}]"#
    );
    assert_eq!(mods_checksum(&mods), "923c978c8e45affb2e74a2dde16a6557");

    let mut numeric_keys = IndexMap::new();
    numeric_keys.insert("z".into(), json!(2));
    numeric_keys.insert("10".into(), json!("ten"));
    numeric_keys.insert("2".into(), json!("two"));
    numeric_keys.insert("a".into(), json!(1));
    numeric_keys.insert("reflection".into(), json!(3));
    let result = calculate_mods(
        ModsInput::Structured(vec![Mod {
            acronym: "ZZ".into(),
            settings: Some(numeric_keys),
        }]),
        false,
    )
    .unwrap();
    assert_eq!(
        mods_checksum_bytes(&result.array),
        br#"[{"acronym":"ZZ","settings":{"2":"two","10":"ten","z":2,"a":1,"reflection":"3"}}]"#
    );
    assert_eq!(result.checksum, "ef037ba3d3cb9e5c48398bf9830c0ef6");
    assert_eq!(js_to_string(&json!([null, "x", [null, 1]])), ",x,,1");
    let mut integer = IndexMap::new();
    integer.insert("value".into(), json!(1_000_000_000_000_000_128_u64));
    let integer_mod = Mod {
        acronym: "ZZ".into(),
        settings: Some(integer),
    };
    assert_eq!(
        mods_checksum_bytes(&[integer_mod]),
        br#"[{"acronym":"ZZ","settings":{"value":1000000000000000100}}]"#
    );
}

#[test]
fn sanitization_and_classic_insertion_are_source_ordered() {
    let mods = vec![mod_("RX"), mod_("HD"), mod_("v2"), mod_("AP"), mod_("V2")];
    assert_eq!(
        sanitize_mods(&mods)
            .iter()
            .map(|m| m.acronym.as_str())
            .collect::<Vec<_>>(),
        ["HD", "SV2", "V2"]
    );
    let stable = mods_for_calculation(&mods, false);
    assert_eq!(
        stable
            .iter()
            .map(|m| m.acronym.as_str())
            .collect::<Vec<_>>(),
        ["HD", "SV2", "V2", "CL"]
    );
    assert_eq!(stable.last().unwrap().settings, Some(IndexMap::new()));
    assert_eq!(mods_for_calculation(&mods, true).len(), 3);
}

#[test]
fn category_and_setting_tables_cover_the_source_union() {
    assert_eq!(ALL_MOD_ACRONYMS.len(), 69);
    for acronym in ALL_MOD_ACRONYMS {
        assert!(!acronym.is_empty());
    }
    assert_eq!(
        mod_categories(0).unwrap().automation,
        ["AT", "CN", "RX", "AP", "SO"]
    );
    assert_eq!(
        mod_categories(1).unwrap().difficulty_reduction.last(),
        Some(&"SR")
    );
    assert_eq!(
        mod_categories(2).unwrap().fun,
        ["WU", "WD", "FF", "MU", "NS", "MF"]
    );
    assert!(mod_categories(3).unwrap().conversion.contains(&"10K"));
    assert!(mod_categories(4).is_none());
    assert_eq!(
        mod_setting_keys("DA"),
        [
            "circle_size",
            "approach_rate",
            "drain_rate",
            "overall_difficulty",
            "extended_limits",
            "hard_rock_offsets"
        ]
    );
    assert_eq!(mod_setting_keys("SR").len(), 3);
    assert!(mod_setting_keys("NF").is_empty());
}

#[test]
fn accuracy_covers_all_modes_and_mania_client_weights() {
    assert_eq!(
        calculate_accuracy(false, &[], 0, &stats(0, 3, 0, 2, 1, 1)),
        54.76
    );
    assert_eq!(
        calculate_accuracy(false, &[], 1, &stats(0, 3, 0, 2, 99, 1)),
        66.67
    );
    assert_eq!(
        calculate_accuracy(false, &[], 2, &stats(0, 3, 4, 2, 1, 1)),
        54.55
    );
    let mania = stats(1, 1, 1, 1, 1, 1);
    assert_eq!(calculate_accuracy(false, &[], 3, &mania), 52.78);
    assert_eq!(calculate_accuracy(true, &[], 3, &mania), 52.19);
    assert_eq!(calculate_accuracy(false, &[mod_("V2")], 3, &mania), 52.19);
    for mode in -1..=4 {
        assert_eq!(
            calculate_accuracy(false, &[], mode, &empty_statistics()),
            0.0
        );
    }
}

#[test]
fn lazer_grade_boundaries_are_inclusive_and_mode_specific() {
    let no_miss = empty_statistics();
    let miss = stats(0, 0, 0, 0, 0, 1);
    let vectors = [
        (0, 100.0, "X"),
        (0, 95.0, "S"),
        (0, 90.0, "A"),
        (0, 80.0, "B"),
        (0, 70.0, "C"),
        (0, 69.99, "D"),
        (1, 95.0, "S"),
        (2, 98.0, "S"),
        (2, 94.0, "A"),
        (2, 90.0, "B"),
        (2, 85.0, "C"),
        (2, 84.99, "D"),
        (3, 95.0, "S"),
        (3, 90.0, "A"),
        (3, 80.0, "B"),
        (3, 70.0, "C"),
    ];
    for (mode, accuracy, expected) in vectors {
        assert_eq!(
            calculate_grade(true, &[], mode, accuracy, &no_miss),
            expected
        );
    }
    assert_eq!(calculate_grade(true, &[], 0, 95.0, &miss), "A");
    assert_eq!(
        calculate_grade(true, &[mod_("HD")], 3, 100.0, &no_miss),
        "XH"
    );
    assert_eq!(
        calculate_grade(true, &[mod_("FL")], 2, 98.0, &no_miss),
        "SH"
    );
    assert_eq!(calculate_grade(true, &[], 9, 100.0, &no_miss), "");
}

#[test]
fn stable_grade_boundaries_are_strict() {
    let empty = empty_statistics();
    assert_eq!(calculate_grade(false, &[], 0, 0.0, &empty), "X");
    assert_eq!(calculate_grade(false, &[mod_("HD")], 1, 0.0, &empty), "XH");
    assert_eq!(calculate_grade(false, &[], 2, 98.0, &empty), "A");
    assert_eq!(calculate_grade(false, &[], 2, 98.01, &empty), "S");
    assert_eq!(calculate_grade(false, &[], 2, 94.0, &empty), "B");
    assert_eq!(calculate_grade(false, &[], 2, 90.0, &empty), "C");
    assert_eq!(calculate_grade(false, &[], 2, 85.0, &empty), "D");
    assert_eq!(calculate_grade(false, &[], 3, 95.0, &empty), "A");
    assert_eq!(calculate_grade(false, &[], 3, 90.0, &empty), "B");
    assert_eq!(calculate_grade(false, &[], 3, 80.0, &empty), "C");
    assert_eq!(calculate_grade(false, &[], 3, 70.0, &empty), "D");
    assert_eq!(
        calculate_grade(false, &[], 0, 0.0, &stats(0, 91, 0, 8, 0, 1)),
        "A"
    );
    assert_eq!(
        calculate_grade(false, &[], 0, 0.0, &stats(0, 90, 0, 9, 1, 0)),
        "A"
    );
    assert_eq!(
        calculate_grade(false, &[], 0, 0.0, &stats(0, 90, 0, 10, 0, 0)),
        "A"
    );
    for mode in [0, 1] {
        assert_eq!(
            calculate_grade(false, &[], mode, 0.0, &stats(0, 100, 0, 0, 0, 0)),
            "X"
        );
        assert_eq!(
            calculate_grade(false, &[mod_("FL")], mode, 0.0, &stats(0, 91, 0, 9, 0, 0)),
            "SH"
        );
        assert_eq!(
            calculate_grade(false, &[], mode, 0.0, &stats(0, 91, 0, 8, 1, 0)),
            "A"
        );
        assert_eq!(
            calculate_grade(false, &[], mode, 0.0, &stats(0, 80, 0, 20, 0, 0)),
            "B"
        );
        assert_eq!(
            calculate_grade(false, &[], mode, 0.0, &stats(0, 61, 0, 0, 0, 39)),
            "C"
        );
        assert_eq!(
            calculate_grade(false, &[], mode, 0.0, &stats(0, 60, 0, 0, 0, 40)),
            "D"
        );
    }
}

#[test]
fn legacy_hit_conversion_covers_every_mode_branch() {
    let input = LegacyHits {
        n0: 1,
        n50: 2,
        n100: 3,
        n300: 4,
        geki: 5,
        katu: 6,
    };
    let osu = from_legacy_hit_results(0, input);
    assert_eq!((osu.great, osu.ok, osu.meh, osu.miss), (4, 3, 2, 1));
    let taiko = from_legacy_hit_results(1, input);
    assert_eq!(
        (taiko.great, taiko.ok, taiko.large_bonus, taiko.meh),
        (4, 3, 11, 0)
    );
    let catch = from_legacy_hit_results(2, input);
    assert_eq!(
        (
            catch.great,
            catch.large_tick_hit,
            catch.small_tick_hit,
            catch.small_tick_miss
        ),
        (4, 3, 2, 6)
    );
    let mania = from_legacy_hit_results(3, input);
    assert_eq!(
        (mania.perfect, mania.great, mania.good, mania.ok, mania.meh),
        (5, 4, 6, 3, 2)
    );
    let catch_back = to_legacy_hits(
        2,
        &Statistics {
            large_tick_miss: 7,
            slider_tail_hit: 8,
            ..catch
        },
    );
    assert_eq!(
        catch_back,
        LegacyHitsExtended {
            n0: 8,
            n50: 2,
            n100: 3,
            n300: 4,
            geki: 0,
            katu: 6,
            slider_end_hits: 8,
            small_tick_hits: 2,
            large_tick_hits: 3
        }
    );
    assert_eq!(to_legacy_hits(1, &taiko).geki, 11);
    assert_eq!(to_legacy_hits(1, &taiko).katu, 11);
    assert_eq!(to_legacy_hits(99, &mania).n300, 4);
}

#[test]
fn decimal_fixing_matches_javascript_edge_behavior() {
    assert_eq!(fix_decimals(4.123123132131), 4.12);
    assert_eq!(fix_decimals(1.005), 1.0);
    assert_eq!(fix_decimals(2.675), 2.67);
    assert_eq!(fix_decimals(1.125), 1.13);
    assert_eq!(fix_decimals(-1.125), -1.13);
    assert_eq!(fix_decimals(f64::NAN), 0.0);
    assert!(!fix_decimals(-0.0).is_sign_negative());
    assert!(fix_decimals(-0.004).is_sign_negative());
    assert_eq!(fix_decimals(f64::INFINITY), f64::INFINITY);
    assert!(fix_decimals_to(1.0, 101).is_err());
    assert_eq!(js_json_number(-0.0), "0");
    assert_eq!(js_json_number(f64::NAN), "null");
    assert_eq!(js_json_number(f64::INFINITY), "null");
    assert_eq!(js_json_number(1e21), "1e+21");
    assert_eq!(
        js_json_number(1_000_000_000_000_000_128.0),
        "1000000000000000100"
    );
    assert_eq!(js_json_number(1e20), "100000000000000000000");
    assert_eq!(js_json_number(1e-6), "0.000001");
    assert_eq!(js_json_number(1e-7), "1e-7");
    assert_eq!(js_json_number(-1e-7), "-1e-7");
    assert_eq!(fix_decimals(1e21), 1e21);
    assert_eq!(
        fix_decimals(999_999_999_999_999_900.0),
        999_999_999_999_999_900.0
    );
}

#[test]
fn allocation_free_json_number_writer_matches_ecmascript_boundaries() {
    let cases = [
        (0.0, "0"),
        (-0.0, "0"),
        (f64::NAN, "null"),
        (f64::INFINITY, "null"),
        (f64::NEG_INFINITY, "null"),
        (f64::MIN_POSITIVE, "2.2250738585072014e-308"),
        (-1.0, "-1"),
        (1.0, "1"),
        (42.0, "42"),
        (f64::from_bits(1), "5e-324"),
        (f64::MAX, "1.7976931348623157e+308"),
        (1e-7, "1e-7"),
        (1e-6, "0.000001"),
        (1e20, "100000000000000000000"),
        (1e21, "1e+21"),
        (1_000_000_000_000_000_128.0, "1000000000000000100"),
        (-1e20, "-100000000000000000000"),
    ];

    for (value, expected) in cases {
        let mut output = Vec::new();
        write_js_json_number(&mut output, value).expect("write JSON number");
        assert_eq!(output, expected.as_bytes(), "value {value:?}");
        assert_eq!(js_json_number(value), expected, "value {value:?}");
    }
}

#[test]
fn dotnet_date_masks_kind_bits_and_truncates_to_milliseconds() {
    let expected = Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap();
    let ticks = (expected.timestamp_millis() as i128 * 10_000
        + 621_355_968_000_000_000_i128
        + 9_999) as u64;
    let low = ticks as u32 as i32;
    let high = ((ticks >> 32) as u32 | 0xc000_0000) as i32;
    assert_eq!(net_date_binary_to_date(high, low), Some(expected));
    let epoch_ticks = 621_355_968_000_000_000_u64 - 9_999;
    assert_eq!(
        net_date_binary_to_date((epoch_ticks >> 32) as i32, epoch_ticks as u32 as i32)
            .unwrap()
            .timestamp_millis(),
        0
    );
}

#[test]
fn dotnet_decimal_vectors_cover_scale_sign_and_invalid_scale() {
    assert_eq!(number_from_decimal(12345.0, 0, 2 << 16), 123.45);
    assert_eq!(
        number_from_decimal(12345.0, 0, (2 << 16) | i32::MIN),
        -123.45
    );
    assert_eq!(number_from_decimal(0.0, 1, 0), 1.8446744073709552e19);
    assert_eq!(number_from_decimal(0.0, 0, 0), 0.0);
    assert!(number_from_decimal(1.0, 0, 81 << 16).is_nan());
}

#[test]
fn safe_join_matches_node_normalization_edges() {
    assert_eq!(safe_join_with_separator(["", ""], '/'), ".");
    assert_eq!(
        safe_join_with_separator([" /foo\\bar ", " ../baz "], '/'),
        "/baz"
    );
    assert_eq!(
        safe_join_with_separator([" /foo ", " bar\\baz "], '/'),
        "/foo/bar\\baz"
    );
    assert_eq!(safe_join_with_separator(["/foo", "/bar"], '/'), "/foo/bar");
    assert_eq!(safe_join_with_separator(["/", ".."], '/'), "/");
    assert_eq!(safe_join_with_separator(["foo/", "bar/"], '/'), "foo/bar/");
    assert_eq!(
        safe_join_with_separator(["foo", "../../bar"], '/'),
        "../bar"
    );
    assert_eq!(
        safe_join_with_separator(["foo\\bar", ".\\baz"], '\\'),
        "foo\\bar\\baz"
    );
    assert_eq!(
        safe_join_with_separator(["C:foo", "bar"], '\\'),
        "C:foo\\bar"
    );
    assert_eq!(safe_join_with_separator(["C:foo", ".."], '\\'), "C:.");
    assert_eq!(safe_join_with_separator(["C:", "foo"], '\\'), "C:\\foo");
    assert_eq!(safe_join_with_separator(["C:", ""], '\\'), "C:.");
    assert_eq!(safe_join_with_separator(["1:\\foo", ".."], '\\'), ".\\1:");
    assert_eq!(safe_join_with_separator(["foo", "./"], '\\'), "foo\\");
    assert_eq!(
        safe_join_with_separator(["\u{feff} foo \u{feff}", "bar"], '\\'),
        "foo\\bar"
    );
    assert_eq!(
        safe_join_with_separator(["foo:", "bar"], '\\'),
        ".\\foo:\\bar"
    );
    assert_eq!(
        safe_join_with_separator(["CON:device"], '\\'),
        ".\\CON:device"
    );
}
