use std::cmp::Ordering;

use indexmap::IndexMap;
use md5::{Digest, Md5};
use serde_json::Value;

use super::helpers::fix_decimals_to;

use crate::model::states::{CalculateMods, Mod};

pub const BIT_VALUES: [&str; 31] = [
    "NF", "EZ", "TD", "HD", "HR", "SD", "DT", "RX", "HT", "NC", "FL", "AT", "SO", "AP", "PF", "4K",
    "5K", "6K", "7K", "8K", "FI", "RD", "CN", "TG", "9K", "10K", "1K", "3K", "2K", "v2", "MR",
];

pub const ALL_MOD_ACRONYMS: [&str; 69] = [
    "EZ", "NF", "HT", "DC", "HR", "SD", "PF", "DT", "NC", "HD", "FL", "BL", "ST", "AC", "TP", "DA",
    "CL", "RD", "1K", "2K", "3K", "4K", "5K", "6K", "7K", "8K", "9K", "10K", "MR", "AL", "SG",
    "AT", "CN", "RX", "AP", "SO", "TR", "WG", "SI", "GR", "DF", "WU", "WD", "TC", "BR", "AD", "MU",
    "NS", "MG", "RP", "AS", "FR", "BU", "SY", "DP", "TD", "SV2", "HO", "CS", "IN", "DS", "CO",
    "FI", "NR", "FF", "SW", "SR", "MF", "BM",
];

pub mod osu_mods {
    pub const NO_FAIL: i64 = 1 << 0;
    pub const EASY: i64 = 1 << 1;
    pub const TOUCH_DEVICE: i64 = 1 << 2;
    pub const HIDDEN: i64 = 1 << 3;
    pub const HARD_ROCK: i64 = 1 << 4;
    pub const SUDDEN_DEATH: i64 = 1 << 5;
    pub const DOUBLE_TIME: i64 = 1 << 6;
    pub const RELAX: i64 = 1 << 7;
    pub const HALF_TIME: i64 = 1 << 8;
    pub const NIGHTCORE: i64 = 1 << 9;
    pub const FLASHLIGHT: i64 = 1 << 10;
    pub const AUTOPLAY: i64 = 1 << 11;
    pub const SPUN_OUT: i64 = 1 << 12;
    pub const RELAX_2: i64 = 1 << 13;
    pub const PERFECT: i64 = 1 << 14;
    pub const KEY_4: i64 = 1 << 15;
    pub const KEY_5: i64 = 1 << 16;
    pub const KEY_6: i64 = 1 << 17;
    pub const KEY_7: i64 = 1 << 18;
    pub const KEY_8: i64 = 1 << 19;
    pub const FADE_IN: i64 = 1 << 20;
    pub const RANDOM: i64 = 1 << 21;
    pub const CINEMA: i64 = 1 << 22;
    pub const TARGET: i64 = 1 << 23;
    pub const KEY_9: i64 = 1 << 24;
    pub const KEY_10: i64 = 1 << 25;
    pub const KEY_1: i64 = 1 << 26;
    pub const KEY_3: i64 = 1 << 27;
    pub const KEY_2: i64 = 1 << 28;
    pub const SCORE_V2: i64 = 1 << 29;
    pub const MIRROR: i64 = 1 << 30;
    pub const KEY_MOD: i64 =
        KEY_1 | KEY_2 | KEY_3 | KEY_4 | KEY_5 | KEY_6 | KEY_7 | KEY_8 | KEY_9 | KEY_10;
    pub const KEY_MOD_UNRANKED: i64 = KEY_1 | KEY_2 | KEY_3 | KEY_9 | KEY_10;
    pub const FREE_MOD_ALLOWED: i64 = NO_FAIL
        | EASY
        | HIDDEN
        | HARD_ROCK
        | SUDDEN_DEATH
        | FLASHLIGHT
        | FADE_IN
        | RELAX
        | RELAX_2
        | SPUN_OUT
        | KEY_MOD;
    pub const SCORE_INCREASE_MODS: i64 = HIDDEN | HARD_ROCK | DOUBLE_TIME | FLASHLIGHT | FADE_IN;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModCategories {
    pub difficulty_reduction: &'static [&'static str],
    pub difficulty_increase: &'static [&'static str],
    pub automation: &'static [&'static str],
    pub conversion: &'static [&'static str],
    pub fun: &'static [&'static str],
    pub system: &'static [&'static str],
}

