use super::*;

pub(super) fn empty_stats() -> Statistics {
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
pub(super) fn default_stats() -> Statistics {
    let mut value = empty_stats();
    value.legacy_combo_increase = Some(0);
    value
}
pub(super) fn score_has_started(play_time: f64, first_object_time: Option<i64>) -> bool {
    first_object_time.is_some_and(|first| play_time >= first.saturating_sub(100) as f64)
}
pub(super) fn js_long(value: i64) -> f64 {
    value as f64
}
pub(super) fn legacy_stats(
    mode: i64,
    h300: i64,
    h100: i64,
    h50: i64,
    miss: i64,
    geki: i64,
    katu: i64,
) -> Statistics {
    let mut s = empty_stats();
    s.great = h300;
    s.miss = miss;
    match mode {
        1 => {
            s.ok = h100;
            s.large_bonus = geki + katu
        }
        2 => {
            s.large_tick_hit = h100;
            s.small_tick_hit = h50;
            s.small_tick_miss = katu
        }
        3 => {
            s.perfect = geki;
            s.good = katu;
            s.ok = h100;
            s.meh = h50
        }
        _ => {
            s.ok = h100;
            s.meh = h50
        }
    };
    s
}
pub(super) fn accuracy(mode: i64, s: &Statistics, mods: &CalculateMods) -> f64 {
    let (n, d) = match mode {
        1 => (2 * s.great + s.ok, 2 * (s.great + s.ok + s.miss)),
        2 => (
            s.great + s.ok + s.meh,
            s.great + s.ok + s.meh + s.good + s.miss,
        ),
        3 => {
            let weight = if mods.array.iter().any(|m| m.acronym == "V2") {
                61
            } else {
                60
            };
            (
                weight * s.perfect + 60 * s.great + 40 * s.good + 20 * s.ok + 10 * s.meh,
                weight * (s.perfect + s.great + s.good + s.ok + s.meh + s.miss),
            )
        }
        _ => (
            6 * s.great + 2 * s.ok + s.meh,
            6 * (s.great + s.ok + s.meh + s.miss),
        ),
    };
    if d == 0 {
        0.0
    } else {
        ((n as f64 / d as f64) * 10_000.0).round() / 100.0
    }
}
fn mod_checksum(array: &[Mod]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Md5::digest(serde_json::to_vec(array).unwrap_or_default());
    let mut checksum = String::with_capacity(32);
    for byte in digest {
        checksum.push(HEX[(byte >> 4) as usize] as char);
        checksum.push(HEX[(byte & 0x0f) as usize] as char);
    }
    checksum
}

pub(super) fn mods(bits: i64) -> CalculateMods {
    const VALUES: [(i64, &str); 31] = [
        (1, "NF"),
        (2, "EZ"),
        (4, "TD"),
        (8, "HD"),
        (16, "HR"),
        (32, "SD"),
        (64, "DT"),
        (128, "RX"),
        (256, "HT"),
        (512, "NC"),
        (1024, "FL"),
        (2048, "AT"),
        (4096, "SO"),
        (8192, "AP"),
        (16384, "PF"),
        (32768, "4K"),
        (65536, "5K"),
        (131072, "6K"),
        (262144, "7K"),
        (524288, "8K"),
        (1048576, "FI"),
        (2097152, "RD"),
        (4194304, "CN"),
        (8388608, "TG"),
        (16777216, "9K"),
        (33554432, "10K"),
        (67108864, "1K"),
        (134217728, "3K"),
        (268435456, "2K"),
        (536870912, "v2"),
        (1073741824, "MR"),
    ];
    fn order(acronym: &str) -> u8 {
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
    let mut acronyms: Vec<&str> = VALUES
        .iter()
        .filter_map(|(bit, acronym)| (bits & bit != 0).then_some(*acronym))
        .collect();
    acronyms.sort_by_key(|acronym| order(acronym));
    let name = acronyms
        .join("")
        .replacen("DTNC", "NC", 1)
        .replacen("SDPF", "PF", 1)
        .replacen("ATCN", "CN", 1);
    let array = name
        .as_bytes()
        .chunks(2)
        .map(|chunk| Mod {
            acronym: String::from_utf8_lossy(chunk).to_ascii_uppercase(),
            settings: None,
        })
        .collect::<Vec<_>>();
    let checksum = mod_checksum(&array);
    let rate = if array
        .iter()
        .any(|item| item.acronym == "DT" || item.acronym == "NC")
    {
        1.5
    } else if array.iter().any(|item| item.acronym == "HT") {
        0.75
    } else {
        1.0
    };
    CalculateMods {
        checksum,
        number: bits,
        name,
        array,
        rate,
    }
}
pub(super) fn mods_from_array(array: Vec<Mod>) -> CalculateMods {
    let mut number = 0i64;
    let mut converted = Vec::with_capacity(array.len());
    let mut mapped_rate = 1.0;
    for item in &array {
        let lower = item.acronym.to_ascii_lowercase();
        let (bit, name, rate) = match lower.as_str() {
            "nf" => (1, "NF", 1.0),
            "ez" => (2, "EZ", 1.0),
            "td" => (4, "TD", 1.0),
            "hd" => (8, "HD", 1.0),
            "hr" => (16, "HR", 1.0),
            "sd" => (32, "SD", 1.0),
            "dt" => (64, "DT", 1.5),
            "rx" => (128, "RX", 1.0),
            "ht" => (256, "HT", 0.75),
            "nc" => (576, "NC", 1.5),
            "fl" => (1024, "FL", 1.0),
            "at" => (2048, "AT", 1.0),
            "so" => (4096, "SO", 1.0),
            "ap" => (8192, "AP", 1.0),
            "pf" => (16416, "PF", 1.0),
            "4k" => (32768, "4K", 1.0),
            "5k" => (65536, "5K", 1.0),
            "6k" => (131072, "6K", 1.0),
            "7k" => (262144, "7K", 1.0),
            "8k" => (524288, "8K", 1.0),
            "fi" => (1048576, "FI", 1.0),
            "rd" => (2097152, "RD", 1.0),
            "cn" => (4194304, "CN", 1.0),
            "target" => (8388608, "TG", 1.0),
            "9k" => (16777216, "9K", 1.0),
            "keycoop" => (33554432, "10K", 1.0),
            "1k" => (67108864, "1K", 1.0),
            "3k" => (134217728, "3K", 1.0),
            "2k" => (268435456, "2K", 1.0),
            "scorev2" => (536870912, "v2", 1.0),
            "mr" => (1073741824, "MR", 1.0),
            _ => (0, item.acronym.as_str(), 1.0),
        };
        number += bit;
        if rate != 1.0 {
            mapped_rate = rate
        }
        converted.push(name.to_owned());
    }
    converted.sort_by_key(|acronym| match acronym.to_ascii_lowercase().as_str() {
        "ez" => 1,
        "hd" => 2,
        "dt" | "nc" | "ht" => 3,
        "hr" => 4,
        "so" | "sd" | "pf" => 5,
        "fl" => 6,
        "td" => 7,
        _ => 99,
    });
    let name = converted
        .join("")
        .replacen("DTNC", "NC", 1)
        .replacen("SDPF", "PF", 1)
        .replacen("ATCN", "CN", 1);
    let rate = array
        .iter()
        .find_map(|item| item.settings.as_ref()?.get("speed_change")?.as_f64())
        .filter(|value| *value != 0.0)
        .unwrap_or(mapped_rate);
    let checksum = mod_checksum(&array);
    CalculateMods {
        checksum,
        number,
        name,
        array,
        rate,
    }
}
pub(super) fn mime(name: &str) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let value = match ext {
        "html" => "text/html",
        "js" => "text/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" => "image/jpg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "woff" => "application/font-woff",
        "ttf" => "application/font-ttf",
        "eot" => "application/vnd.ms-fontobject",
        "otf" => "application/font-otf",
        "wasm" => "application/wasm",
        "aac" => "audio/aac",
        "abw" => "application/x-abiword",
        "apng" => "image/apng",
        "arc" => "application/x-freearc",
        "avif" => "image/avif",
        "avi" => "video/x-msvideo",
        "bin" => "application/octet-stream",
        "bmp" => "image/bmp",
        "bz" => "application/x-bzip",
        "bz2" => "application/x-bzip2",
        "cda" => "application/x-cdf",
        "csv" => "text/csv",
        "gz" => "application/gzip",
        "htm" => "text/html",
        "ico" => "image/vnd.microsoft.icon",
        "jpeg" => "image/jpeg",
        "jsonld" => "application/ld+json",
        "mid" | "midi" => "audio/x-midi",
        "mjs" => "text/javascript",
        "mp3" => "audio/mpeg",
        "mpeg" => "video/mpeg",
        "mpkg" => "application/vnd.apple.installer+xml",
        "ogg" | "oga" => "audio/ogg",
        "ogv" => "video/ogg",
        "ogx" => "application/ogg",
        "opus" => "audio/opus",
        "rar" => "application/vnd.rar",
        "sh" => "application/x-sh",
        "tar" => "application/x-tar",
        "tif" | "tiff" => "image/tiff",
        "ts" => "video/mp2t",
        "txt" | "osu" => "text/plain",
        "weba" => "audio/webm",
        "webm" => "video/webm",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "xhtml" => "application/xhtml+xml",
        "xml" => "application/xmlz",
        "xul" => "application/vnd.mozilla.xul+xml",
        "zip" => "application/zip",
        "3gp" => "video/3gpp;",
        "3g2" => "video/3gpp2;",
        "7z" => "application/x-7z-compressed",
        "swf" => "application/x-shockwave-flash",
        _ => return String::new(),
    };
    format!("{value}; charset=utf-8")
}
pub(super) fn net_date(high: i32, low: i32) -> Result<String, MemoryError> {
    net_date_binary_to_date(high, low)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or(MemoryError::Corrupt {
            context: "stable result date",
            address: 0,
            message: "timestamp is outside chrono range",
        })
}

pub(super) fn stable_ranked_status(raw_status: i64) -> f64 {
    raw_status as f64
}
