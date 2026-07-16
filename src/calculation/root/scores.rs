use super::helpers::fix_decimals_to;

use crate::model::states::{Mod, Statistics};

pub fn calculate_accuracy(is_lazer: bool, mods: &[Mod], mode: i32, hits: &Statistics) -> f64 {
    let (numerator, denominator) = match mode {
        0 => (
            6.0 * hits.great as f64 + 2.0 * hits.ok as f64 + hits.meh as f64,
            6.0 * (hits.great + hits.ok + hits.meh + hits.miss) as f64,
        ),
        1 => (
            2.0 * hits.great as f64 + hits.ok as f64,
            2.0 * (hits.great + hits.ok + hits.miss) as f64,
        ),
        2 => (
            (hits.great + hits.ok + hits.meh) as f64,
            (hits.great + hits.ok + hits.meh + hits.good + hits.miss) as f64,
        ),
        3 => {
            let weight = if is_lazer || mods.iter().any(|m| m.acronym == "V2") {
                61.0
            } else {
                60.0
            };
            (
                weight * hits.perfect as f64
                    + 60.0 * hits.great as f64
                    + 40.0 * hits.good as f64
                    + 20.0 * hits.ok as f64
                    + 10.0 * hits.meh as f64,
                weight
                    * (hits.perfect + hits.great + hits.good + hits.ok + hits.meh + hits.miss)
                        as f64,
            )
        }
        _ => (0.0, 0.0),
    };
    if denominator == 0.0 {
        0.0
    } else {
        fix_decimals_to(numerator / denominator * 100.0, 2).unwrap()
    }
}

fn silver_grade<'a>(silver: bool, silver_rank: &'a str, normal_rank: &'a str) -> &'a str {
    if silver { silver_rank } else { normal_rank }
}

