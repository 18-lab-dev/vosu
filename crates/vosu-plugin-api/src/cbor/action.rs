//! Actions, CBOR (`docs/v2/plugin-abi.md` §6.8, §6.12).

use ciborium::Value as CborValue;
use serde::{Deserialize, Serialize};

/// `caller.kind` on `VOSU_OP_ACTION_INVOKE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum CallerKind {
    #[serde(rename = "tool")]
    Tool,
    #[serde(rename = "plugin")]
    Plugin,
    #[serde(rename = "cli")]
    Cli,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Caller {
    pub kind: CallerKind,
    pub id: String,
}

/// `VOSU_OP_ACTION_INVOKE` (host -> plugin). `session` is `null` for an
/// action declared `session: "none"`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ActionInvoke {
    pub invocation: u64,
    /// Bare fully qualified action id, never prefixed with `action:`.
    pub action: String,
    #[serde(default)]
    pub session: Option<u64>,
    pub caller: Caller,
    pub args: CborValue,
}

/// `VOSU_OP_ACTION_PROGRESS` (bidirectional, zero or more, no reply).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ActionProgress {
    pub invocation: u64,
    #[serde(default)]
    pub fraction: Option<f64>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum ActionStatus {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "error")]
    Error,
}

/// `VOSU_OP_ACTION_RESULT` (bidirectional, exactly once).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ActionResult {
    pub invocation: u64,
    pub status: ActionStatus,
    #[serde(default)]
    pub value: Option<CborValue>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// `VOSU_OP_ACTION_CANCEL` (bidirectional): host -> plugin to ask a callee
/// to stop, plugin -> host to cancel its own outstanding `ACTION_CALL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct ActionCancel {
    pub invocation: u64,
}

/// `VOSU_OP_ACTION_CALL` (plugin -> host): this plugin invokes another
/// plugin's declared action, `action` in the bare `<plugin-id>.<name>` form.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ActionCall {
    pub invocation: u64,
    pub action: String,
    #[serde(default)]
    pub session: Option<u64>,
    pub args: CborValue,
}
