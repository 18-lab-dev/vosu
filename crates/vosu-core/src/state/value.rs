//! `StateValue`: the in-memory value tree a committed namespace document is
//! built from (S1, S2; plan:567-569). `from_cbor` decodes a publish's CBOR
//! payload under the same bounds `docs/v2/plugin-abi.md` places on it (MF):
//! depth at most 32, map keys must be text, and tags are refused.
//! `write_json` follows D13's JSON projection rule: a non-finite `f64`
//! writes as `null` (JS `JSON.stringify`), and `BYTES` writes as standard
//! base64 (no schema in this freeze has a bytes field).

use std::io::Write as _;
use std::sync::Arc;

use vosu_plugin_api::CborValue;

/// At most 32 levels of CBOR nesting (MF), matching the schema walker's own
/// D7 bound.
const MAX_DEPTH: usize = 32;

/// One value in a session's namespace documents: a persistent,
/// `Arc`-shared tree so a commit only clones the path it writes (MI).
#[derive(Clone, Debug, PartialEq)]
pub enum StateValue {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    String(Arc<str>),
    Bytes(Arc<[u8]>),
    Array(Arc<[StateValue]>),
    Object(Arc<StateObject>),
}

/// An object's fields, in publish/insertion order (not resorted): the order
/// a 1.x consumer and the schema's `properties` both expect.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateObject(Vec<(Arc<str>, StateValue)>);

/// Every way decoding a publish's CBOR bytes into a [`StateValue`] can fail
/// (MF).
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ValueError {
    #[error("invalid CBOR: {0}")]
    Decode(String),
    #[error("CBOR nests more than 32 levels deep (MF)")]
    TooDeep,
    #[error("CBOR map key is not text (MF)")]
    NonTextKey,
    #[error("CBOR tags are not allowed (MF)")]
    Tagged,
    #[error("CBOR integer {0} does not fit i64 or u64")]
    IntegerOutOfRange(String),
    #[error("unsupported CBOR value kind")]
    Unsupported,
}

impl StateObject {
    pub fn new(entries: Vec<(Arc<str>, StateValue)>) -> Self {
        Self(entries)
    }

    pub(super) fn entries_mut(&mut self) -> &mut Vec<(Arc<str>, StateValue)> {
        &mut self.0
    }