pub fn calculate_grade(
    is_lazer: bool,
    mods: &[Mod],
    mode: i32,
    accuracy_percent: f64,
    hits: &Statistics,
) -> String {
    let silver = mods
        .iter()
        .any(|m| matches!(m.acronym.as_str(), "FL" | "HD"));
    let top = |x, normal| silver_grade(silver, x, normal);
    let accuracy = accuracy_percent / 100.0;
    let rank = if is_lazer {
        match mode {
            0 | 1 => {
                if accuracy == 1.0 {
                    top("XH", "X")
                } else if accuracy >= 0.95 && hits.miss == 0 {
                    top("SH", "S")
                } else if accuracy >= 0.9 {
                    "A"
                } else if accuracy >= 0.8 {
                    "B"
                } else if accuracy >= 0.7 {
                    "C"
                } else {
                    "D"
                }
            }
            2 => {
                if accuracy == 1.0 {
                    top("XH", "X")
                } else if accuracy >= 0.98 {
                    top("SH", "S")
                } else if accuracy >= 0.94 {
                    "A"
                } else if accuracy >= 0.9 {
                    "B"
                } else if accuracy >= 0.85 {
                    "C"
                } else {
                    "D"
                }
            }
            3 => {
                if accuracy == 1.0 {
                    top("XH", "X")
                } else if accuracy >= 0.95 {
                    top("SH", "S")
                } else if accuracy >= 0.9 {
                    "A"
                } else if accuracy >= 0.8 {
                    "B"
                } else if accuracy >= 0.7 {
                    "C"
                } else {
                    "D"
                }
            }
            _ => "",
        }
    } else {
        match mode {
            0 | 1 => {
                let total = hits.great + hits.ok + hits.meh + hits.miss;
                if total == 0 {
                    top("XH", "X")
                } else {
                    let r300 = hits.great as f64 / total as f64;
                    let r50 = hits.meh as f64 / total as f64;
                    if r300 == 1.0 {
                        top("XH", "X")
                    } else if r300 > 0.9 && r50 < 0.01 && hits.miss == 0 {
                        top("SH", "S")
                    } else if (r300 > 0.8 && hits.miss == 0) || r300 > 0.9 {
                        "A"
                    } else if (r300 > 0.7 && hits.miss == 0) || r300 > 0.8 {
                        "B"
                    } else if r300 > 0.6 {
                        "C"
                    } else {
                        "D"
                    }
                }
            }
            2 => {
                if accuracy == 1.0 {
                    top("XH", "X")
                } else if accuracy > 0.98 {
                    top("SH", "S")
                } else if accuracy > 0.94 {
                    "A"
                } else if accuracy > 0.9 {
                    "B"
                } else if accuracy > 0.85 {
                    "C"
                } else {
                    "D"
                }
            }
            3 => {
                if accuracy == 1.0 {
                    top("XH", "X")
                } else if accuracy > 0.95 {
                    top("SH", "S")
                } else if accuracy > 0.9 {
                    "A"
                } else if accuracy > 0.8 {
                    "B"
                } else if accuracy > 0.7 {
                    "C"
                } else {
                    "D"
                }
            }
            _ => "",
        }
    };
    rank.into()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LegacyHits {
    pub n0: i64,
    pub n50: i64,
    pub n100: i64,
    pub n300: i64,
    pub geki: i64,
    pub katu: i64,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LegacyHitsExtended {
    pub n0: i64,
    pub n50: i64,
    pub n100: i64,
    pub n300: i64,
    pub geki: i64,
    pub katu: i64,
    pub slider_end_hits: i64,
    pub small_tick_hits: i64,
    pub large_tick_hits: i64,
}

pub fn from_legacy_hit_results(mode: i32, hits: LegacyHits) -> Statistics {
    let mut out = empty_statistics();
    out.great = hits.n300;
    out.miss = hits.n0;
    match mode {
        1 => out.large_bonus += hits.geki,
        3 => out.perfect += hits.geki,
        _ => {}
    }
    match mode {
        1 => out.large_bonus += hits.katu,
        2 => out.small_tick_miss += hits.katu,
        3 => out.good += hits.katu,
        _ => {}
    }
    match mode {
        0 | 1 | 3 => out.ok += hits.n100,
        2 => out.large_tick_hit += hits.n100,
        _ => {}
    }
    match mode {
        0 | 3 => out.meh += hits.n50,
        2 => out.small_tick_hit += hits.n50,
        _ => {}
    }
    out
}

pub fn to_legacy_hits(mode: i32, hits: &Statistics) -> LegacyHitsExtended {
    LegacyHitsExtended {
        n0: match mode {
            0 | 1 | 3 => hits.miss,
            2 => hits.miss + hits.large_tick_miss,
            _ => 0,
        },
        n50: match mode {
            0 | 3 => hits.meh,
            2 => hits.small_tick_hit,
            _ => 0,
        },
        n100: match mode {
            0 | 1 | 3 => hits.ok,
            2 => hits.large_tick_hit,
            _ => 0,
        },
        n300: hits.great,
        geki: match mode {
            1 => hits.large_bonus,
            3 => hits.perfect,
            _ => 0,
        },
        katu: match mode {
            1 => hits.large_bonus,
            2 => hits.small_tick_miss,
            3 => hits.good,
            _ => 0,
        },
        slider_end_hits: hits.slider_tail_hit,
        small_tick_hits: hits.small_tick_hit,
        large_tick_hits: hits.large_tick_hit,
    }
}

pub fn empty_statistics() -> Statistics {
    Statistics {
        perfect: 0,
        great: 0,
        good: 0,
        ok: 0,
        meh: 0,
        miss: 0,
        small_tick_miss: 0,
        small_tick_hit: 0,
        large_tick_miss: 0,
        large_tick_hit: 0,
        small_bonus: 0,
        large_bonus: 0,
        ignore_miss: 0,
        ignore_hit: 0,
        combo_break: 0,
        slider_tail_hit: 0,
        legacy_combo_increase: None,
    }
}