const OSU_CATEGORIES: ModCategories = ModCategories {
    difficulty_reduction: &["EZ", "NF", "HT", "DC"],
    difficulty_increase: &[
        "HR", "SD", "PF", "DT", "NC", "HD", "TC", "FL", "BL", "ST", "AC",
    ],
    automation: &["AT", "CN", "RX", "AP", "SO"],
    conversion: &["TP", "DA", "CL", "RD", "MR", "AL", "SG"],
    fun: &[
        "TR", "WG", "SI", "GR", "DF", "WU", "WD", "BR", "AD", "MU", "NS", "MG", "RP", "AS", "FR",
        "BU", "SY", "DP", "BM",
    ],
    system: &["TD", "SV2"],
};
const TAIKO_CATEGORIES: ModCategories = ModCategories {
    difficulty_reduction: &["EZ", "NF", "HT", "DC", "SR"],
    difficulty_increase: &["HR", "SD", "PF", "DT", "NC", "HD", "FL", "AC"],
    automation: &["AT", "CN", "RX"],
    conversion: &["RD", "DA", "CL", "SW", "SG", "CS"],
    fun: &["WU", "WD", "MU", "AS"],
    system: &["TD", "SV2"],
};
const FRUITS_CATEGORIES: ModCategories = ModCategories {
    difficulty_reduction: &["EZ", "NF", "HT", "DC"],
    difficulty_increase: &["HR", "SD", "PF", "DT", "NC", "HD", "FL", "AC"],
    automation: &["AT", "CN", "RX"],
    conversion: &["DA", "CL", "MR"],
    fun: &["WU", "WD", "FF", "MU", "NS", "MF"],
    system: &["TD", "SV2"],
};
const MANIA_CATEGORIES: ModCategories = ModCategories {
    difficulty_reduction: &["EZ", "NF", "HT", "DC", "NR"],
    difficulty_increase: &["HR", "SD", "PF", "DT", "NC", "FI", "HD", "CO", "FL", "AC"],
    automation: &["AT", "CN"],
    conversion: &[
        "RD", "DS", "MR", "DA", "CL", "IN", "CS", "HO", "1K", "2K", "3K", "4K", "5K", "6K", "7K",
        "8K", "9K", "10K",
    ],
    fun: &["WU", "WD", "MU", "AS"],
    system: &["TD", "SV2"],
};

pub fn mod_categories(mode: i32) -> Option<&'static ModCategories> {
    match mode {
        0 => Some(&OSU_CATEGORIES),
        1 => Some(&TAIKO_CATEGORIES),
        2 => Some(&FRUITS_CATEGORIES),
        3 => Some(&MANIA_CATEGORIES),
        _ => None,
    }
}
/// Setting property names accepted by each structured lazer mod.
///
/// An empty slice means that the source mod interface has no settings.
pub fn mod_setting_keys(acronym: &str) -> &'static [&'static str] {
    match acronym {
        "EZ" => &["retries"],
        "HT" | "DT" => &["speed_change", "adjust_pitch"],
        "DC" | "NC" => &["speed_change"],
        "SD" => &["fail_on_slider_tail", "restart"],
        "PF" => &["require_perfect_hits", "restart"],
        "HD" => &["only_fade_approach_circles"],
        "FL" => &["follow_delay", "size_multiplier", "combo_based_size"],
        "AC" => &["minimum_accuracy", "accuracy_judge_mode", "restart"],
        "TP" => &["seed", "metronome"],
        "DA" => &[
            "circle_size",
            "approach_rate",
            "drain_rate",
            "overall_difficulty",
            "extended_limits",
            "hard_rock_offsets",
        ],
        "CL" => &[
            "no_slider_head_accuracy",
            "classic_note_lock",
            "always_play_tail_sample",
            "fade_hit_circle_early",
            "classic_health",
        ],
        "RD" => &["angle_sharpness", "seed"],
        "MR" => &["reflection"],
        "WG" => &["strength"],
        "GR" | "DF" => &["start_scale"],
        "WU" | "WD" => &["initial_rate", "final_rate", "adjust_pitch"],
        "BR" => &["spin_speed", "direction"],
        "AD" => &["scale", "style"],
        "MU" => &[
            "inverse_muting",
            "enable_metronome",
            "mute_combo_count",
            "affects_hit_sounds",
        ],
        "NS" => &["hidden_combo_count"],
        "MG" => &["attraction_strength"],
        "RP" => &["repulsion_strength"],
        "AS" => &["initial_rate", "adjust_pitch"],
        "DP" => &["max_depth", "show_approach_circles"],
        "CO" => &["coverage", "direction"],
        "BM" => &["max_size_combo_count", "max_cursor_size"],
        "SR" => &[
            "one_third_conversion",
            "one_sixth_conversion",
            "one_eighth_conversion",
        ],
        _ => &[],
    }
}

