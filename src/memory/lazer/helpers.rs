use super::*;

pub(super) fn add(base: usize, offset: usize) -> Result<usize, MemoryError> {
    base.checked_add(offset)
        .ok_or(MemoryError::AddressOverflow { base, offset })
}
pub(super) fn corrupt(context: &'static str, address: usize, message: &'static str) -> MemoryError {
    MemoryError::Corrupt {
        context,
        address,
        message,
    }
}
pub(super) fn default_mods() -> CalculateMods {
    CalculateMods {
        checksum: String::new(),
        number: 0,
        name: String::new(),
        array: Vec::new(),
        rate: 1.0,
    }
}
pub(super) fn calculate_mods(mut array: Vec<Mod>) -> CalculateMods {
    let mut number = 0i64;
    let mut names = Vec::with_capacity(array.len());
    let mut legacy_rate = 1.0;
    for item in &mut array {
        if let Some(settings) = item.settings.as_mut() {
            for (key, value) in settings.iter_mut() {
                if let Some(raw) = value.as_f64()
                    && let Some(number) = Number::from_f64(js_fixed2(raw))
                {
                    *value = Value::Number(number)
                }
                if matches!(
                    key.as_str(),
                    "accuracy_judge_mode" | "reflection" | "direction" | "style"
                ) && !value.is_string()
                {
                    *value = Value::String(js_setting_string(value))
                }
            }
        }
        let lower = item.acronym.to_ascii_lowercase();
        let (bit, name) = match lower.as_str() {
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
            _ => (0, item.acronym.as_str()),
        };
        if matches!(lower.as_str(), "dt" | "nc") {
            legacy_rate = 1.5
        } else if lower == "ht" {
            legacy_rate = 0.75
        }
        number += bit;
        names.push(if bit == 0 {
            item.acronym.to_ascii_uppercase()
        } else {
            name.to_owned()
        });
    }
    if legacy_rate == 1.0
        && array
            .iter()
            .any(|item| matches!(item.acronym.as_str(), "DT" | "NC"))
    {
        legacy_rate = 1.5
    }
    if legacy_rate == 1.0
        && array
            .iter()
            .any(|item| matches!(item.acronym.as_str(), "HT" | "DC"))
    {
        legacy_rate = 0.75
    }
    names.sort_by_key(|name| mod_order(name));
    let name = names
        .concat()
        .replacen("DTNC", "NC", 1)
        .replacen("SDPF", "PF", 1)
        .replacen("ATCN", "CN", 1);
    let settings_rate = array
        .iter()
        .find_map(|item| item.settings.as_ref()?.get("speed_change")?.as_f64())
        .filter(|value| *value != 0.0);
    let checksum = crate::calculation::mods_checksum(&array);
    CalculateMods {
        checksum,
        number,
        name,
        array,
        rate: settings_rate.unwrap_or(legacy_rate),
    }
}
pub(super) fn js_setting_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Number(value) => value
            .as_f64()
            .filter(|number| number.fract() == 0.0)
            .map(|number| format!("{number:.0}"))
            .unwrap_or_else(|| value.to_string()),
        Value::Array(values) => values
            .iter()
            .map(js_setting_string)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}