    pub fn get(&self, key: &str) -> Option<&StateValue> {
        self.0
            .iter()
            .find(|(k, _)| k.as_ref() == key)
            .map(|(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &StateValue)> {
        self.0.iter().map(|(k, v)| (k.as_ref(), v))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl StateValue {
    /// Decodes a publish's CBOR bytes, enforcing MF's bounds (depth ≤ 32,
    /// text-only map keys, no tags). A CBOR integer that fits `i64`
    /// canonicalizes to [`Self::I64`] even if the publisher encoded it as
    /// unsigned; only a value past `i64::MAX` decodes as [`Self::U64`] —
    /// CBOR's wire integer carries no signedness of its own.
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, ValueError> {
        let decoded: CborValue =
            ciborium::de::from_reader(bytes).map_err(|e| ValueError::Decode(e.to_string()))?;
        from_cbor_value(decoded, 1)
    }

    /// Converts to the dynamic CBOR value `STATE_UPDATE`/`vosu-surface`
    /// entries carry.
    pub fn to_cbor_value(&self) -> CborValue {
        match self {
            Self::Null => CborValue::Null,
            Self::Bool(b) => CborValue::Bool(*b),
            Self::I64(v) => CborValue::Integer((*v).into()),
            Self::U64(v) => CborValue::Integer((*v).into()),
            Self::F64(f) => CborValue::Float(*f),
            Self::String(s) => CborValue::Text(s.to_string()),
            Self::Bytes(b) => CborValue::Bytes(b.to_vec()),
            Self::Array(items) => CborValue::Array(items.iter().map(Self::to_cbor_value).collect()),
            Self::Object(obj) => CborValue::Map(
                obj.0
                    .iter()
                    .map(|(k, v)| (CborValue::Text(k.to_string()), v.to_cbor_value()))
                    .collect(),
            ),
        }
    }

    /// Writes this value as CBOR bytes.
    pub fn write_cbor(&self, out: &mut Vec<u8>) {
        ciborium::ser::into_writer(&self.to_cbor_value(), out)
            .expect("serializing a StateValue into a Vec<u8> cannot fail");
    }

    /// Writes this value as JSON bytes: a non-finite `f64` writes `null`
    /// (D13; JS `JSON.stringify`), and `Bytes` writes as standard base64.
    pub fn write_json(&self, out: &mut Vec<u8>) {
        match self {
            Self::Null => out.extend_from_slice(b"null"),
            Self::Bool(true) => out.extend_from_slice(b"true"),
            Self::Bool(false) => out.extend_from_slice(b"false"),
            Self::I64(v) => {
                let _ = write!(out, "{v}");
            }
            Self::U64(v) => {
                let _ = write!(out, "{v}");
            }
            Self::F64(f) if f.is_finite() => {
                let _ = write!(out, "{f}");
            }
            Self::F64(_) => out.extend_from_slice(b"null"),
            Self::String(s) => write_json_string(out, s),
            Self::Bytes(b) => write_json_string(out, &base64_standard(b)),
            Self::Array(items) => {
                out.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    item.write_json(out);
                }
                out.push(b']');
            }
            Self::Object(obj) => {
                out.push(b'{');
                for (i, (k, v)) in obj.0.iter().enumerate() {
                    if i > 0 {
                        out.push(b',');
                    }
                    write_json_string(out, k);
                    out.push(b':');
                    v.write_json(out);
                }
                out.push(b'}');
            }
        }
    }

    /// Descends through nested objects by key; the empty token slice
    /// selects the whole value (the root pointer). Arrays are leaves (ML):
    /// no token ever descends into one.
    pub fn get(&self, tokens: &[impl AsRef<str>]) -> Option<&StateValue> {
        let mut current = self;
        for token in tokens {
            let Self::Object(obj) = current else {
                return None;
            };
            current = obj.get(token.as_ref())?;
        }
        Some(current)
    }
}

/// Writes a `{"ns": <value>, ...}` document: the shape both
/// [`super::document::SessionState::api_json`]'s cache and
/// [`super::projector::Redacted`]'s debug rendering share.
pub(super) fn write_document_json(doc: &[(Arc<str>, StateValue)], out: &mut Vec<u8>) {
    out.push(b'{');
    for (i, (ns, v)) in doc.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        write_json_string(out, ns);
        out.push(b':');
        v.write_json(out);
    }
    out.push(b'}');
}

fn write_json_string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard (padded) base64, RFC 4648 §4: `Bytes`' JSON projection (D13).
fn base64_standard(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(BASE64_ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(BASE64_ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}
fn from_cbor_value(value: CborValue, depth: usize) -> Result<StateValue, ValueError> {
    if depth > MAX_DEPTH {
        return Err(ValueError::TooDeep);
    }
    Ok(match value {
        CborValue::Null => StateValue::Null,
        CborValue::Bool(b) => StateValue::Bool(b),
        CborValue::Integer(i) => {
            if let Ok(v) = i64::try_from(i) {
                StateValue::I64(v)
            } else if let Ok(v) = u64::try_from(i) {
                StateValue::U64(v)
            } else {
                return Err(ValueError::IntegerOutOfRange(format!("{i:?}")));
            }
        }
        CborValue::Float(f) => StateValue::F64(f),
        CborValue::Text(s) => StateValue::String(Arc::from(s)),
        CborValue::Bytes(b) => StateValue::Bytes(Arc::from(b)),
        CborValue::Tag(_, _) => return Err(ValueError::Tagged),
        CborValue::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(from_cbor_value(item, depth + 1)?);
            }
            StateValue::Array(Arc::from(out))
        }
        CborValue::Map(entries) => {
            let mut out = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let CborValue::Text(key) = k else {
                    return Err(ValueError::NonTextKey);
                };
                out.push((Arc::from(key), from_cbor_value(v, depth + 1)?));
            }
            StateValue::Object(Arc::new(StateObject(out)))
        }
        _ => return Err(ValueError::Unsupported),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(entries: Vec<(&str, StateValue)>) -> StateValue {
        StateValue::Object(Arc::new(StateObject::new(
            entries
                .into_iter()
                .map(|(k, v)| (Arc::from(k), v))
                .collect(),
        )))
    }

    #[test]
    fn get_descends_nested_objects_and_root_is_whole_value() {
        let v = obj(vec![(
            "internal",
            obj(vec![("paths", obj(vec![("stable", StateValue::Null)]))]),
        )]);
        let empty: &[&str] = &[];
        assert_eq!(v.get(empty), Some(&v));
        assert_eq!(
            v.get(&["internal", "paths", "stable"]),
            Some(&StateValue::Null)
        );
        assert_eq!(v.get(&["internal", "missing"]), None);
    }

    #[test]
    fn get_does_not_descend_into_scalars_or_arrays() {
        let v = obj(vec![("score", StateValue::I64(5))]);
        assert_eq!(v.get(&["score", "x"]), None);
        let arr = StateValue::Array(Arc::from(vec![StateValue::I64(1)]));
        assert_eq!(arr.get(&["0"]), None);
    }

    #[test]
    fn json_writes_nan_as_null() {
        let mut out = Vec::new();
        StateValue::F64(f64::NAN).write_json(&mut out);
        assert_eq!(out, b"null");
        out.clear();
        StateValue::F64(f64::INFINITY).write_json(&mut out);
        assert_eq!(out, b"null");
        out.clear();
        StateValue::F64(1.5).write_json(&mut out);
        assert_eq!(out, b"1.5");
    }

    #[test]
    fn json_writes_bytes_as_standard_base64() {
        let mut out = Vec::new();
        StateValue::Bytes(Arc::from(b"foob".as_slice())).write_json(&mut out);
        assert_eq!(out, b"\"Zm9vYg==\"");
    }

    #[test]
    fn json_escapes_strings() {
        let mut out = Vec::new();
        StateValue::String(Arc::from("a\"b\\c\n")).write_json(&mut out);
        assert_eq!(out, b"\"a\\\"b\\\\c\\n\"");
    }

    #[test]
    fn cbor_roundtrips_through_to_cbor_value() {
        let v = obj(vec![
            ("n", StateValue::Null),
            ("b", StateValue::Bool(true)),
            ("i", StateValue::I64(-5)),
            ("u", StateValue::U64(u64::MAX)),
            ("f", StateValue::F64(1.25)),
            ("s", StateValue::String(Arc::from("hi"))),
            ("bytes", StateValue::Bytes(Arc::from(b"xy".as_slice()))),
            (
                "arr",
                StateValue::Array(Arc::from(vec![StateValue::I64(1), StateValue::I64(2)])),
            ),
        ]);
        let mut bytes = Vec::new();
        v.write_cbor(&mut bytes);
        let back = StateValue::from_cbor(&bytes).expect("round-trips");
        assert_eq!(v, back);
    }

    #[test]
    fn from_cbor_rejects_non_text_keys() {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(
            &CborValue::Map(vec![(CborValue::Integer(1.into()), CborValue::Null)]),
            &mut bytes,
        )
        .unwrap();
        assert_eq!(StateValue::from_cbor(&bytes), Err(ValueError::NonTextKey));
    }

    #[test]
    fn from_cbor_rejects_tags() {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&CborValue::Tag(0, Box::new(CborValue::Null)), &mut bytes)
            .unwrap();
        assert_eq!(StateValue::from_cbor(&bytes), Err(ValueError::Tagged));
    }

    #[test]
    fn from_cbor_rejects_too_deep() {
        let mut value = CborValue::Null;
        for _ in 0..=MAX_DEPTH {
            value = CborValue::Array(vec![value]);
        }
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&value, &mut bytes).unwrap();
        assert_eq!(StateValue::from_cbor(&bytes), Err(ValueError::TooDeep));
    }

    fn arb_state_value() -> impl proptest::strategy::Strategy<Value = StateValue> {
        use proptest::prelude::*;
        let leaf = prop_oneof![
            Just(StateValue::Null),
            any::<bool>().prop_map(StateValue::Bool),
            any::<i64>().prop_map(StateValue::I64),
            ((i64::MAX as u64 + 1)..=u64::MAX).prop_map(StateValue::U64),
            (-1.0e10f64..1.0e10f64).prop_map(StateValue::F64),
            "[ -~]{0,8}".prop_map(|s: String| StateValue::String(Arc::from(s))),
            proptest::collection::vec(any::<u8>(), 0..8)
                .prop_map(|b| StateValue::Bytes(Arc::from(b))),
        ];
        leaf.prop_recursive(3, 16, 4, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..4)
                    .prop_map(|v| StateValue::Array(Arc::from(v))),
                proptest::collection::vec(("[a-z]{1,6}", inner), 0..4).prop_map(|entries| {
                    let fields = entries
                        .into_iter()
                        .map(|(k, v)| (Arc::from(k), v))
                        .collect();
                    StateValue::Object(Arc::new(StateObject::new(fields)))
                }),
            ]
        })
    }

    proptest::proptest! {
        #[test]
        fn prop_cbor_value_roundtrip(v in arb_state_value()) {
            let mut bytes = Vec::new();
            v.write_cbor(&mut bytes);
            let back = StateValue::from_cbor(&bytes).expect("round-trips");
            proptest::prop_assert_eq!(v, back);
        }
    }
}
