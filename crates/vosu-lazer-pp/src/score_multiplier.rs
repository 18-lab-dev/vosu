// Copyright (c) ppy Pty Ltd <contact@ppy.sh>. Licensed under MIT Licence.
// See the vendored upstream notice for full licence text.

use std::collections::HashSet;

use crate::{BeatmapDifficultyData, Error, ModValue, Ruleset, StructuredMod, game_mods};

/// Score metadata used only by the pinned backwards-compatibility branches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreMultiplierScoreContext {
    pub total_score_version: i32,
    pub client_version: String,
    pub date_unix_ms: i64,
}

/// Calculates the lazer score multiplier at osu! revision
/// `97e94babde9d5ecbe574ecdd33509db27452cd0f`.
///
/// `difficulty` is the unmodified beatmap difficulty, matching
/// `ScoreMultiplierContext.BeatmapDifficultyWithoutMods`.
pub fn score_multiplier(
    mode: Ruleset,
    difficulty: BeatmapDifficultyData,
    mods: &[StructuredMod],
    score: Option<&ScoreMultiplierScoreContext>,
) -> Result<f64, Error> {
    // This is the same API-mod conversion path as difficulty/performance. It
    // rejects ruleset-invalid acronyms and invalid setting representations.
    let _ = game_mods(mode, mods)?;

    let mut seen = HashSet::with_capacity(mods.len());
    for game_mod in mods {
        let acronym = game_mod.acronym.to_ascii_uppercase();
        if !seen.insert(acronym.clone()) {
            return Err(Error::Mods(format!("duplicate mod type `{acronym}`")));
        }
    }

    Ok(match mode {
        Ruleset::Osu => osu_multiplier(difficulty, mods, score),
        Ruleset::Taiko => taiko_multiplier(mods, score),
        Ruleset::Catch => catch_multiplier(mods, score),
        Ruleset::Mania => mania_multiplier(mods, score),
    })
}

fn osu_multiplier(
    difficulty: BeatmapDifficultyData,
    mods: &[StructuredMod],
    score: Option<&ScoreMultiplierScoreContext>,
) -> f64 {
    let mut result = 1.0;
    let mut consumed = HashSet::new();

    // Combination registration order is observable because a matched
    // combination consumes both concrete mod types before later combinations.
    let combinations: [(&str, &str); 8] = [
        ("HD", "BL"),
        ("HD", "WG"),
        ("HD", "GR"),
        ("HD", "DF"),
        ("HD", "RP"),
        ("HD", "DP"),
        ("TC", "BL"),
        ("FL", "FF"),
    ];

    for (first, second) in combinations {
        if consumed.contains(first) || consumed.contains(second) {
            continue;
        }

        let Some(first_mod) = find_mod(mods, first) else {
            continue;
        };
        let Some(second_mod) = find_mod(mods, second) else {
            continue;
        };

        result *= match (first, second) {
            ("HD", "BL") | ("TC", "BL") => 1.24,
            ("HD", "DF") => hidden_multiplier(first_mod, true) * deflate_multiplier(second_mod),
            ("HD", _) => hidden_multiplier(first_mod, true),
            ("FL", "FF") => 1.0 + (flashlight_multiplier(first_mod) - 1.0) / 2.0,
            _ => unreachable!(),
        };

        consumed.insert(first);
        consumed.insert(second);
    }

    for game_mod in mods {
        let acronym = game_mod.acronym.to_ascii_uppercase();
        if consumed.contains(acronym.as_str()) {
            continue;
        }

        result *= match acronym.as_str() {
            "EZ" => (0.8 - (0.1 * (number(game_mod, "retries", 2.0) - 2.0)).max(0.0)).max(0.4),
            "NF" => 0.5,
            "HT" | "DC" => half_time_multiplier(number(game_mod, "speedChange", 0.75)),
            "HR" => 1.09,
            "DT" | "NC" => double_time_multiplier(number(game_mod, "speedChange", 1.5)),
            "HD" => hidden_multiplier(game_mod, false),
            "TC" => 1.02,
            "FL" => flashlight_multiplier(game_mod),
            "BL" => 1.24,
            "TP" => 0.01,
            "DA" => difficulty_adjust_multiplier(game_mod, difficulty),
            "CL" => {
                if score.is_some_and(|score| score.total_score_version < 30_000_017) {
                    0.96
                } else if boolean(game_mod, "classicNoteLock", true) {
                    0.985
                } else {
                    0.96
                }
            }
            "RD" => 0.7,
            "RX" | "AP" | "AS" => 0.1,
            "SO" => 0.95,
            "DF" => deflate_multiplier(game_mod),
            "WU" => time_ramp_multiplier(game_mod, 1.0, 1.5),
            "WD" => time_ramp_multiplier(game_mod, 1.0, 0.75),
            "AD" => 0.7,
            "MG" => 0.7 - number(game_mod, "attractionStrength", 0.5) * 0.6,
            "SY" => 0.99,
            _ => 1.0,
        };
    }

    result
}