pub fn default_calculated_mods() -> CalculateMods {
    CalculateMods {
        checksum: String::new(),
        number: 0,
        name: String::new(),
        array: Vec::new(),
        rate: 1.0,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModsInput {
    Number(i64),
    Name(String),
    Structured(Vec<Mod>),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModsError {
    #[error("Specify mods name (HDDT or 72)")]
    Missing,
}

fn source_order(acronym: &str) -> u8 {
    // The source uses `(ModsOrder[key] || 99)`: NF's explicit zero is falsy.
    match acronym.to_ascii_lowercase().as_str() {
        "ez" => 1,
        "hd" => 2,
        "dt" | "nc" | "ht" => 3,
        "hr" => 4,
        "so" | "sd" | "pf" => 5,
        "fl" => 6,
        "td" => 7,
        _ => 99,
    }
}

fn replace_aliases(name: String) -> String {
    name.replacen("DTNC", "NC", 1)
        .replacen("SDPF", "PF", 1)
        .replacen("ATCN", "CN", 1)
}

pub fn mods_name(mods_number: i64, ordered: bool) -> String {
    let bits = mods_number as i32;
    if mods_number < 1 {
        return String::new();
    }
    let mut parts = Vec::new();
    for (index, acronym) in BIT_VALUES.iter().enumerate() {
        if bits & (1_i32.wrapping_shl(index as u32)) != 0 {
            parts.push(*acronym);
        }
    }
    if ordered {
        parts.sort_by_key(|part| source_order(part));
    }
    replace_aliases(parts.concat())
}

fn legacy_bit(acronym: &str) -> Option<(i64, &'static str)> {
    Some(match acronym {
        "nf" => (1, "NF"),
        "ez" => (2, "EZ"),
        "td" => (4, "TD"),
        "hd" => (8, "HD"),
        "hr" => (16, "HR"),
        "sd" => (32, "SD"),
        "dt" => (64, "DT"),
        "rx" => (128, "RX"),
        "ht" => (256, "HT"),
        "nc" => (576, "NC"),
        "fl" => (1024, "FL"),
        "at" => (2048, "AT"),
        "so" => (4096, "SO"),
        "ap" => (8192, "AP"),
        "pf" => (16416, "PF"),
        "4k" => (32768, "4K"),
        "5k" => (65536, "5K"),
        "6k" => (131072, "6K"),
        "7k" => (262144, "7K"),
        "8k" => (524288, "8K"),
        "fi" => (1048576, "FI"),
        "rd" => (2097152, "RD"),
        "cn" => (4194304, "CN"),
        "target" => (8388608, "TG"),
        "9k" => (16777216, "9K"),
        "keycoop" => (33554432, "10K"),
        "1k" => (67108864, "1K"),
        "3k" => (134217728, "3K"),
        "2k" => (268435456, "2K"),
        "scorev2" => (536870912, "v2"),
        "mr" => (1073741824, "MR"),
        _ => return None,
    })
}

pub(super) fn js_to_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(v) => v.to_string(),
        Value::String(v) => v.clone(),
        Value::Number(v) => js_number(v),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    js_to_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

pub(super) fn js_number(number: &serde_json::Number) -> String {
    ecma_number_to_string(number.as_f64().unwrap_or(f64::NAN))
}

fn ecma_number_to_string(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    if !value.is_finite() {
        return if value.is_nan() {
            "NaN"
        } else if value.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .into();
    }
    let negative = value.is_sign_negative();
    let raw = value.abs().to_string();
    let (mantissa, exponent) = raw
        .split_once(['e', 'E'])
        .map_or((raw.as_str(), 0), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i32>().unwrap())
        });
    let point_before_exponent = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let mut digits = mantissa
        .chars()
        .filter(|character| *character != '.')
        .collect::<String>();
    let leading = digits.bytes().take_while(|byte| *byte == b'0').count();
    if leading != 0 {
        digits.drain(..leading);
    }
    if digits.is_empty() {
        return "0".into();
    }
    let point = point_before_exponent + exponent - leading as i32;
    while digits.ends_with('0') && digits.len() > 1 {
        digits.pop();
    }
    let mut output = String::new();
    if negative {
        output.push('-');
    }
    if point > 21 || point <= -6 {
        output.push(digits.as_bytes()[0] as char);
        if digits.len() > 1 {
            output.push('.');
            output.push_str(&digits[1..]);
        }
        output.push('e');
        let scientific_exponent = point - 1;
        if scientific_exponent >= 0 {
            output.push('+');
        }
        output.push_str(&scientific_exponent.to_string());
    } else if point <= 0 {
        output.push_str("0.");
        output.extend(std::iter::repeat_n('0', (-point) as usize));
        output.push_str(&digits);
    } else if point as usize >= digits.len() {
        output.push_str(&digits);
        output.extend(std::iter::repeat_n('0', point as usize - digits.len()));
    } else {
        output.push_str(&digits[..point as usize]);
        output.push('.');
        output.push_str(&digits[point as usize..]);
    }
    output
}

fn round_mod_settings(mods: &mut [Mod]) {
    for entry in mods {
        let Some(settings) = entry.settings.as_mut() else {
            continue;
        };
        for value in settings.values_mut() {
            if let Some(number) = value.as_f64() {
                let rounded = fix_decimals_to(number, 2).expect("two decimals are always valid");
                *value = if rounded == 0.0 {
                    Value::from(0)
                } else {
                    Value::from(rounded)
                };
            }
        }
        for key in ["accuracy_judge_mode", "reflection", "direction", "style"] {
            if let Some(value) = settings.get_mut(key) {
                *value = Value::String(js_to_string(value));
            }
        }
    }
}

fn array_json_stringify(mods: &[Mod]) -> String {
    let mut out = String::from("[");
    for (index, entry) in mods.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        out.push_str("{\"acronym\":");
        out.push_str(
            &serde_json::to_string(&entry.acronym).expect("string serialization cannot fail"),
        );
        if let Some(settings) = &entry.settings {
            out.push_str(",\"settings\":{");
            let mut keys: Vec<_> = settings.keys().collect();
            keys.sort_by(|a, b| match (array_index(a), array_index(b)) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            });
            for (setting_index, key) in keys.into_iter().enumerate() {
                if setting_index != 0 {
                    out.push(',');
                }
                out.push_str(
                    &serde_json::to_string(key).expect("string serialization cannot fail"),
                );
                out.push(':');
                write_js_json(&mut out, &settings[key]);
            }
            out.push('}');
        }
        out.push('}');
    }
    out.push(']');
    out
}

