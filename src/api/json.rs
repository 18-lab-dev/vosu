use std::io::{self, Write};

use serde::Serialize;
use serde_json::ser::{CompactFormatter, Formatter, Serializer};

use super::ApiError;

/// Serializer formatter matching JavaScript JSON number spelling.
///
/// General ECMAScript number conversion remains owned by `calculation`; this
/// is the single adapter used by every serde-based API wire encoder. It does
/// not route through [`write_js_number`]: on the cold v1 document (almost all
/// zeros) interleaved measurements showed that hook costing ~3%, pushing it
/// past the regression budget, while only the hand-written writers gain.
pub(crate) struct JsFormatter;

impl Formatter for JsFormatter {
    fn write_f32<W>(&mut self, writer: &mut W, value: f32) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        crate::calculation::write_js_json_number(writer, f64::from(value))
    }

    fn write_f64<W>(&mut self, writer: &mut W, value: f64) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        crate::calculation::write_js_json_number(writer, value)
    }
}

/// Writes `value` as `JSON.stringify` does, for hand-written wire writers.
///
/// Integral magnitudes below 2^53 are exact, so their shortest round-trip
/// digits are the integer itself and integer formatting is byte-identical to
/// the general ECMAScript conversion. Larger integers are not taken here
/// because JavaScript prints shortest digits padded with zeros, not the exact
/// binary value. NaN and infinities fail the round-trip test. The test uses a
/// conversion round trip rather than `f64::fract`, which is a libm call on the
/// baseline x86-64 target.
pub(crate) fn write_js_number(output: &mut Vec<u8>, value: f64) -> io::Result<()> {
    const EXACT_INTEGER_LIMIT: f64 = 9_007_199_254_740_992.0;
    let integer = value as i64;
    if integer as f64 == value && value.abs() < EXACT_INTEGER_LIMIT {
        write_i64(output, integer);
        Ok(())
    } else {
        crate::calculation::write_js_json_number(output, value)
    }
}

/// Writes `value` in decimal, as serde_json's `CompactFormatter` does.
///
/// Most API integers are counts, flags and enum numbers below ten, which are
/// pushed as one digit instead of going through the general formatter and
/// `io::Write` adapter.
#[inline]
pub(crate) fn write_i64(output: &mut Vec<u8>, value: i64) {
    if (0..10).contains(&value) {
        output.push(b'0' + value as u8);
    } else {
        CompactFormatter
            .write_i64(output, value)
            .expect("writing to a Vec cannot fail");
    }
}

/// Writes a JSON string with serde_json's exact escape spelling.
pub(crate) fn write_str(output: &mut Vec<u8>, value: &str) -> io::Result<()> {
    output.reserve(value.len() + 2);
    output.push(b'"');
    write_str_content(output, value);
    output.push(b'"');
    Ok(())
}

/// Writes the escaped content of a JSON string, without quotes, exactly as
/// serde_json's `format_escaped_str` spells it: the two-character escapes
/// for `"`, `\`, backspace, form feed, newline, carriage return and tab;
/// lowercase `\u00xx` for other control characters; everything else verbatim.
/// Escaping is per character, so concatenated parts escape like their join.
pub(crate) fn write_str_content(output: &mut Vec<u8>, value: &str) {
    let bytes = value.as_bytes();
    if bytes
        .iter()
        .all(|&byte| byte >= 0x20 && byte != b'"' && byte != b'\\')
    {
        output.extend_from_slice(bytes);
        return;
    }
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        let escape: &[u8] = match byte {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            0x08 => b"\\b",
            0x0c => b"\\f",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x00..=0x1f => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                output.extend_from_slice(&bytes[start..index]);
                output.extend_from_slice(&[
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[usize::from(byte >> 4)],
                    HEX[usize::from(byte & 0xf)],
                ]);
                start = index + 1;
                continue;
            }
            _ => continue,
        };
        output.extend_from_slice(&bytes[start..index]);
        output.extend_from_slice(escape);
        start = index + 1;
    }
    output.extend_from_slice(&bytes[start..]);
}

/// A value written directly as wire JSON by the hand-written encoders.
///
/// Implementations must produce exactly what the corresponding serde DTO
/// field produces through [`JsFormatter`]; the encoders are verified against
/// the DTO path byte for byte.
pub(crate) trait Wire {
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()>;
}

