//! Shared `ciborium` encode/decode helpers (D13) and the `VOSU_FRAME_MAX_BYTES`
//! cap every CBOR-encoded opcode shares (`docs/v2/plugin-abi.md` §3/§7.2).

use core::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::validate::{ValidationError, validate_frame_len};

/// Every way encoding or decoding a CBOR opcode payload can fail.
#[derive(Debug)]
pub enum CborError {
    /// The encoded payload is, or would be, larger than `VOSU_FRAME_MAX_BYTES`
    /// (the same cap `vosu_message.payload` shares for every opcode).
    TooLarge(ValidationError),
    /// `ciborium` could not serialize the value.
    Encode(ciborium::ser::Error<std::io::Error>),
    /// `ciborium` could not parse the bytes, or they do not match the
    /// target type (an ill-typed field, per the spec's own enum/string
    /// literals).
    Decode(ciborium::de::Error<std::io::Error>),
}

impl fmt::Display for CborError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge(err) => write!(f, "CBOR payload too large: {err:?}"),
            Self::Encode(err) => write!(f, "CBOR encode error: {err}"),
            Self::Decode(err) => write!(f, "CBOR decode error: {err}"),
        }
    }
}

impl std::error::Error for CborError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TooLarge(_) => None,
            Self::Encode(err) => Some(err),
            Self::Decode(err) => Some(err),
        }
    }
}

/// Encodes `value` as CBOR, rejecting a result over `VOSU_FRAME_MAX_BYTES`
/// before returning it.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CborError> {
    let mut buf = Vec::new();
    ciborium::into_writer(value, &mut buf).map_err(CborError::Encode)?;
    validate_frame_len(buf.len()).map_err(CborError::TooLarge)?;
    Ok(buf)
}

/// Decodes a CBOR opcode payload, rejecting one over `VOSU_FRAME_MAX_BYTES`
/// before ever parsing it.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CborError> {
    validate_frame_len(bytes.len()).map_err(CborError::TooLarge)?;
    ciborium::from_reader(bytes).map_err(CborError::Decode)
}

/// A CBOR byte string (major type 2), distinct from a bare `Vec<u8>` field,
/// which `serde`'s blanket sequence impl would otherwise encode as an array
/// of integers. Used wherever the spec marks a field `bytes`
/// (`VOSU_OP_STREAM_CHUNK`'s `data`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CborBytes(pub Vec<u8>);

impl Serialize for CborBytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for CborBytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BytesVisitor;

        impl<'de> serde::de::Visitor<'de> for BytesVisitor {
            type Value = CborBytes;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a CBOR byte string")
            }

            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                Ok(CborBytes(v.to_vec()))
            }

            fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
                Ok(CborBytes(v))
            }
        }

        deserializer.deserialize_bytes(BytesVisitor)
    }
}

/// `false`, for `#[serde(skip_serializing_if = "is_false")]` on a field the
/// spec marks `bool|omitted` with omission equivalent to `false`
/// (`VOSU_OP_SESSION_IDENTIFY`'s `focus_priority`, §6.2).
pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}