fn taiko_multiplier(mods: &[StructuredMod], score: Option<&ScoreMultiplierScoreContext>) -> f64 {
    product(mods, |game_mod, acronym| match acronym {
        "EZ" | "NF" => 0.5,
        "HT" | "DC" => rate_adjust_multiplier(number(game_mod, "speedChange", 0.75)),
        "SR" => 0.6,
        "HR" | "HD" => 1.06,
        "DT" | "NC" => rate_adjust_multiplier(number(game_mod, "speedChange", 1.5)),
        "FL" => {
            if flashlight_uses_default_configuration(game_mod) {
                1.12
            } else {
                1.0
            }
        }
        "DA" => 0.5,
        "CL" => classic_multiplier(score),
        "CS" => 0.9,
        "RX" => 0.1,
        "WU" | "WD" | "AS" => 0.5,
        _ => 1.0,
    })
}

fn catch_multiplier(mods: &[StructuredMod], score: Option<&ScoreMultiplierScoreContext>) -> f64 {
    product(mods, |game_mod, acronym| match acronym {
        "EZ" | "NF" => 0.5,
        "HT" | "DC" => rate_adjust_multiplier(number(game_mod, "speedChange", 0.75)),
        "HR" => 1.12,
        "DT" | "NC" => rate_adjust_multiplier(number(game_mod, "speedChange", 1.5)),
        "HD" => 1.06,
        "FL" => {
            if flashlight_uses_default_configuration(game_mod) {
                1.12
            } else {
                1.0
            }
        }
        "DA" => 0.5,
        "CL" => classic_multiplier(score),
        "RX" => 0.1,
        "WU" | "WD" => 0.5,
        "SY" => 0.8,
        _ => 1.0,
    })
}

fn mania_multiplier(mods: &[StructuredMod], score: Option<&ScoreMultiplierScoreContext>) -> f64 {
    product(mods, |game_mod, acronym| match acronym {
        "EZ" | "NF" => 0.5,
        "HT" | "DC" => rate_adjust_multiplier(number(game_mod, "speedChange", 0.75)),
        "NR" | "CS" | "HO" => 0.9,
        "DA" | "WU" | "WD" | "AS" => 0.5,
        "CL" => classic_multiplier(score),
        "1K" | "2K" | "3K" | "4K" | "5K" | "6K" | "7K" | "8K" | "9K" | "10K" => {
            key_mod_multiplier(score)
        }
        _ => 1.0,
    })
}

fn product(mut mods: &[StructuredMod], multiplier: impl Fn(&StructuredMod, &str) -> f64) -> f64 {
    let mut result = 1.0;
    while let Some((game_mod, rest)) = mods.split_first() {
        let acronym = game_mod.acronym.to_ascii_uppercase();
        result *= multiplier(game_mod, &acronym);
        mods = rest;
    }
    result
}

fn find_mod<'a>(mods: &'a [StructuredMod], acronym: &str) -> Option<&'a StructuredMod> {
    mods.iter()
        .find(|game_mod| game_mod.acronym.eq_ignore_ascii_case(acronym))
}

fn setting<'a>(game_mod: &'a StructuredMod, key: &str) -> Option<&'a ModValue> {
    let snake_case = crate::camel_to_snake(key);

    // Memory-reader DTOs use osu!'s snake_case setting names. Retain the
    // wrapper's camelCase input spelling as a lower-priority public alias.
    game_mod
        .settings
        .get(&snake_case)
        .or_else(|| game_mod.settings.get(key))
}

fn number(game_mod: &StructuredMod, key: &str, default: f64) -> f64 {
    match setting(game_mod, key) {
        Some(ModValue::Number(value)) => *value,
        _ => default,
    }
}

fn optional_number(game_mod: &StructuredMod, key: &str, default: f64) -> f64 {
    number(game_mod, key, default)
}

fn boolean(game_mod: &StructuredMod, key: &str, default: bool) -> bool {
    match setting(game_mod, key) {
        Some(ModValue::Bool(value)) => *value,
        _ => default,
    }
}

