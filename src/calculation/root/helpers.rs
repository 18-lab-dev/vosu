use std::path::MAIN_SEPARATOR;

use chrono::{DateTime, Utc};

use super::mods::js_number;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("decimal amount must be between 0 and 100")]
pub struct DecimalPlacesError;

pub fn fix_decimals(value: f64) -> f64 {
    fix_decimals_to(value, 2).expect("two decimals are valid")
}
pub fn fix_decimals_to(value: f64, amount: u32) -> Result<f64, DecimalPlacesError> {
    if amount > 100 {
        return Err(DecimalPlacesError);
    }
    let value = if value == 0.0 || value.is_nan() {
        0.0
    } else {
        value
    };
    if value.is_infinite() || value.abs() >= 1e21 {
        return Ok(value);
    }
    if let Some(rounded) = exact_fixed_round(value, amount) {
        return Ok(rounded);
    }
    Ok(format!("{value:.amount$}", amount = amount as usize)
        .parse()
        .expect("fixed-point output is numeric"))
}

/// Round the exact binary value as ECMA-262 `Number::toFixed` does. Keeping the
/// integer arithmetic bounded covers every precision that can change a parsed
/// f64 around ordinary vosu values; larger results fall back to formatting.
fn exact_fixed_round(value: f64, amount: u32) -> Option<f64> {
    if amount > 22 {
        return None;
    }
    let bits = value.abs().to_bits();
    let raw_exponent = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1_u64 << 52) - 1);
    let (significand, exponent) = if raw_exponent == 0 {
        (fraction, -1074)
    } else {
        ((1_u64 << 52) | fraction, raw_exponent - 1023 - 52)
    };
    let power_five = 5_u128.checked_pow(amount)?;
    let numerator = (significand as u128).checked_mul(power_five)?;
    let shift = exponent + amount as i32;
    let integer = if shift >= 0 {
        numerator.checked_shl(shift as u32)?
    } else {
        let right = (-shift) as u32;
        if right >= 128 {
            0
        } else {
            let quotient = numerator >> right;
            let remainder_mask = (1_u128 << right) - 1;
            let remainder = numerator & remainder_mask;
            quotient + u128::from(right != 0 && remainder >= (1_u128 << (right - 1)))
        }
    };
    let mut text = integer.to_string();
    if amount != 0 {
        let amount = amount as usize;
        if text.len() <= amount {
            text.insert_str(0, &"0".repeat(amount + 1 - text.len()));
        }
        text.insert(text.len() - amount, '.');
    }
    if value.is_sign_negative() {
        text.insert(0, '-');
    }
    text.parse().ok()
}

pub fn net_date_binary_to_date(date_data_hi: i32, date_data_lo: i32) -> Option<DateTime<Utc>> {
    let high = (date_data_hi as u32 & 0x3fff_ffff) as u64;
    let date_data = (high << 32) | date_data_lo as u32 as u64;
    let milliseconds = (date_data as i128 - 621_355_968_000_000_000_i128) / 10_000;
    let milliseconds = i64::try_from(milliseconds).ok()?;
    DateTime::from_timestamp_millis(milliseconds)
}

const DOUBLE_POWERS_10: [f64; 81] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22, 1e23, 1e24, 1e25, 1e26, 1e27, 1e28, 1e29, 1e30, 1e31, 1e32,
    1e33, 1e34, 1e35, 1e36, 1e37, 1e38, 1e39, 1e40, 1e41, 1e42, 1e43, 1e44, 1e45, 1e46, 1e47, 1e48,
    1e49, 1e50, 1e51, 1e52, 1e53, 1e54, 1e55, 1e56, 1e57, 1e58, 1e59, 1e60, 1e61, 1e62, 1e63, 1e64,
    1e65, 1e66, 1e67, 1e68, 1e69, 1e70, 1e71, 1e72, 1e73, 1e74, 1e75, 1e76, 1e77, 1e78, 1e79, 1e80,
];
pub fn number_from_decimal(lo64: f64, hi32: u32, flags: i32) -> f64 {
    if lo64 == 0.0 && hi32 == 0 && flags == 0 {
        return 0.0;
    }
    let negative = ((flags >> 24) & 0xff) != 0;
    let scale = ((flags >> 16) & 0xff) as usize;
    let divisor = DOUBLE_POWERS_10.get(scale).copied().unwrap_or(f64::NAN);
    let value = (lo64 + hi32 as f64 * 1.844_674_407_370_955_2e19) / divisor;
    if negative { -value } else { value }
}

