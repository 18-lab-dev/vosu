//! Allocation-free `Number(value.toFixed(n))` for the API encoders.
//!
//! [`crate::calculation::fix_decimals_to`] defines the semantics: it rounds the
//! exact binary value half-up to `amount` decimals, then parses the decimal
//! text. The API encoders call it dozens of times per cold document, and the
//! text round trip (format, insert, parse) dominated their cost. When the
//! rounded integer and `10^amount` are both exactly representable, IEEE
//! division is correctly rounded just like the parse, so the quotient is the
//! same `f64`; everything else still goes through the general conversion.

use crate::calculation::DecimalPlacesError;

/// `Number(value.toFixed(2))`; see [`fix_decimals_to`].
pub(crate) fn fix_decimals(value: f64) -> f64 {
    fix_decimals_to(value, 2).expect("two decimals are valid")
}

/// Same result as [`crate::calculation::fix_decimals_to`] for every input.
pub(crate) fn fix_decimals_to(value: f64, amount: u32) -> Result<f64, DecimalPlacesError> {
    const EXACT_INTEGER_LIMIT: u128 = 1 << 53;
    if value == 0.0 || value.is_nan() {
        return if amount > 100 {
            Err(DecimalPlacesError)
        } else {
            Ok(0.0)
        };
    }
    if amount > 100 {
        return Err(DecimalPlacesError);
    }
    // An exact integer prints as its digits plus zero decimals and parses
    // back to itself.
    if (value as i64) as f64 == value && value.abs() < EXACT_INTEGER_LIMIT as f64 {
        return Ok(value);
    }
    if amount <= 22 && value.abs() < 1e21 {
        let bits = value.abs().to_bits();
        let raw_exponent = ((bits >> 52) & 0x7ff) as i32;
        let fraction = bits & ((1_u64 << 52) - 1);
        let (significand, exponent) = if raw_exponent == 0 {
            (fraction, -1074)
        } else {
            ((1_u64 << 52) | fraction, raw_exponent - 1023 - 52)
        };
        // significand < 2^53 and 5^22 < 2^52, so the product cannot overflow.
        let numerator = u128::from(significand) * 5_u128.pow(amount);
        let shift = exponent + amount as i32;
        let integer = if shift >= 0 {
            // Exact integers at or above 2^53 take the general path below.
            (shift < 53).then(|| numerator << shift)
        } else {
            let right = (-shift) as u32;
            Some(if right >= 128 {
                0
            } else {
                let quotient = numerator >> right;
                let remainder = numerator & ((1_u128 << right) - 1);
                quotient + u128::from(remainder >= (1_u128 << (right - 1)))
            })
        };
        if let Some(integer) = integer.filter(|integer| *integer <= EXACT_INTEGER_LIMIT) {
            let magnitude = integer as f64 / POWERS_OF_TEN[amount as usize];
            return Ok(if value.is_sign_negative() {
                -magnitude
            } else {
                magnitude
            });
        }
    }
    crate::calculation::fix_decimals_to(value, amount)
}

/// Every power of ten that is exactly representable as an `f64`.
const POWERS_OF_TEN: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

#[cfg(test)]
mod tests {
    use super::fix_decimals_to;

    fn assert_same(value: f64, amount: u32) {
        let fast = fix_decimals_to(value, amount);
        let general = crate::calculation::fix_decimals_to(value, amount);
        assert_eq!(
            fast.map(f64::to_bits),
            general.map(f64::to_bits),
            "{value:e} to {amount}: {fast:?} vs {general:?}"
        );
    }

    #[test]
    fn matches_general_to_fixed_bit_for_bit() {
        let mut values = vec![
            0.0,
            -0.0,
            f64::NAN,
            -f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            5e-324,
            -5e-324,
            0.005,
            -0.005,
            0.015,
            1.005,
            2.675,
            1.115,
            -1.125,
            0.125,
            0.00499999999,
            123.456,
            99.995,
            9_007_199_254_740_991.0,
            9_007_199_254_740_993.0,
            90_071_992_547_409.9,
            900_719_925_474_099.5,
            1e15 + 0.3,
            1e20,
            999_999_999_999_999_999_999.0,
            1e21,
            -1e21,
            f64::MAX,
        ];
        // Deterministic xorshift values across magnitudes, plus halfway cases.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..20_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let unit = (state >> 11) as f64 / (1_u64 << 53) as f64;
            let scale = 10_f64.powi((state % 40) as i32 - 12);
            values.push((unit - 0.5) * scale);
            values.push(((state % 2_000_000) as f64 + 0.5) / 100.0);
            values.push(f64::from_bits(state));
        }
        for value in values {
            for amount in [0, 1, 2, 3, 4, 10, 22, 23, 100, 101] {
                assert_same(value, amount);
            }
        }
    }
}
