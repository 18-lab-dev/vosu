//! CBOR schema numbers can use integer or floating tags; widen only when requested.
use serde::{
    Deserialize, Deserializer,
    de::{self, Visitor},
};
use std::fmt;
struct Number(f64);
impl<'de> Deserialize<'de> for Number {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct NumberVisitor;
        impl Visitor<'_> for NumberVisitor {
            type Value = Number;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a number")
            }
            fn visit_f64<E: de::Error>(self, n: f64) -> Result<Number, E> {
                Ok(Number(n))
            }
            fn visit_i64<E: de::Error>(self, n: i64) -> Result<Number, E> {
                Ok(Number(n as f64))
            }
            fn visit_u64<E: de::Error>(self, n: u64) -> Result<Number, E> {
                Ok(Number(n as f64))
            }
        }
        d.deserialize_any(NumberVisitor)
    }
}
pub(crate) fn float<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    Number::deserialize(d).map(|n| n.0)
}
pub(crate) fn optional<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Option::<Number>::deserialize(d).map(|n| n.map(|n| n.0))
}