fn half_time_multiplier(speed_change: f64) -> f64 {
    (speed_change * 20.0).trunc() / 20.0 * 1.4 - 0.5
}

fn double_time_multiplier(speed_change: f64) -> f64 {
    let value = (speed_change * 10.0).trunc() / 10.0;
    let penalty = if value != 1.5 && value != 1.0 {
        0.01
    } else {
        0.0
    };
    (value - 1.0) * 0.46 + 1.0 - penalty
}

fn rate_adjust_multiplier(speed_change: f64) -> f64 {
    let value = (speed_change * 10.0).trunc() / 10.0 - 1.0;
    if speed_change >= 1.0 {
        1.0 + value / 5.0
    } else {
        0.6 + value
    }
}

fn hidden_multiplier(hidden: &StructuredMod, other_mods_provide_timing_info: bool) -> f64 {
    let mut value = 1.04;
    if boolean(hidden, "onlyFadeApproachCircles", false) {
        value -= 0.02;
    }
    if other_mods_provide_timing_info {
        value -= 0.02;
    }
    value
}

fn flashlight_multiplier(flashlight: &StructuredMod) -> f64 {
    let mut value =
        (1.2 - 0.2 * (number(flashlight, "sizeMultiplier", 1.0) - 1.0)).clamp(1.02, 1.2);
    if !boolean(flashlight, "comboBasedSize", true) {
        value = 1.0 + (value - 1.0) / 5.0;
    }
    value
}

fn flashlight_uses_default_configuration(flashlight: &StructuredMod) -> bool {
    number(flashlight, "sizeMultiplier", 1.0) == 1.0 && boolean(flashlight, "comboBasedSize", true)
}

fn difficulty_adjust_multiplier(
    game_mod: &StructuredMod,
    difficulty: BeatmapDifficultyData,
) -> f64 {
    let cs = optional_number(game_mod, "circleSize", f64::from(difficulty.circle_size));
    let hp = optional_number(game_mod, "drainRate", f64::from(difficulty.drain_rate));
    let od = optional_number(
        game_mod,
        "overallDifficulty",
        f64::from(difficulty.overall_difficulty),
    );
    let ar = optional_number(
        game_mod,
        "approachRate",
        f64::from(difficulty.approach_rate),
    );

    let parameter = |selected: f64, original: f32| {
        (1.0 - (selected - f64::from(original)).abs() * 0.5).max(0.1)
    };

    (parameter(cs, difficulty.circle_size)
        * parameter(hp, difficulty.drain_rate)
        * parameter(od, difficulty.overall_difficulty)
        * parameter(ar, difficulty.approach_rate))
    .max(0.1)
}

fn time_ramp_multiplier(game_mod: &StructuredMod, default_initial: f64, default_final: f64) -> f64 {
    let initial = number(game_mod, "initialRate", default_initial);
    let final_rate = number(game_mod, "finalRate", default_final);
    let min_speed = initial.min(final_rate);
    let max_speed = initial.max(final_rate);
    let multiplier = |speed| {
        if speed < 1.0 {
            half_time_multiplier(speed)
        } else {
            double_time_multiplier(speed)
        }
    };
    0.8 * multiplier(min_speed) + 0.2 * multiplier(max_speed)
}

fn deflate_multiplier(deflate: &StructuredMod) -> f64 {
    1.0 - (0.02 * (number(deflate, "startScale", 2.0) - 2.0)).max(0.0)
}

fn classic_multiplier(score: Option<&ScoreMultiplierScoreContext>) -> f64 {
    if score.is_some_and(|score| score.total_score_version < 30_000_017) {
        0.96
    } else {
        1.0
    }
}

fn key_mod_multiplier(score: Option<&ScoreMultiplierScoreContext>) -> f64 {
    const NEW_KEY_MOD_DATE: i64 = 1_752_796_800_000;
    let Some(score) = score else { return 0.9 };

    if !score.client_version.is_empty() {
        let mut pieces = score.client_version.split('.');
        let year = pieces.next().and_then(|piece| piece.parse::<i32>().ok());
        let month_day = pieces.next().and_then(|piece| piece.parse::<i32>().ok());
        return if matches!((year, month_day), (Some(year), Some(month_day)) if year < 2025 || (year == 2025 && month_day < 718))
        {
            1.0
        } else {
            0.9
        };
    }

    if score.date_unix_ms < NEW_KEY_MOD_DATE {
        1.0
    } else {
        0.9
    }
}