fn array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    let value = key.parse::<u32>().ok()?;
    (value != u32::MAX && value.to_string() == key).then_some(value)
}

fn write_js_json(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
        Value::Number(v) => out.push_str(&js_number(v)),
        Value::String(v) => {
            out.push_str(&serde_json::to_string(v).expect("string serialization cannot fail"))
        }
        Value::Array(values) => {
            out.push('[');
            for (i, value) in values.iter().enumerate() {
                if i != 0 {
                    out.push(',');
                }
                write_js_json(out, value);
            }
            out.push(']');
        }
        Value::Object(values) => {
            out.push('{');
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|(a, _), (b, _)| match (array_index(a), array_index(b)) {
                (Some(a), Some(b)) => a.cmp(&b),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                _ => Ordering::Equal,
            });
            for (i, (key, value)) in entries.into_iter().enumerate() {
                if i != 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(key).unwrap());
                out.push(':');
                write_js_json(out, value);
            }
            out.push('}');
        }
    }
}

pub fn mods_checksum_bytes(mods: &[Mod]) -> Vec<u8> {
    array_json_stringify(mods).into_bytes()
}

pub fn mods_checksum(mods: &[Mod]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Md5::digest(mods_checksum_bytes(mods));
    let mut output = String::with_capacity(32);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

pub fn calculate_mods_optional(
    input: Option<ModsInput>,
    ordered: bool,
) -> Result<CalculateMods, ModsError> {
    calculate_mods(input.ok_or(ModsError::Missing)?, ordered)
}

pub fn calculate_mods(input: ModsInput, ordered: bool) -> Result<CalculateMods, ModsError> {
    if let ModsInput::Name(name) = &input
        && name.is_empty()
    {
        return Ok(default_calculated_mods());
    }
    if let ModsInput::Number(number) = input {
        let name = mods_name(number, ordered);
        let array = name
            .as_bytes()
            .chunks(2)
            .map(|chunk| Mod {
                acronym: String::from_utf8_lossy(chunk).to_ascii_uppercase(),
                settings: None,
            })
            .collect::<Vec<_>>();
        let rate = if array
            .iter()
            .any(|m| matches!(m.acronym.as_str(), "DT" | "NC"))
        {
            1.5
        } else if array
            .iter()
            .any(|m| matches!(m.acronym.as_str(), "HT" | "DC"))
        {
            0.75
        } else {
            1.0
        };
        return Ok(CalculateMods {
            checksum: mods_checksum(&array),
            number,
            name,
            array,
            rate,
        });
    }
    let (names, mut array) = match input {
        ModsInput::Name(name) => {
            let names = name
                .to_ascii_lowercase()
                .as_bytes()
                .chunks(2)
                .map(|part| String::from_utf8_lossy(part).into_owned())
                .collect::<Vec<_>>();
            let array = names
                .iter()
                .map(|name| Mod {
                    acronym: name.to_ascii_uppercase(),
                    settings: None,
                })
                .collect();
            (names, array)
        }
        ModsInput::Structured(array) => (
            array
                .iter()
                .map(|entry| entry.acronym.to_ascii_lowercase())
                .collect(),
            array,
        ),
        ModsInput::Number(_) => unreachable!(),
    };
    let mut number = 0_i64;
    let mut speed_change = 1.0;
    let mut parts = Vec::with_capacity(names.len());
    for name in &names {
        if let Some((bit, display)) = legacy_bit(name) {
            number += bit;
            if name == "dt" || name == "nc" {
                speed_change = 1.5;
            }
            if name == "ht" {
                speed_change = 0.75;
            }
            parts.push(display.to_owned());
        } else {
            parts.push(name.to_ascii_uppercase());
        }
    }
    parts.sort_by_key(|part| source_order(part));
    round_mod_settings(&mut array);
    let settings_speed = array
        .iter()
        .find_map(|entry| entry.settings.as_ref()?.get("speed_change")?.as_f64());
    if speed_change == 1.0
        && array
            .iter()
            .any(|m| matches!(m.acronym.as_str(), "DT" | "NC"))
    {
        speed_change = 1.5;
    }
    if speed_change == 1.0
        && array
            .iter()
            .any(|m| matches!(m.acronym.as_str(), "HT" | "DC"))
    {
        speed_change = 0.75;
    }
    let rate = settings_speed
        .filter(|rate| *rate != 0.0 && !rate.is_nan())
        .unwrap_or(speed_change);
    Ok(CalculateMods {
        checksum: mods_checksum(&array),
        number,
        name: replace_aliases(parts.concat()),
        array,
        rate,
    })
}

pub fn sanitize_mods(mods: &[Mod]) -> Vec<Mod> {
    let mut sanitized: Vec<_> = mods
        .iter()
        .filter(|entry| !matches!(entry.acronym.as_str(), "RX" | "AP"))
        .cloned()
        .collect();
    if let Some(index) = sanitized
        .iter()
        .position(|entry| entry.acronym.eq_ignore_ascii_case("v2"))
    {
        sanitized[index] = Mod {
            acronym: "SV2".into(),
            settings: None,
        };
    }
    sanitized
}

pub fn mods_for_calculation(mods: &[Mod], is_lazer: bool) -> Vec<Mod> {
    let mut sanitized = sanitize_mods(mods);
    if !is_lazer {
        sanitized.push(Mod {
            acronym: "CL".into(),
            settings: Some(IndexMap::new()),
        });
    }
    sanitized
}
