//! Logging, CBOR (`docs/v2/plugin-abi.md` §6.10).

use ciborium::Value as CborValue;
use serde::{Deserialize, Serialize};

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
/// rate-limited per plugin instance (200/second) by the host.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct LogRecord {
    pub level: LogLevel,
    pub message: String,
    pub fields: CborValue,
}
