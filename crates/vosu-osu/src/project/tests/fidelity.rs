use super::*;
use proptest::prelude::*;
use vosu_osu_model::states::KeyOverlayButton;

#[test]
fn independent_overlay_slots_never_alias_logical_buttons_or_missing_slots() {
    let mut s = fixture();
    assert!(collect(&s)["/keys/overlay"].is_null());
    Arc::make_mut(&mut s.precise).key_overlay = Arc::new(
        [11, 22, 33, 44]
            .into_iter()
            .enumerate()
            .map(|(i, count)| KeyOverlayButton {
                name: Arc::from("not-a-logical-name"),
                is_pressed: i % 2 == 0,
                count,
            })
            .collect(),
    );
    let d = collect(&s);
    assert_eq!(d["/keys/all"][0]["count"], 19);
    for (name, count, pressed) in [
        ("k1", 11, true),
        ("k2", 22, false),
        ("m1", 33, true),
        ("m2", 44, false),
    ] {
        assert_eq!(
            d["/keys/overlay"][name],
            json!({"count":count,"isPressed":pressed})
        );
    }
    Arc::make_mut(&mut Arc::make_mut(&mut s.precise).key_overlay).truncate(1);
    let d = collect(&s);
    assert_eq!(d["/keys/overlay"]["k1"]["count"], 11);
    assert!(d["/keys/overlay"]["k2"].is_null());
    assert!(d["/keys/overlay"]["m1"].is_null());
    assert!(d["/keys/overlay"]["m2"].is_null());
}

#[test]
fn raw_gameplay_is_not_activity_gated_or_legacy_converted() {
    let mut s = fixture();
    let g = &mut Arc::make_mut(&mut s.regular).gameplay;
    g.score = 12345.5;
    g.combo = 23;
    g.max_combo = 47;
    g.retries = 9;
    g.accuracy = 98.123456789;
    g.player_hp = 153.123456789;
    g.player_hp_smooth = 111.25;
    g.statistics.miss = 1;
    g.statistics.meh = 2;
    g.statistics.ok = 3;
    g.statistics.good = 4;
    g.statistics.great = 5;
    g.statistics.perfect = 6;
    g.leaderboard_visible = true;
    g.leaderboard_player.name = "main-not-slot".into();
    g.leaderboard_player.score = 9876.0;
    for status in [0, 2, 7, 11] {
        Arc::make_mut(&mut s.precise).status = status;
        let d = collect(&s);
        assert_eq!(d["/play/score"], 12345.5);
        assert_eq!(d["/play/combo"], json!({"current":23,"max":47}));
        assert_eq!(d["/play/retries"], 9);
        assert_eq!(d["/play/accuracyPercent"], 98.123456789);
        assert_eq!(
            d["/play/healthRaw"],
            json!({"normal":153.123456789,"smooth":111.25})
        );
        assert_eq!(
            d["/play/judgements"],
            json!({"miss":1,"meh":2,"ok":3,"good":4,"great":5,"perfect":6})
        );
        assert_eq!(d["/play/preciseUnstableRate"], 999.0);
        assert_ne!(d["/play/unstableRate"], d["/play/preciseUnstableRate"]);
        assert_eq!(d["/leaderboardState"]["player"]["name"], "main-not-slot");
        assert_eq!(d["/leaderboardState"]["player"]["score"], 9876.0);
        assert_eq!(d["/leaderboardState"]["visible"], true);
        assert_eq!(d["/leaderboard"], json!([]));
    }
}

#[test]
fn raw_float_boundaries_preserve_bits_and_nonfinite_is_unavailable() {
    let mut s = fixture();
    for value in [
        -0.0,
        0.0,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        0.145,
        98.005,
        99.99999999999999,
        f64::MAX,
    ] {
        let g = &mut Arc::make_mut(&mut s.regular).gameplay;
        g.accuracy = value;
        g.player_hp = value;
        let d = collect(&s);
        assert_eq!(
            d["/play/accuracyPercent"].as_f64().unwrap().to_bits(),
            value.to_bits()
        );
        assert_eq!(
            d["/play/healthRaw"]["normal"].as_f64().unwrap().to_bits(),
            value.to_bits()
        );
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        Arc::make_mut(&mut s.regular).gameplay.accuracy = value;
        assert!(collect(&s)["/play/accuracyPercent"].is_null());
    }
    // A concrete counterexample forbids reconstructing the source percent.
    let value = 0.007_f64;
    assert_ne!(((value / 100.0) * 100.0).to_bits(), value.to_bits());
}

proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]
    #[test]
    fn prop_raw_accuracy_and_health_preserve_finite_source_bits(bits in any::<u64>()) {
        let value = f64::from_bits(bits);
        prop_assume!(value.is_finite());
        let mut s = fixture();
        let g = &mut Arc::make_mut(&mut s.regular).gameplay;
        g.accuracy = value; g.player_hp = value;
        let d = collect(&s);
        prop_assert_eq!(d["/play/accuracyPercent"].as_f64().unwrap().to_bits(), bits);
        prop_assert_eq!(d["/play/healthRaw"]["normal"].as_f64().unwrap().to_bits(), bits);
    }

    #[test]
    fn prop_raw_counts_and_combo_preserve_source_integers(count in any::<i64>(), mode in 0_i64..4) {
        let mut s = fixture();
        let g = &mut Arc::make_mut(&mut s.regular).gameplay;
        g.mode = mode;
        g.combo = count;
        g.retries = count;
        g.statistics.good = count;
        let d = collect(&s);
        prop_assert_eq!(d["/play/mode"]["number"].as_i64(), Some(mode));
        prop_assert_eq!(d["/play/combo"]["current"].as_i64(), Some(count));
        prop_assert_eq!(d["/play/judgements"]["good"].as_i64(), Some(count));
        prop_assert_eq!(d["/play/retries"].as_i64(), Some(count));
    }
}
