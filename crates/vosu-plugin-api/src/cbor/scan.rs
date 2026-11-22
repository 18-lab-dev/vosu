//! Signature scanning and module enumeration, CBOR
//! (`docs/v2/plugin-abi.md` §6.4).

use serde::{Deserialize, Serialize};

use crate::cbor::common::ErrorInfo;

/// `VOSU_OP_SCAN_REQUEST` (plugin -> host). `pattern` uses exactly
/// `src/process/scanner.rs`'s `Pattern::parse` grammar; `module: None`
/// scans every readable region of the process.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ScanRequest {
    /// a `vosu_session_id` handle, despite the wire name `session`
    pub session: u64,
    #[serde(default)]
    pub module: Option<String>,
    pub pattern: String,
    pub non_zero_mask: bool,
    pub all_matches: bool,
}

/// `VOSU_OP_SCAN_RESULT` (host -> plugin, reply correlated by
/// `correlation_id`). `addresses` holds absolute target-process addresses,
/// suitable as a later read plan's `VOSU_BASE_ABSOLUTE`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ScanResult {
    pub addresses: Vec<u64>,
    #[serde(default)]
    pub error: Option<ErrorInfo>,
}
