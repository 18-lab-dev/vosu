//! The allocating pre-D17 implementation is retained only as a parity oracle.
use super::*;
use proptest::prelude::*;

fn original_fix_decimals_to(value: f64, amount: u32) -> Result<f64, DecimalPlacesError> {
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
    if let Some(rounded) = original_exact_fixed_round(value, amount) {
        return Ok(rounded);
    }
    Ok(format!("{value:.amount$}", amount = amount as usize)
        .parse()
        .expect("fixed-point output is numeric"))
}
fn original_exact_fixed_round(value: f64, amount: u32) -> Option<f64> {
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
proptest! {
    #![proptest_config(vosu_proptest::vosu_proptest_config())]
    #[test]
    fn prop_stack_rounding_is_bit_identical_to_allocating_oracle(bits in any::<u64>(), amount in 0_u32..=100) {
        let value = f64::from_bits(bits);
        prop_assert_eq!(fix_decimals_to(value, amount).unwrap().to_bits(), original_fix_decimals_to(value, amount).unwrap().to_bits());
    }
}
#[test]
fn rounding_edges_and_every_api_precision_match_original_bits() {
    // API uses 2 (default fixed values) and 4 (BPM); also exercise every
    // accepted amount so both the integer and formatting paths are covered.
    let edges = [
        0.0,
        -0.0,
        0.5,
        -0.5,
        1.005,
        -1.005,
        2.675,
        -2.675,
        1.125,
        -1.125,
        0.00005,
        -0.00005,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        -f64::from_bits(1),
        1e-100,
        -1e-100,
        1e20,
        -1e20,
        1e21,
        -1e21,
        f64::MAX,
        f64::MIN,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];
    for value in edges {
        for amount in 0..=100 {
            assert_eq!(
                fix_decimals_to(value, amount).unwrap().to_bits(),
                original_fix_decimals_to(value, amount).unwrap().to_bits(),
                "value={value:?}, amount={amount}"
            );
        }
    }
    assert!(fix_decimals_to(1.0, 101).is_err());
    assert_eq!(fix_decimals(-0.0).to_bits(), 0.0_f64.to_bits());
}
