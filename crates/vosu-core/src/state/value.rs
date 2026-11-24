//! `StateValue`: the in-memory value tree a committed namespace document is
//! built from (S1, S2; plan:567-569). `from_cbor` decodes a publish's CBOR
//! payload under the same bounds `docs/v2/plugin-abi.md` places on it (MF)
//! ([`super::cbor`]): depth at most 32, map keys must be text, no
//! duplicates, no tags, no trailing bytes. `write_json` follows D13's JSON
//! projection rule: a non-finite `f64` writes as `null`
//! ([`super::ecma_number`], JS `JSON.stringify`), and `BYTES` writes as
//! standard base64 (no schema in this freeze has a bytes field).

use std::io::Write as _;
use std::sync::Arc;

use vosu_plugin_api::CborValue;

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
    #[error("duplicate CBOR map key {0:?} (RFC 8949 S5.6)")]
    DuplicateKey(String),
    #[error("bytes remain after the root CBOR value (MF)")]
    TrailingBytes,
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
    /// text-only map keys, no tags, no duplicate map keys, no trailing
    /// bytes). A CBOR integer that fits `i64` canonicalizes to [`Self::I64`]
    /// even if the publisher encoded it as unsigned; only a value past
    /// `i64::MAX` decodes as [`Self::U64`] — CBOR's wire integer carries no
    /// signedness of its own. Decoded header-by-header
    /// ([`super::cbor::decode`]), not through `ciborium::de::from_reader`'s
    /// `CborValue` tree, so a bignum tag (2/3) over a short byte string
    /// cannot silently fold into a plain integer past the "no tags" rule.
    pub fn from_cbor(bytes: &[u8]) -> Result<Self, ValueError> {
        super::cbor::decode(bytes)
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
            Self::F64(f) => super::ecma_number::write_js_number(out, *f),
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

    /// Descends through nested objects by key and arrays by index (1.x
    /// `parse_array_index`: no leading zero, out-of-range is absent); the
    /// empty token slice selects the whole value (the root pointer).
    pub fn get(&self, tokens: &[impl AsRef<str>]) -> Option<&StateValue> {
        let mut current = self;
        for token in tokens {
            current = match current {
                Self::Object(obj) => obj.get(token.as_ref())?,
                Self::Array(items) => items.get(parse_array_index(token.as_ref())?)?,
                _ => return None,
            };
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

/// 1.x's array-index pointer token rule (`src/server/v2_subscription.rs:
/// 375-386`): `"0"` is the only index allowed a leading zero; any other
/// leading zero, an empty token, or a non-digit byte is not an index at
/// all (not just out of range), so it reports absent exactly as a genuine
/// out-of-range index does.
fn parse_array_index(token: &str) -> Option<usize> {
    if token == "0" {
        return Some(0);
    }
    if token.starts_with('0') || token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    token.parse().ok()
}

fn write_json_string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
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
    fn get_does_not_descend_into_scalars() {
        let v = obj(vec![("score", StateValue::I64(5))]);
        assert_eq!(v.get(&["score", "x"]), None);
    }

    #[test]
    fn get_indexes_arrays_with_1x_rules() {
        let arr = StateValue::Array(Arc::from(vec![
            StateValue::I64(10),
            StateValue::I64(11),
            StateValue::I64(12),
        ]));
        assert_eq!(arr.get(&["0"]), Some(&StateValue::I64(10)));
        assert_eq!(arr.get(&["2"]), Some(&StateValue::I64(12)));
        // Out of range: absent, not an error.
        assert_eq!(arr.get(&["3"]), None);
        // A leading zero is refused for any index but "0" itself.
        assert_eq!(arr.get(&["00"]), None);
        assert_eq!(arr.get(&["01"]), None);
        // Non-digit and empty tokens are refused too.
        assert_eq!(arr.get(&["1a"]), None);
        assert_eq!(arr.get(&[""]), None);
        // Nested: an array inside an object, and vice versa.
        let nested = obj(vec![("items", arr)]);
        assert_eq!(nested.get(&["items", "1"]), Some(&StateValue::I64(11)));
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
        StateValue::String(Arc::from("a\"b\\c\n\u{8}\u{c}")).write_json(&mut out);
        assert_eq!(out, b"\"a\\\"b\\\\c\\n\\b\\f\"");
    }
}
