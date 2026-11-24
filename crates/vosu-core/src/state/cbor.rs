//! Decodes a publish's CBOR bytes into a [`StateValue`] directly from
//! `ciborium_ll` headers (MF; abi:219-220), rather than through
//! `ciborium::de::from_reader::<CborValue, _>` and a second pass over the
//! resulting tree.
//!
//! That matters for one specific bound: `ciborium::de::from_reader` folds a
//! bignum tag (2 or 3) over a byte string of at most 16 bytes straight into
//! an integer (`ciborium-0.2.2/src/de/mod.rs:166-176`), so `CborValue::Tag`
//! never appears for that case, and MF's "no CBOR tags" rule would miss it.
//! Reading headers ourselves sees — and refuses — every `Header::Tag`,
//! bignum or otherwise, before it can be folded away. The same single pass
//! also enforces the depth bound without ever building a whole tree, and
//! catches a duplicate map key and trailing bytes after the root value,
//! neither of which `ciborium::Value` decoding does.

use std::sync::Arc;

use ciborium_ll::{Decoder, Header};

use super::value::{StateObject, StateValue, ValueError};

/// At most 32 levels of CBOR container nesting (MF), matching the schema
/// walker's `json_depth` (`schema/walker.rs`): a container one level deeper
/// than its deepest child container, a scalar contributing no level of its
/// own. 32 nested containers holding a scalar is the boundary case that
/// must still decode.
const MAX_DEPTH: usize = 32;

/// Read-chunk buffer size for segmented (indefinite-length) bytes/text
/// items; definite-length items (every publish ciborium itself encodes)
/// read in one chunk regardless.
const CHUNK: usize = 4096;

pub(super) fn decode(bytes: &[u8]) -> Result<StateValue, ValueError> {
    let mut decoder = Decoder::from(bytes);
    let value = parse_value(&mut decoder, 0)?;
    if decoder.offset() != bytes.len() {
        return Err(ValueError::TrailingBytes);
    }
    Ok(value)
}

fn ll_err<E: std::fmt::Debug>(err: ciborium_ll::Error<E>) -> ValueError {
    ValueError::Decode(format!("{err:?}"))
}

fn parse_value<R: ciborium_io::Read>(
    decoder: &mut Decoder<R>,
    depth: usize,
) -> Result<StateValue, ValueError>
where
    R::Error: std::fmt::Debug,
{
    match decoder.pull().map_err(ll_err)? {
        Header::Positive(n) => Ok(positive_integer(n)),
        Header::Negative(n) => negative_integer(n),
        Header::Float(f) => Ok(StateValue::F64(f)),
        Header::Simple(20) => Ok(StateValue::Bool(false)),
        Header::Simple(21) => Ok(StateValue::Bool(true)),
        Header::Simple(22) => Ok(StateValue::Null),
        Header::Simple(_) => Err(ValueError::Unsupported),
        Header::Tag(_) => Err(ValueError::Tagged),
        Header::Bytes(len) => Ok(StateValue::Bytes(Arc::from(read_bytes(decoder, len)?))),
        Header::Text(len) => Ok(StateValue::String(Arc::from(read_text(decoder, len)?))),
        Header::Array(len) => parse_array(decoder, len, depth),
        Header::Map(len) => parse_map(decoder, len, depth),
        Header::Break => Err(ValueError::Decode(
            "unexpected break outside a container".into(),
        )),
    }
}

fn positive_integer(n: u64) -> StateValue {
    match i64::try_from(n) {
        Ok(v) => StateValue::I64(v),
        Err(_) => StateValue::U64(n),
    }
}

/// `Header::Negative(n)` carries the wire's raw unsigned field; the actual
/// CBOR major-type-1 integer it encodes is `-1 - n` (`ciborium_ll::Header`'s
/// own doc comment).
fn negative_integer(n: u64) -> Result<StateValue, ValueError> {
    let actual = -1_i128 - i128::from(n);
    i64::try_from(actual)
        .map(StateValue::I64)
        .map_err(|_| ValueError::IntegerOutOfRange(actual.to_string()))
}

