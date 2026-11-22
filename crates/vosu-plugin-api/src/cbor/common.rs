//! Shared CBOR shapes used across more than one opcode.

use ciborium::Value as CborValue;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};

/// `{"code": string, "message": string}`, the inline error shape shared by
/// `VOSU_OP_SCAN_RESULT` (§6.4), `VOSU_OP_RESOURCE_RESOLVED`/
/// `VOSU_OP_STREAM_CHUNK` (§6.5), and `VOSU_OP_STATE_UPDATE`'s per-entry
/// error (§6.12).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
}

/// Deserializes a CBOR value that the spec requires to be a map (minor 9:
/// `VOSU_OP_LOG_RECORD`'s `fields`, `VOSU_OP_SETTINGS_CHANGED`'s `values`),
/// rejecting any other CBOR major type rather than accepting it and
/// forwarding a shape downstream code does not expect.
pub(crate) fn deserialize_cbor_map<'de, D>(deserializer: D) -> Result<CborValue, D::Error>
where
    D: Deserializer<'de>,
{
    let value = CborValue::deserialize(deserializer)?;
    if value.is_map() {
        Ok(value)
    } else {
        Err(D::Error::custom("expected a CBOR map"))
    }
}
