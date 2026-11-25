use vosu_plugin_api::{CborValue, StateEntry};

/// CBOR preserves signed zero and non-finite source numbers. Rust's derived
/// float equality would miss -0 changes and invalidate unchanged NaNs forever.
pub(super) fn same_entries(
    left: &[StateEntry],
    right: &[StateEntry],
    observes: impl Fn(&StateEntry, &str) -> bool,
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.ns == right.ns
                && left.pointer == right.pointer
                && left.error == right.error
                && match (&left.value, &right.value) {
                    (Some(lv), Some(rv)) => same_value(lv, rv, &|field| observes(left, field)),
                    (None, None) => true,
                    _ => false,
                }
        })
}
fn same_value(left: &CborValue, right: &CborValue, observes: &impl Fn(&str) -> bool) -> bool {
    match (left, right) {
        (CborValue::Float(left), CborValue::Float(right)) => left.to_bits() == right.to_bits(),
        (CborValue::Array(left), CborValue::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| same_value(left, right, observes))
        }
        (CborValue::Map(left), CborValue::Map(right)) => {
            let relevant = |(key, _): &&(CborValue, CborValue)| key.as_text().is_none_or(observes);
            let mut left = left.iter().filter(relevant);
            let mut right = right.iter().filter(relevant);
            loop {
                match (left.next(), right.next()) {
                    (None, None) => break true,
                    (Some((lk, lv)), Some((rk, rv)))
                        if same_value(lk, rk, observes) && same_value(lv, rv, observes) => {}
                    _ => break false,
                }
            }
        }
        (CborValue::Tag(lt, left), CborValue::Tag(rt, right)) => {
            lt == rt && same_value(left, right, observes)
        }
        _ => left == right,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn entry(value: f64) -> Vec<StateEntry> {
        vec![StateEntry {
            ns: "test".into(),
            pointer: "/value".into(),
            value: Some(CborValue::Array(vec![CborValue::Float(value)])),
            error: None,
        }]
    }
    #[test]
    fn selected_float_identity_preserves_nan_payload_and_signed_zero() {
        let nan = f64::from_bits(0x7ff8_0000_0000_0001);
        assert!(same_entries(&entry(nan), &entry(nan), |_, _| true));
        assert!(!same_entries(
            &entry(nan),
            &entry(f64::from_bits(0x7ff8_0000_0000_0002)),
            |_, _| true
        ));
        assert!(!same_entries(&entry(0.0), &entry(-0.0), |_, _| true));
    }
}
