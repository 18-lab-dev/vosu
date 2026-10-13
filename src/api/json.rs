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
/// binary value. NaN and infinities fail the `fract` test.
pub(crate) fn write_js_number<W>(writer: &mut W, value: f64) -> io::Result<()>
where
    W: ?Sized + Write,
{
    const EXACT_INTEGER_LIMIT: f64 = 9_007_199_254_740_992.0;
    if value.fract() == 0.0 && value.abs() < EXACT_INTEGER_LIMIT {
        CompactFormatter.write_i64(writer, value as i64)
    } else {
        crate::calculation::write_js_json_number(writer, value)
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
    use super::write_js_number;

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
