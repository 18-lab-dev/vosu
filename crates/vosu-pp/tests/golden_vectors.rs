use vosu_pp::{LiveLazerEngine, LiveScoreState, Ruleset, ScoreTotals, simulate_score};

#[test]
fn pinned_live_score_goldens() {
    let live = LiveLazerEngine;
    let totals = ScoreTotals {
        objects: 4,
        max_combo: 5,
        sliders: 1,
        slider_ends: 1,
        large_ticks: 1,
        hold_notes: 1,
        fruits: 2,
        droplets: 1,
        tiny_droplets: 1,
    };
    let goldens = [
        (
            Ruleset::Osu,
            LiveScoreState {
                accuracy: 1.0,
                n300: 4,
                slider_end_hits: 1,
                ..Default::default()
            },
        ),
        (
            Ruleset::Taiko,
            LiveScoreState {
                accuracy: 1.0,
                n300: 5,
                ..Default::default()
            },
        ),
        (
            Ruleset::Mania,
            LiveScoreState {
                accuracy: 1420.0 / 1525.0,
                n_geki: 4,
                n_katu: 1,
                ..Default::default()
            },
        ),
    ];
    for (mode, expected) in goldens {
        assert_eq!(
            live.generate_score(mode, totals, 0.95, false).unwrap(),
            expected
        );
    }

    let catch = simulate_score(Ruleset::Catch, totals, 0.95, false).unwrap();
    assert_eq!(catch.greats, 2);
    assert_eq!(catch.large_tick_hits, 1);
    assert_eq!(catch.small_tick_hits, 3);
    assert_eq!(catch.small_tick_misses, -2);
    assert_eq!(catch.goods, 0);

    let state = LiveScoreState {
        n300: 2,
        n100: 1,
        n50: 1,
        slider_end_hits: 1,
        large_tick_hits: 1,
        ..Default::default()
    };
    let live_accuracy = live.progressive_accuracy(Ruleset::Osu, state, false, 1, 1);
    assert!((live_accuracy - 0.673_913_043_478_261).abs() < 1e-15);
    assert_ne!(
        live_accuracy,
        live.progressive_accuracy(Ruleset::Osu, state, true, 1, 1)
    );
}