/// JavaScript `Math.round`, retaining the number domain for callers whose
/// source data can be non-finite.
pub(crate) fn js_round_number(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 || value.fract() == 0.0 {
        return value;
    }
    // ECMAScript preserves negative zero when the nearest integer is zero
    // and ties are resolved toward positive infinity.
    if (-0.5..0.0).contains(&value) {
        return -0.0;
    }
    let lower = value.floor();
    if value - lower < 0.5 {
        lower
    } else {
        lower + 1.0
    }
}

pub fn safe_join<I, S>(paths: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    safe_join_with_separator(paths, MAIN_SEPARATOR)
}

/// Serialize a JavaScript number as `JSON.stringify` does.
pub fn js_json_number(value: f64) -> String {
    if !value.is_finite() {
        return "null".into();
    }
    if value == 0.0 {
        return "0".into();
    }
    let number = serde_json::Number::from_f64(value).expect("finite number");
    js_number(&number)
}

pub fn safe_join_with_separator<I, S>(paths: I, separator: char) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut joined = String::new();
    let mut absolute = false;
    let mut saw_path = false;
    for path in paths {
        let cleaned = path
            .as_ref()
            .trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}');
        if cleaned.is_empty() {
            continue;
        }
        let cleaned = cleaned.replace(['/', '\\'], &separator.to_string());
        if !saw_path {
            absolute = cleaned.starts_with(separator);
            saw_path = true;
        }
        if !joined.is_empty() && !joined.ends_with(separator) {
            joined.push(separator);
        }
        joined.push_str(cleaned.trim_start_matches(separator));
    }
    if absolute {
        joined.insert(0, separator);
    }
    normalize_path(&joined, separator)
}

fn normalize_path(path: &str, separator: char) -> String {
    if separator == '\\' {
        return normalize_windows_path(path);
    }
    if path.is_empty() {
        return ".".into();
    }
    let absolute = path.starts_with(separator);
    let trailing = path.ends_with(separator);
    let mut components: Vec<&str> = Vec::new();
    for component in path.split(separator) {
        match component {
            "" | "." => {}
            ".." if components.last().is_some_and(|last| *last != "..") => {
                components.pop();
            }
            ".." if !absolute => components.push(component),
            ".." => {}
            value => components.push(value),
        }
    }
    let sep = separator.to_string();
    let mut result = components.join(&sep);
    if absolute {
        result.insert(0, separator);
    }
    if result.is_empty() {
        result.push(if absolute { separator } else { '.' });
    }
    if trailing && result != separator.to_string() && result != "." {
        result.push(separator);
    }
    result
}

fn normalize_windows_path(path: &str) -> String {
    const RESERVED: [&str; 28] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "COM¹", "COM²", "COM³", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7",
        "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³",
    ];
    if path.is_empty() {
        return ".".into();
    }
    let bytes = path.as_bytes();
    let colon = path.find(':');
    let drive = colon == Some(1) && bytes[0].is_ascii_alphabetic();
    let reserved =
        colon.is_some_and(|index| RESERVED.contains(&path[..index].to_ascii_uppercase().as_str()));
    let device_end = if drive || reserved {
        colon.map(|index| index + 1)
    } else {
        None
    };
    let rooted_drive = drive && bytes.get(2) == Some(&b'\\');
    let absolute = !drive && !reserved && path.starts_with('\\');
    let trailing = path.ends_with('\\');
    let (device, body) = if let Some(end) = device_end {
        (&path[..end], path[end..].trim_start_matches('\\'))
    } else {
        ("", path.trim_start_matches('\\'))
    };
    let mut components: Vec<&str> = Vec::new();
    for component in body.split('\\') {
        match component {
            "" | "." => {}
            ".." if components.last().is_some_and(|last| *last != "..") => {
                components.pop();
            }
            ".." if !rooted_drive && !absolute => components.push(component),
            ".." => {}
            value => components.push(value),
        }
    }
    let joined = components.join("\\");
    let mut tail = if joined.is_empty() && !rooted_drive && !absolute {
        ".".into()
    } else {
        joined
    };
    if trailing && !tail.is_empty() && !tail.ends_with('\\') {
        tail.push('\\');
    }
    if reserved {
        return format!(".\\{device}{tail}");
    }
    if drive {
        return if rooted_drive {
            format!("{device}\\{tail}")
        } else {
            format!("{device}{tail}")
        };
    }
    if !absolute && path.contains(':') {
        let looks_like_drive = tail.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && tail.as_bytes().get(1) == Some(&b':');
        let unsafe_colon = path.match_indices(':').any(|(index, _)| {
            index + 1 == path.len() || path.as_bytes().get(index + 1) == Some(&b'\\')
        });
        if looks_like_drive || unsafe_colon {
            return format!(".\\{tail}");
        }
    }
    if absolute { format!("\\{tail}") } else { tail }
}
