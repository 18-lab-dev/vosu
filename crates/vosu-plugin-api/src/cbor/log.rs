//! Logging, CBOR (`docs/v2/plugin-abi.md` §6.10).

use ciborium::Value as CborValue;
use serde::{Deserialize, Serialize};

use crate::cbor::common::deserialize_cbor_map;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum LogLevel {
    #[serde(rename = "trace")]
    Trace,
    #[serde(rename = "debug")]
    Debug,
    #[serde(rename = "info")]
    Info,
    #[serde(rename = "warn")]
    Warn,
    #[serde(rename = "error")]
    Error,
}

/// `VOSU_OP_LOG_RECORD` (plugin -> host, no reply). Records are
/// rate-limited per plugin instance (200/second) by the host. `fields` is
/// always a CBOR map (minor 9): a non-map value is rejected on decode.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct LogRecord {
    pub level: LogLevel,
    pub message: String,
    #[serde(deserialize_with = "deserialize_cbor_map")]
    pub fields: CborValue,
}