impl Wire for f64 {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        write_js_number(output, *self)
    }
}

impl Wire for i64 {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        write_i64(output, *self);
        Ok(())
    }
}

impl Wire for bool {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        output.extend_from_slice(if *self { b"true" } else { b"false" });
        Ok(())
    }
}

impl Wire for str {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        write_str(output, self)
    }
}

impl Wire for String {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        write_str(output, self)
    }
}

impl Wire for std::sync::Arc<str> {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        write_str(output, self)
    }
}

impl Wire for std::borrow::Cow<'_, str> {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        write_str(output, self)
    }
}

impl<T: Wire + ?Sized> Wire for &T {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        (**self).write_wire(output)
    }
}

impl<T: Wire> Wire for Option<T> {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        match self {
            Some(value) => value.write_wire(output),
            None => {
                output.extend_from_slice(b"null");
                Ok(())
            }
        }
    }
}

impl<T: Wire> Wire for [T] {
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        output.push(b'[');
        for (index, value) in self.iter().enumerate() {
            if index > 0 {
                output.push(b',');
            }
            value.write_wire(output)?;
        }
        output.push(b']');
        Ok(())
    }
}

impl<T: Wire> Wire for Vec<T> {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        self.as_slice().write_wire(output)
    }
}

/// Writes a value through serde and [`JsFormatter`], for rare or dynamic
/// subtrees whose DTO serialization is the definition of their wire form.
pub(crate) struct Serde<'a, T: ?Sized>(pub(crate) &'a T);

impl<T: Serialize + ?Sized> Wire for Serde<'_, T> {
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        let mut serializer = Serializer::with_formatter(output, JsFormatter);
        self.0.serialize(&mut serializer).map_err(io::Error::other)
    }
}

/// Writes a value with a closure, for nested structures built in place.
pub(crate) struct With<F>(pub(crate) F);

impl<F: Fn(&mut Vec<u8>) -> io::Result<()>> Wire for With<F> {
    #[inline]
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        (self.0)(output)
    }
}

/// Writes a JSON object in serde field order to a `Vec<u8>` named by `$out`.
///
/// `"key": value` writes a [`Wire`] value; `"key": { ... }` writes a nested
/// object; `"key"?: option` omits the member when `None`, like
/// `skip_serializing_if = "Option::is_none"`. Separators are literals
/// combined with the key, and the first-member flag is a local constant the
/// optimizer folds wherever no optional member precedes. The enclosing
/// function must return `io::Result`.
///
/// Expansion recurses once per member, so objects longer than the recursion
/// limit allows are written in groups: `wire_object!(@open first)`, then
/// `wire_object!(@members out first { ... })` per group, then
/// `wire_object!(@close out first)`.
macro_rules! wire_object {
    ($out:ident { $($body:tt)* }) => {{
        $crate::api::json::wire_object!(@open first);
        $crate::api::json::wire_object!(@field $out first; $($body)*);
        $crate::api::json::wire_object!(@close $out first);
    }};
    (@open $first:ident) => {
        #[allow(unused_assignments, unused_mut)]
        let mut $first = true;
    };
    (@members $out:ident $first:ident { $($body:tt)* }) => {
        $crate::api::json::wire_object!(@field $out $first; $($body)*);
    };
    (@close $out:ident $first:ident) => {
        if $first {
            $out.push(b'{');
        }
        $out.push(b'}');
    };
    (@key $out:ident $first:ident $key:literal) => {
        $out.extend_from_slice(if $first {
            concat!("{\"", $key, "\":").as_bytes()
        } else {
            concat!(",\"", $key, "\":").as_bytes()
        });
        $first = false;
    };
    (@field $out:ident $first:ident;) => {};
    (@field $out:ident $first:ident; $key:literal : { $($inner:tt)* } $(, $($rest:tt)*)?) => {
        $crate::api::json::wire_object!(@key $out $first $key);
        $crate::api::json::wire_object!($out { $($inner)* });
        $crate::api::json::wire_object!(@field $out $first; $($($rest)*)?);
    };
    (@field $out:ident $first:ident; $key:literal ?: $value:expr $(, $($rest:tt)*)?) => {
        if let Some(value) = $value {
            $crate::api::json::wire_object!(@key $out $first $key);
            $crate::api::json::Wire::write_wire(&value, $out)?;
        }
        $crate::api::json::wire_object!(@field $out $first; $($($rest)*)?);
    };
    (@field $out:ident $first:ident; $key:literal : $value:expr $(, $($rest:tt)*)?) => {
        $crate::api::json::wire_object!(@key $out $first $key);
        $crate::api::json::Wire::write_wire(&$value, $out)?;
        $crate::api::json::wire_object!(@field $out $first; $($($rest)*)?);
    };
}
pub(crate) use wire_object;
impl Wire for crate::model::memory::Catcher {
    fn write_wire(&self, output: &mut Vec<u8>) -> io::Result<()> {
        wire_object!(output {
            "x": f64::from(self.x),
            "dashing": self.dashing,
        });
        Ok(())
    }
}

