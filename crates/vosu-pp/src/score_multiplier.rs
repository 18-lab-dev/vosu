use crate::{BeatmapDifficultyData, Error, Ruleset, StructuredMod};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreMultiplierScoreContext {
    pub total_score_version: i32,
    pub client_version: String,
    pub date_unix_ms: i64,
}

pub fn score_multiplier(
    mode: Ruleset,
    difficulty: BeatmapDifficultyData,
    mods: &[StructuredMod],
    score: Option<&ScoreMultiplierScoreContext>,
) -> Result<f64, Error> {
    let mut seen = std::collections::BTreeSet::new();
    for game_mod in mods {
        let key = game_mod.acronym.to_ascii_uppercase();
        if !seen.insert(key) {
            return Err(Error::Calculation("duplicate mod type".into()));
        }
    }
    if mode == Ruleset::Osu && score.is_some_and(|context| context.total_score_version < 30_000_017)
    {
        return Ok(osu_v1_multiplier(mods));
    }
    let mut product = 1.0;
    let mut consumed = vec![false; mods.len()];
    if mode == Ruleset::Osu {
        for (a, b, m) in [
            ("HD", "BL", 1.24),
            ("HD", "WG", 1.02),
            ("HD", "GR", 1.02),
            ("HD", "DF", 1.0),
            ("HD", "RP", 1.02),
            ("HD", "DP", 1.02),
            ("TC", "BL", 1.24),
            ("FL", "FF", 0.0),
        ] {
            if let (Some(i), Some(j)) = (find(mods, &consumed, a), find(mods, &consumed, b)) {
                let value = match (a, b) {
                    ("HD", "BL") | ("TC", "BL") => 1.24,
                    ("HD", "DF") => hidden(&mods[i], true) * deflate(&mods[j]),
                    ("HD", _) => hidden(&mods[i], true),
                    ("FL", "FF") => 1.0 + (flashlight(&mods[i]) - 1.0) / 2.0,
                    _ => m,
                };
                product *= value;
                consumed[i] = true;
                consumed[j] = true;
            }
        }
    }
    for (index, game_mod) in mods.iter().enumerate() {
        if consumed[index] {
            continue;
        }
        let ac = game_mod.acronym.to_ascii_uppercase();
        product *= match mode {
            Ruleset::Osu => match ac.as_str() {
                "EZ" => (0.8 - 0.1 * (game_mod.number("retries").unwrap_or(2.0) - 2.0).max(0.0))
                    .max(0.4),
                "NF" => 0.5,
                "HT" | "DC" => half(game_mod.number("speed_change").unwrap_or(0.75)),
                "HR" => 1.09,
                "DT" | "NC" => double(game_mod.number("speed_change").unwrap_or(1.5)),
                "HD" => hidden(game_mod, false),
                "TC" => 1.02,
                "FL" => flashlight(game_mod),
                "BL" => 1.24,
                "TP" => 0.01,
                "DA" => da(game_mod, difficulty),
                "CL" => {
                    if game_mod.boolean("classic_note_lock").unwrap_or(true) {
                        0.985
                    } else {
                        0.96
                    }
                }
                "RD" => 0.7,
                "RX" | "AP" => 0.1,
                "SO" => 0.95,
                "DF" => deflate(game_mod),
                "WU" | "WD" => time_ramp(game_mod),
                "AD" => 0.7,
                "MG" => 0.7 - game_mod.number("attraction_strength").unwrap_or(0.5) * 0.6,
                "AS" => 0.1,
                "SY" => 0.99,
                _ => 1.0,
            },
            Ruleset::Taiko => match ac.as_str() {
                "EZ" | "NF" | "DA" | "WU" | "WD" | "AS" => 0.5,
                "HT" | "DC" => rate(game_mod.number("speed_change").unwrap_or(0.75)),
                "SR" => 0.6,
                "HR" => 1.06,
                "DT" | "NC" => rate(game_mod.number("speed_change").unwrap_or(1.5)),
                "HD" => 1.06,
                "FL" => {
                    if game_mod.settings.is_empty() {
                        1.12
                    } else {
                        1.0
                    }
                }
                "CL" => classic(score),
                "CS" => 0.9,
                "RX" => 0.1,
                _ => 1.0,
            },
            Ruleset::Catch => match ac.as_str() {
                "EZ" | "NF" | "DA" => 0.5,
                "HT" | "DC" => rate(game_mod.number("speed_change").unwrap_or(0.75)),
                "HR" => 1.12,
                "DT" | "NC" => rate(game_mod.number("speed_change").unwrap_or(1.5)),
                "HD" => 1.06,
                "FL" => {
                    if game_mod.settings.is_empty() {
                        1.12
                    } else {
                        1.0
                    }
                }
                "CL" => classic(score),
                "RX" => 0.1,
                "WU" | "WD" => 0.5,
                "SY" => 0.8,
                _ => 1.0,
            },
            Ruleset::Mania => match ac.as_str() {
                "EZ" | "NF" | "DA" | "WU" | "WD" | "AS" => 0.5,
                "HT" | "DC" => rate(game_mod.number("speed_change").unwrap_or(0.75)),
                "NR" | "CS" | "HO" => 0.9,
                "CL" => classic(score),
                "1K" | "2K" | "3K" | "4K" | "5K" | "6K" | "7K" | "8K" | "9K" | "10K" => key(score),
                _ => 1.0,
            },
        };
    }
    Ok(product)
}
fn osu_v1_multiplier(mods: &[StructuredMod]) -> f64 {
    mods.iter()
        .map(
            |game_mod| match game_mod.acronym.to_ascii_uppercase().as_str() {
                "EZ" | "NF" => 0.5,
                "HT" | "DC" => rate_adjust(game_mod.number("speed_change").unwrap_or(0.75)),
                "HR" => default_multiplier(game_mod, 1.06),
                "DT" | "NC" => rate_adjust(game_mod.number("speed_change").unwrap_or(1.5)),
                "HD" | "FL" | "BL" => default_multiplier(
                    game_mod,
                    if game_mod.acronym.eq_ignore_ascii_case("HD") {
                        1.06
                    } else {
                        1.12
                    },
                ),
                "TP" => 0.1,
                "DA" => 0.5,
                "CL" => 0.96,
                "RX" | "AP" => 0.1,
                "SO" => 0.9,
                "WU" | "WD" => 0.5,
                "MG" => 0.5,
                "AD" => 0.5,
                "SY" => 0.8,
                _ => 1.0,
            },
        )
        .product()
}

