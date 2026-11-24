//! JS-number JSON formatting (D13; `docs/v2/state-schema.md` "write_json"):
//! `write_json`'s only number convention, matching `JSON.stringify` and
//! 1.x's own `write_js_json_number`/`finite_ecma_number`
//! (`crates/vosu-osu-model/src/root/{helpers.rs:141-151, mods.rs:313-440}`).
//!
//! There is no shared production crate below core and osu-model (only the
//! test-support crate `vosu-proptest` is common). Keep this game-agnostic copy
//! here; `tests/state_number_compat.rs` runs both writers over shared boundary
//! vectors and compares their outputs directly.

use std::sync::Arc;

use zmij::Buffer;

/// Writes `value` the way `JSON.stringify` (and 1.x) would: a non-finite
/// value writes `null`, `0.0`/`-0.0` write `0`, and every other finite value
/// writes ECMA-262 `Number::toString`'s shortest round-tripping decimal.
pub(super) fn write_js_number(out: &mut Vec<u8>, value: f64) {
    if !value.is_finite() {
        out.extend_from_slice(b"null");
    } else if value == 0.0 {
        out.push(b'0');
    } else {
        out.extend_from_slice(finite_ecma_number(value).as_bytes());
    }
}

/// ECMA-262 `Number::toString` for one finite, non-zero `f64`: shortest
/// round-tripping digits (`zmij`, a `ryu`-family formatter) reshaped into
/// JS's own decimal/exponential split (`point > 21` or `point <= -6` goes
/// exponential; `toString()`, not `toFixed`/`toPrecision`).
fn finite_ecma_number(value: f64) -> Arc<str> {
    debug_assert!(value.is_finite() && value != 0.0);

    let negative = value.is_sign_negative();
    let mut raw_buffer = Buffer::new();
    let raw = raw_buffer.format_finite(value.abs()).as_bytes();
    let exponent_index = raw.iter().position(|byte| matches!(byte, b'e' | b'E'));
    let mantissa = &raw[..exponent_index.unwrap_or(raw.len())];
    let exponent = exponent_index.map_or(0, |index| parse_decimal(&raw[index + 1..]));
    let point_before_exponent = mantissa
        .iter()
        .position(|byte| *byte == b'.')
        .unwrap_or(mantissa.len()) as i32;

    let mut digits = [0_u8; 24];
    let mut digits_len = 0;
    for &byte in mantissa {
        if byte != b'.' {
            digits[digits_len] = byte;
            digits_len += 1;
        }
    }
    let leading = digits[..digits_len]
        .iter()
        .take_while(|byte| **byte == b'0')
        .count();
    let mut trailing = digits_len;
    while trailing > leading + 1 && digits[trailing - 1] == b'0' {
        trailing -= 1;
    }
    let digits = &digits[leading..trailing];
    let point = point_before_exponent + exponent - leading as i32;

    let mut out: Vec<u8> = Vec::with_capacity(32);
    if negative {
        out.push(b'-');
    }
    if point > 21 || point <= -6 {
        out.push(digits[0]);
        if digits.len() > 1 {
            out.push(b'.');
            out.extend_from_slice(&digits[1..]);
        }
        out.push(b'e');
        let scientific_exponent = point - 1;
        out.push(if scientific_exponent >= 0 { b'+' } else { b'-' });
        push_decimal(&mut out, scientific_exponent.unsigned_abs());
    } else if point <= 0 {
        out.extend_from_slice(b"0.");
        out.extend(std::iter::repeat_n(b'0', (-point) as usize));
        out.extend_from_slice(digits);
    } else if point as usize >= digits.len() {
        out.extend_from_slice(digits);
        out.extend(std::iter::repeat_n(b'0', point as usize - digits.len()));
    } else {
        out.extend_from_slice(&digits[..point as usize]);
        out.push(b'.');
        out.extend_from_slice(&digits[point as usize..]);
    }
    Arc::from(
        String::from_utf8(out)
            .expect("output is ASCII")
            .into_boxed_str(),
    )
}

fn push_decimal(out: &mut Vec<u8>, mut value: u32) {
    let mut reversed = [0_u8; 3];
    let mut len = 0;
    loop {
        reversed[len] = b'0' + (value % 10) as u8;
        len += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend(reversed[..len].iter().rev());
}

fn parse_decimal(bytes: &[u8]) -> i32 {
    let (negative, digits) = match bytes.first() {
        Some(b'-') => (true, &bytes[1..]),
        Some(b'+') => (false, &bytes[1..]),
        _ => (false, bytes),
    };
    let value = digits
        .iter()
        .fold(0_i32, |value, byte| value * 10 + i32::from(byte - b'0'));
    if negative { -value } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(value: f64) -> String {
        let mut out = Vec::new();
        write_js_number(&mut out, value);
        String::from_utf8(out).unwrap()
    }

    /// The same vectors ReviewK2KT's finding checked against 1.x
    /// `write_js_json_number`/`JSON.stringify`.
    #[test]
    fn matches_js_json_stringify_vectors() {
        for &(value, expected) in vosu_proptest::ECMA_NUMBER_CASES {
            assert_eq!(write(value), expected, "value {value:?}");
        }
        assert_eq!(write(f64::NAN), "null");
        assert_eq!(write(f64::INFINITY), "null");
        assert_eq!(write(f64::NEG_INFINITY), "null");
        assert_eq!(write(0.0), "0");
        assert_eq!(write(-0.0), "0");
        assert_eq!(write(1.5), "1.5");
        assert_eq!(write(1e21), "1e+21");
        assert_eq!(write(1e-7), "1e-7");
        assert_eq!(write(100.0), "100");
        assert_eq!(write(-100.0), "-100");
        assert_eq!(write(0.1), "0.1");
        assert_eq!(write(123_456_789.0), "123456789");
    }
}