fn read_bytes<R: ciborium_io::Read>(
    decoder: &mut Decoder<R>,
    len: Option<usize>,
) -> Result<Vec<u8>, ValueError>
where
    R::Error: std::fmt::Debug,
{
    let mut out = Vec::new();
    let mut segments = decoder.bytes(len);
    let mut buf = [0_u8; CHUNK];
    while let Some(mut segment) = segments.pull().map_err(ll_err)? {
        while let Some(chunk) = segment.pull(&mut buf).map_err(ll_err)? {
            out.extend_from_slice(chunk);
        }
    }
    Ok(out)
}

fn read_text<R: ciborium_io::Read>(
    decoder: &mut Decoder<R>,
    len: Option<usize>,
) -> Result<String, ValueError>
where
    R::Error: std::fmt::Debug,
{
    let mut out = String::new();
    let mut segments = decoder.text(len);
    let mut buf = [0_u8; CHUNK];
    while let Some(mut segment) = segments.pull().map_err(ll_err)? {
        while let Some(chunk) = segment
            .pull(&mut buf)
            .map_err(|_| ValueError::Decode("invalid UTF-8 in a CBOR text item".into()))?
        {
            out.push_str(chunk);
        }
    }
    Ok(out)
}

fn parse_array<R: ciborium_io::Read>(
    decoder: &mut Decoder<R>,
    len: Option<usize>,
    depth: usize,
) -> Result<StateValue, ValueError>
where
    R::Error: std::fmt::Debug,
{
    let child_depth = depth + 1;
    if child_depth > MAX_DEPTH {
        return Err(ValueError::TooDeep);
    }
    let mut items = Vec::with_capacity(len.unwrap_or(0));
    match len {
        Some(count) => {
            for _ in 0..count {
                items.push(parse_value(decoder, child_depth)?);
            }
        }
        None => loop {
            let header = decoder.pull().map_err(ll_err)?;
            if matches!(header, Header::Break) {
                break;
            }
            decoder.push(header);
            items.push(parse_value(decoder, child_depth)?);
        },
    }
    Ok(StateValue::Array(Arc::from(items)))
}