fn default_multiplier(game_mod: &StructuredMod, value: f64) -> f64 {
    if game_mod.settings.is_empty() {
        value
    } else {
        1.0
    }
}

fn rate_adjust(speed: f64) -> f64 {
    let value = (speed * 10.0).trunc() / 10.0 - 1.0;
    if speed >= 1.0 {
        1.0 + value / 5.0
    } else {
        0.6 + value
    }
}
fn find(mods: &[StructuredMod], used: &[bool], ac: &str) -> Option<usize> {
    mods.iter()
        .enumerate()
        .find_map(|(i, m)| (!used[i] && m.acronym.eq_ignore_ascii_case(ac)).then_some(i))
}
fn half(speed: f64) -> f64 {
    (speed * 20.0).trunc() / 20.0 * 1.4 - 0.5
}
fn double(speed: f64) -> f64 {
    let value = (speed * 10.0).trunc() / 10.0;
    (value - 1.0) * 0.46 + 1.0
        - if value != 1.5 && value != 1.0 {
            0.01
        } else {
            0.0
        }
}
fn rate(speed: f64) -> f64 {
    let v = (speed * 10.0).trunc() / 10.0 - 1.0;
    if speed >= 1.0 { 1.0 + v / 5.0 } else { 0.6 + v }
}
fn hidden(m: &StructuredMod, timing: bool) -> f64 {
    1.04 - if m.boolean("only_fade_approach_circles").unwrap_or(false) {
        0.02
    } else {
        0.0
    } - if timing { 0.02 } else { 0.0 }
}
fn flashlight(m: &StructuredMod) -> f64 {
    let mut v = (1.2 - 0.2 * (m.number("size_multiplier").unwrap_or(1.0) - 1.0)).clamp(1.02, 1.2);
    if !m.boolean("combo_based_size").unwrap_or(true) {
        v = 1.0 + (v - 1.0) / 5.0
    }
    v
}
fn deflate(m: &StructuredMod) -> f64 {
    1.0 - (0.02 * (m.number("start_scale").unwrap_or(2.0) - 2.0)).max(0.0)
}
fn da(m: &StructuredMod, d: BeatmapDifficultyData) -> f64 {
    let vals = [
        (m.number("circle_size"), d.circle_size),
        (m.number("drain_rate"), d.drain_rate),
        (m.number("overall_difficulty"), d.overall_difficulty),
        (m.number("approach_rate"), d.approach_rate),
    ];
    vals.into_iter()
        .map(|(v, b)| (1.0 - 0.5 * (v.unwrap_or(f64::from(b)) - f64::from(b)).abs()).max(0.1))
        .product::<f64>()
        .max(0.1)
}
fn time_ramp(m: &StructuredMod) -> f64 {
    let (default_initial, default_final) = if m.acronym.eq_ignore_ascii_case("WU") {
        (1.0, 1.5)
    } else {
        (1.0, 0.75)
    };
    let a = m.number("initial_rate").unwrap_or(default_initial);
    let b = m.number("final_rate").unwrap_or(default_final);
    let f = |v| if v < 1.0 { half(v) } else { double(v) };
    0.8 * f(a.min(b)) + 0.2 * f(a.max(b))
}
fn classic(score: Option<&ScoreMultiplierScoreContext>) -> f64 {
    if score.is_some_and(|s| s.total_score_version < 30_000_017) {
        0.96
    } else {
        1.0
    }
}
fn key(score: Option<&ScoreMultiplierScoreContext>) -> f64 {
    let Some(s) = score else { return 0.9 };
    if !s.client_version.is_empty() {
        let mut p = s
            .client_version
            .split('.')
            .filter_map(|x| x.parse::<i32>().ok());
        let y = p.next().unwrap_or_default();
        let md = p.next().unwrap_or_default();
        if y < 2025 || (y == 2025 && md < 718) {
            1.0
        } else {
            0.9
        }
    } else if s.date_unix_ms < 1_752_796_800_000 {
        1.0
    } else {
        0.9
    }
}