pub(super) fn mod_order(name: &str) -> u8 {
    match name.to_ascii_lowercase().as_str() {
        "nf" => 99,
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
pub(super) fn country_code(code: &str) -> i64 {
    const CODES: [&str; 252] = [
        "oc", "eu", "ad", "ae", "af", "ag", "ai", "al", "am", "an", "ao", "aq", "ar", "as", "at",
        "au", "aw", "az", "ba", "bb", "bd", "be", "bf", "bg", "bh", "bi", "bj", "bm", "bn", "bo",
        "br", "bs", "bt", "bv", "bw", "by", "bz", "ca", "cc", "cd", "cf", "cg", "ch", "ci", "ck",
        "cl", "cm", "cn", "co", "cr", "cu", "cv", "cx", "cy", "cz", "de", "dj", "dk", "dm", "do",
        "dz", "ec", "ee", "eg", "eh", "er", "es", "et", "fi", "fj", "fk", "fm", "fo", "fr", "fx",
        "ga", "gb", "gd", "ge", "gf", "gh", "gi", "gl", "gm", "gn", "gp", "gq", "gr", "gs", "gt",
        "gu", "gw", "gy", "hk", "hm", "hn", "hr", "ht", "hu", "id", "ie", "il", "in", "io", "iq",
        "ir", "is", "it", "jm", "jo", "jp", "ke", "kg", "kh", "ki", "km", "kn", "kp", "kr", "kw",
        "ky", "kz", "la", "lb", "lc", "li", "lk", "lr", "ls", "lt", "lu", "lv", "ly", "ma", "mc",
        "md", "mg", "mh", "mk", "ml", "mm", "mn", "mo", "mp", "mq", "mr", "ms", "mt", "mu", "mv",
        "mw", "mx", "my", "mz", "na", "nc", "ne", "nf", "ng", "ni", "nl", "no", "np", "nr", "nu",
        "nz", "om", "pa", "pe", "pf", "pg", "ph", "pk", "pl", "pm", "pn", "pr", "ps", "pt", "pw",
        "py", "qa", "re", "ro", "ru", "rw", "sa", "sb", "sc", "sd", "se", "sg", "sh", "si", "sj",
        "sk", "sl", "sm", "sn", "so", "sr", "st", "sv", "sy", "sz", "tc", "td", "tf", "tg", "th",
        "tj", "tk", "tm", "tn", "to", "tl", "tr", "tt", "tv", "tw", "tz", "ua", "ug", "um", "us",
        "uy", "uz", "va", "vc", "ve", "vg", "vi", "vn", "vu", "wf", "ws", "ye", "yt", "rs", "za",
        "zm", "me", "zw", "xx", "a2", "o1", "ax", "gg", "im", "je", "bl", "mf",
    ];
    CODES
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(code))
        .map_or(0, |index| index as i64 + 1)
}
pub(super) fn lazer_ranked_status(raw_status: i32) -> f64 {
    match raw_status {
        -4 | -3 => 1.0,
        -2..=0 => 2.0,
        1 => 4.0,
        2 => 5.0,
        3 => 6.0,
        4 => 7.0,
        _ => f64::NAN,
    }
}

pub(super) fn empty_statistics() -> Statistics {
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
        legacy_combo_increase: Some(0),
    }
}
pub(super) fn set_stat(s: &mut Statistics, key: i32, v: i64) {
    match key {
        1 => s.miss = v,
        2 => s.meh = v,
        3 => s.ok = v,
        4 => s.good = v,
        5 => s.great = v,
        6 => s.perfect = v,
        7 => s.small_tick_miss = v,
        8 => s.small_tick_hit = v,
        9 => s.large_tick_miss = v,
        10 => s.large_tick_hit = v,
        11 => s.small_bonus = v,
        12 => s.large_bonus = v,
        13 => s.ignore_miss = v,
        14 => s.ignore_hit = v,
        15 => s.combo_break = v,
        16 => s.slider_tail_hit = v,
        99 => s.legacy_combo_increase = Some(v),
        _ => {}
    }
}
pub(super) fn object_count(s: &Statistics) -> i64 {
    s.meh + s.ok + s.good + s.great + s.perfect + s.miss
}
/// Decodes a score's `ModsJson` text; empty text means no mods.
pub(super) fn mods_from_json(score_info: usize, text: &str) -> Result<CalculateMods, MemoryError> {
    if text.is_empty() {
        return Ok(default_mods());
    }
    let array = serde_json::from_str::<Vec<Mod>>(text)
        .map_err(|_| corrupt("mods json", score_info, "invalid JSON"))?;
    if array
        .iter()
        .filter_map(|item| item.settings.as_ref())
        .any(|settings| {
            ["accuracy_judge_mode", "reflection", "direction", "style"]
                .iter()
                .any(|key| settings.get(*key).is_some_and(Value::is_null))
        })
    {
        return Err(corrupt("mods json", score_info, "null string setting"));
    }
    Ok(calculate_mods(array))
}
/// Maps lazer's built-in bot names to the names the source publishes.
pub(super) fn display_username(username: String) -> String {
    match username.as_str() {
        "Autoplay" => "osu!".into(),
        "osu!salad" => "salad!".into(),
        "osu!topus" => "osu!topus!".into(),
        _ => username,
    }
}
pub(super) fn required_non_null(
    value: usize,
    context: &'static str,
    address: usize,
) -> Result<usize, MemoryError> {
    if value == 0 {
        Err(corrupt(context, address, "null pointer"))
    } else {
        Ok(value)
    }
}
// Direct comparisons intentionally treat NaN as in range, matching the scanner's prior behavior.
#[allow(clippy::manual_range_contains)]
pub(super) fn hit_error_out_of_range(value: f64) -> bool {
    value < -10_000.0 || value > 10_000.0
}
pub(super) fn spectator_user_state(
    states: &BTreeMap<i64, (MultiplayerTeamType, i32)>,
    user_id: i64,
) -> Result<(MultiplayerTeamType, i32), MemoryError> {
    states
        .get(&user_id)
        .cloned()
        .ok_or_else(|| corrupt("spectator room user", user_id as usize, "missing metadata"))
}
pub(super) fn net_date(process: &ProcessHandle, address: usize) -> Result<String, MemoryError> {
    let raw = process.read_u64(address)?;
    net_date_binary_to_date((raw >> 32) as i32, raw as u32 as i32)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| corrupt("lazer date", address, "timestamp is outside chrono range"))
}
pub(super) fn js_long(value: i64) -> f64 {
    value as f64
}
pub(super) fn display_score(mode: i64, score: f64, objects: i64) -> f64 {
    let n = objects as f64;
    match mode {
        0 => js_round_number((n * n * 32.57 + 100_000.0) * score / MAX_SCORE),
        1 => js_round_number((n * 1109.0 + 100_000.0) * score / MAX_SCORE),
        2 => js_round_number(((score / MAX_SCORE * n).powi(2) * 21.62) + score / 10.0),
        _ => score,
    }
}
pub(super) fn lazer_path(hash: &str) -> Result<String, MemoryError> {
    if hash.is_empty() {
        return Ok(String::new());
    }
    if !matches!(hash.len(), 32 | 64) || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MemoryError::Corrupt {
            context: "resource hash",
            address: 0,
            message: "expected an ASCII hexadecimal MD5 or SHA-256 digest",
        });
    }
    Ok(Path::new(&hash[..1])
        .join(&hash[..2])
        .join(hash)
        .to_string_lossy()
        .into_owned())
}
pub(super) fn mime(name: &str) -> String {
    let basename = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let ext = basename
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, e)| e)
        .unwrap_or_default();
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png; charset=utf-8",
        "jpg" => "image/jpg; charset=utf-8",
        "jpeg" => "image/jpeg; charset=utf-8",
        "gif" => "image/gif; charset=utf-8",
        "svg" => "image/svg+xml; charset=utf-8",
        "wav" => "audio/wav; charset=utf-8",
        "mp4" => "video/mp4; charset=utf-8",
        "woff" => "application/font-woff; charset=utf-8",
        "woff2" => "font/woff2; charset=utf-8",
        "ttf" => "application/font-ttf; charset=utf-8",
        "eot" => "application/vnd.ms-fontobject; charset=utf-8",
        "otf" => "application/font-otf; charset=utf-8",
        "wasm" => "application/wasm; charset=utf-8",
        "aac" => "audio/aac; charset=utf-8",
        "abw" => "application/x-abiword; charset=utf-8",
        "apng" => "image/apng; charset=utf-8",
        "arc" => "application/x-freearc; charset=utf-8",
        "avif" => "image/avif; charset=utf-8",
        "avi" => "video/x-msvideo; charset=utf-8",
        "bin" => "application/octet-stream; charset=utf-8",
        "bmp" => "image/bmp; charset=utf-8",
        "bz" => "application/x-bzip; charset=utf-8",
        "bz2" => "application/x-bzip2; charset=utf-8",
        "cda" => "application/x-cdf; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "gz" => "application/gzip; charset=utf-8",
        "ico" => "image/vnd.microsoft.icon; charset=utf-8",
        "jsonld" => "application/ld+json; charset=utf-8",
        "mid" | "midi" => "audio/x-midi; charset=utf-8",
        "mp3" => "audio/mpeg; charset=utf-8",
        "mpeg" => "video/mpeg; charset=utf-8",
        "mpkg" => "application/vnd.apple.installer+xml; charset=utf-8",
        "ogg" | "oga" => "audio/ogg; charset=utf-8",
        "ogv" => "video/ogg; charset=utf-8",
        "ogx" => "application/ogg; charset=utf-8",
        "opus" => "audio/opus; charset=utf-8",
        "rar" => "application/vnd.rar; charset=utf-8",
        "sh" => "application/x-sh; charset=utf-8",
        "tar" => "application/x-tar; charset=utf-8",
        "tif" | "tiff" => "image/tiff; charset=utf-8",
        "ts" => "video/mp2t; charset=utf-8",
        "txt" | "osu" => "text/plain; charset=utf-8",
        "weba" => "audio/webm; charset=utf-8",
        "webm" => "video/webm; charset=utf-8",
        "webp" => "image/webp; charset=utf-8",
        "xhtml" => "application/xhtml+xml; charset=utf-8",
        "xml" => "application/xmlz; charset=utf-8",
        "xul" => "application/vnd.mozilla.xul+xml; charset=utf-8",
        "zip" => "application/zip; charset=utf-8",
        "3gp" => "video/3gpp;; charset=utf-8",
        "3g2" => "video/3gpp2;; charset=utf-8",
        "7z" => "application/x-7z-compressed; charset=utf-8",
        "swf" => "application/x-shockwave-flash; charset=utf-8",
        _ => "",
    }
    .into()
}
pub(super) fn js_fixed2(value: f64) -> f64 {
    crate::calculation::fix_decimals(value)
}
