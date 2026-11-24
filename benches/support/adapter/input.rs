//! D17 common source input. The adapter path and the benchmark-only 1.x
//! baselines (pre-M0, M3) consume these exact nonzero values; only the
//! era-specific imports differ in those baseline harnesses.
use std::sync::Arc;
use vosu_osu_model::{
    enums::ClientType,
    states::{HitErrorArray, KeyOverlayButton},
};
use vosu_osu_state::{PreciseState, RegularState, StateBundleSnapshot};

/// Alternating regular (`/play/score`) values.
pub const SCORES: [f64; 2] = [12_345.0, 23_456.0];
/// Alternating precise-only (`k1` key-overlay count) values; regular v2 never emits keys.
pub const K1_COUNTS: [i64; 2] = [41, 42];

fn key(name: &str, count: i64) -> KeyOverlayButton {
    KeyOverlayButton {
        name: Arc::from(name),
        is_pressed: count % 2 == 0,
        count,
    }
}

/// Stable gameplay (status 2) with a nonzero score and four distinct key slots.
pub fn snapshot(score: f64, k1: i64) -> Arc<StateBundleSnapshot> {
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
    regular.global.status = 2;
    regular.gameplay.score = score;
    let precise = PreciseState {
        catcher: None,
        status: 2,
        play_time: 1234.0,
        mode: 0,
        key_overlay: Arc::new(vec![
            key("K1", k1),
            key("K2", 17),
            key("M1", 5),
            key("M2", 3),
        ]),
        keypresses: Arc::new(Vec::new()),
        hit_errors: Arc::new(HitErrorArray::default()),
        total_hit_errors: 0.0,
        committed_hit_error_index: 0,
        unstable_rate: 0.0,
    };
    Arc::new(StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(regular),
        precise: Arc::new(precise),
    })
}
