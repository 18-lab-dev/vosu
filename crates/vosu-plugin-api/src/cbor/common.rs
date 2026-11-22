//! Shared CBOR shapes used across more than one opcode.

use serde::{Deserialize, Serialize};

/// `{"code": string, "message": string}`, the inline error shape shared by
/// `VOSU_OP_SCAN_RESULT` (§6.4), `VOSU_OP_RESOURCE_RESOLVED`/
/// `VOSU_OP_STREAM_CHUNK` (§6.5), and `VOSU_OP_STATE_UPDATE`'s per-entry
/// error (§6.12).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
}
