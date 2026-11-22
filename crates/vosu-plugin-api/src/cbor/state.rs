//! Cross-plugin state reads, CBOR (`docs/v2/plugin-abi.md` §6.12).

use ciborium::Value as CborValue;
use serde::{Deserialize, Serialize};

use crate::cbor::common::ErrorInfo;

/// One `(namespace, pointer)` selector.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Selector {
    pub ns: String,
    pub pointer: String,
}

/// `VOSU_OP_STATE_SUBSCRIBE` (plugin -> host, no reply): **replaces** this
/// plugin's previous selection for the named session wholesale; an empty
/// `selectors` list unsubscribes entirely. `session: None` with the
/// reserved `vosu.sessions` namespace subscribes to session lifecycle
/// events instead (§6.2).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct StateSubscribe {
    #[serde(default)]
    pub session: Option<u64>,
    pub selectors: Vec<Selector>,
}

/// One `STATE_UPDATE` entry. An absent `value`/`error` reports `None` for
/// both, matching the HTTP/WebSocket absence rule.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct StateEntry {
    pub ns: String,
    pub pointer: String,
    #[serde(default)]
    pub value: Option<CborValue>,
    #[serde(default)]
    pub error: Option<ErrorInfo>,
}

/// `VOSU_OP_STATE_UPDATE` (host -> plugin, no reply): sent once
/// immediately with the current values and again on every change,
/// latest-only. `more: true` on every part but the last of a split
/// delivery; the receiver applies the accumulated set only once a
/// `more: false` part completes it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct StateUpdate {
    pub session: u64,
    pub values: Vec<StateEntry>,
    pub more: bool,
}