fn parse_map<R: ciborium_io::Read>(
    decoder: &mut Decoder<R>,
    len: Option<usize>,
    depth: usize,
) -> Result<StateValue, ValueError>
where
    R::Error: std::fmt::Debug,
{
    let child_depth = depth + 1;
    if child_depth > MAX_DEPTH {
        return Err(ValueError::TooDeep);
    }
    let mut entries: Vec<(Arc<str>, StateValue)> = Vec::with_capacity(len.unwrap_or(0));
    let mut parse_one = |decoder: &mut Decoder<R>| -> Result<(), ValueError> {
        let key = match decoder.pull().map_err(ll_err)? {
            Header::Text(key_len) => read_text(decoder, key_len)?,
            _ => return Err(ValueError::NonTextKey),
        };
        if entries.iter().any(|(k, _)| k.as_ref() == key) {
            return Err(ValueError::DuplicateKey(key));
        }
        let value = parse_value(decoder, child_depth)?;
        entries.push((Arc::from(key), value));
        Ok(())
    };
    match len {
        Some(count) => {
            for _ in 0..count {
                parse_one(decoder)?;
            }
        }
        None => loop {
            let header = decoder.pull().map_err(ll_err)?;
            if matches!(header, Header::Break) {
                break;
            }
            decoder.push(header);
            parse_one(decoder)?;
        },
    }
    Ok(StateValue::Object(Arc::new(StateObject::new(entries))))
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

    fn write(value: &vosu_plugin_api::CborValue) -> Vec<u8> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(value, &mut bytes).unwrap();
        bytes
    }

    #[test]
    fn roundtrips_through_to_cbor_value() {
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
    fn rejects_non_text_keys() {
        let bytes = write(&vosu_plugin_api::CborValue::Map(vec![(
            vosu_plugin_api::CborValue::Integer(1.into()),
            vosu_plugin_api::CborValue::Null,
        )]));
        assert_eq!(StateValue::from_cbor(&bytes), Err(ValueError::NonTextKey));
    }

    #[test]
    fn rejects_ordinary_tags() {
        let bytes = write(&vosu_plugin_api::CborValue::Tag(
            0,
            Box::new(vosu_plugin_api::CborValue::Null),
        ));
        assert_eq!(StateValue::from_cbor(&bytes), Err(ValueError::Tagged));
    }

    /// Tag 2 (positive bignum): `ciborium::de::from_reader` folds a
    /// 16-byte-or-shorter tag-2 byte string straight into a plain integer
    /// (`ciborium-0.2.2/src/de/mod.rs:166-176`), never surfacing
    /// `CborValue::Tag`. Header-level decoding must still refuse it.
    #[test]
    fn rejects_bignum_tag_2() {
        // 0xC2 (tag 2) 0x41 (bytes, len 1) 0x05 -> ciborium::Value decodes
        // this as plain Integer(5); from_cbor must see the tag and refuse.
        let bytes = vec![0xC2, 0x41, 0x05];
        let decoded_as_plain_value: vosu_plugin_api::CborValue =
            ciborium::de::from_reader(bytes.as_slice()).expect("ciborium::Value decodes it");
        assert_eq!(
            decoded_as_plain_value,
            vosu_plugin_api::CborValue::Integer(5.into()),
            "confirms the bypass this test guards against"
        );
        assert_eq!(StateValue::from_cbor(&bytes), Err(ValueError::Tagged));
    }

    #[test]
    fn rejects_bignum_tag_3() {
        // tag 3 (negative bignum) over the same 1-byte string.
        let bytes = vec![0xC3, 0x41, 0x05];
        assert_eq!(StateValue::from_cbor(&bytes), Err(ValueError::Tagged));
    }

    #[test]
    fn rejects_too_deep() {
        let mut value = vosu_plugin_api::CborValue::Null;
        for _ in 0..=MAX_DEPTH {
            value = vosu_plugin_api::CborValue::Array(vec![value]);
        }
        let bytes = write(&value);
        assert_eq!(StateValue::from_cbor(&bytes), Err(ValueError::TooDeep));
    }

    /// `MAX_DEPTH` nested empty arrays, matching the walker's `json_depth`:
    /// a container one level deeper than its deepest child, so `MAX_DEPTH`
    /// levels of pure container nesting is the accepted boundary.
    #[test]
    fn accepts_exactly_max_depth() {
        let mut value = vosu_plugin_api::CborValue::Array(Vec::new());
        for _ in 0..MAX_DEPTH - 1 {
            value = vosu_plugin_api::CborValue::Array(vec![value]);
        }
        let bytes = write(&value);
        StateValue::from_cbor(&bytes).expect("MAX_DEPTH levels of nesting must decode");
    }

    #[test]
    fn rejects_duplicate_map_keys() {
        let bytes = write(&vosu_plugin_api::CborValue::Map(vec![
            (
                vosu_plugin_api::CborValue::Text("a".into()),
                vosu_plugin_api::CborValue::Integer(1.into()),
            ),
            (
                vosu_plugin_api::CborValue::Text("a".into()),
                vosu_plugin_api::CborValue::Integer(2.into()),
            ),
        ]));
        assert_eq!(
            StateValue::from_cbor(&bytes),
            Err(ValueError::DuplicateKey("a".into()))
        );
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = write(&vosu_plugin_api::CborValue::Integer(1.into()));
        bytes.push(0xFF);
        assert_eq!(
            StateValue::from_cbor(&bytes),
            Err(ValueError::TrailingBytes)
        );
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
                    // Deduped: a well-formed value tree never has a
                    // duplicate map key once decoded (P3), so the
                    // generator must not produce one either.
                    let mut seen = std::collections::HashSet::new();
                    let fields = entries
                        .into_iter()
                        .filter(|(k, _)| seen.insert(k.clone()))
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