pub(crate) fn encode<T: Serialize>(value: &T, capacity: usize) -> Result<Vec<u8>, ApiError> {
    let mut output = Vec::with_capacity(capacity);
    let mut serializer = Serializer::with_formatter(&mut output, JsFormatter);
    value
        .serialize(&mut serializer)
        .map_err(|error| ApiError::serialization(error.to_string()))?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{write_i64, write_js_number, write_str};

    #[test]
    fn integer_spelling_matches_serde_json() {
        for value in [
            i64::MIN,
            i64::MIN + 1,
            -10,
            -9,
            -1,
            0,
            1,
            9,
            10,
            11,
            99,
            100,
            i64::MAX - 1,
            i64::MAX,
        ] {
            let mut direct = Vec::new();
            write_i64(&mut direct, value);
            assert_eq!(
                String::from_utf8(direct).unwrap(),
                serde_json::to_string(&value).unwrap()
            );
        }
    }

    #[test]
    fn string_escapes_match_serde_json() {
        let mut values: Vec<String> = (0_u32..0x80)
            .chain([0xe9, 0x2028, 0x2029, 0xfeff, 0x1f3b5])
            .map(|code| char::from_u32(code).unwrap().to_string())
            .collect();
        values.push(String::new());
        values.push("plain ascii".into());
        values.push("a\"b\\c/d\u{0}e\u{1f}f\u{7f}\n\r\t\u{8}\u{c}é🎵".into());
        values.push((0_u8..0x80).map(char::from).collect());
        for value in values {
            let mut direct = Vec::new();
            write_str(&mut direct, &value).unwrap();
            assert_eq!(
                String::from_utf8(direct).unwrap(),
                serde_json::to_string(&value).unwrap(),
                "{value:?}"
            );
        }
    }

    fn spelled(value: f64) -> (String, String) {
        let mut fast = Vec::new();
        write_js_number(&mut fast, value).unwrap();
        let mut general = Vec::new();
        crate::calculation::write_js_json_number(&mut general, value).unwrap();
        (
            String::from_utf8(fast).unwrap(),
            String::from_utf8(general).unwrap(),
        )
    }

    #[test]
    fn integral_fast_path_matches_ecmascript_spelling() {
        let limit = 9_007_199_254_740_992.0_f64;
        let mut values = vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            1000.0,
            1e15,
            1e16,
            123_456_789_012_345.0,
            limit - 1.0,
            -(limit - 1.0),
            limit,
            -limit,
            limit + 2.0,
            1e21,
            1e22,
            f64::MAX,
            f64::MIN,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.5,
            -2.5,
            1e-7,
            4_503_599_627_370_495.5,
        ];
        values.extend((0..64).map(|shift| 2_f64.powi(shift)));
        values.extend((0..64).map(|shift| -(2_f64.powi(shift) - 1.0)));
        values.extend((1..=22).map(|exponent| 10_f64.powi(exponent)));
        for value in values {
            let (fast, general) = spelled(value);
            assert_eq!(fast, general, "{value:e}");
        }
        assert_eq!(spelled(-0.0).0, "0");
        assert_eq!(spelled(f64::NAN).0, "null");
        assert_eq!(spelled(1e21).0, "1e+21");
        assert_eq!(spelled(2_f64.powi(60)).0, "1152921504606847000");
    }
}
