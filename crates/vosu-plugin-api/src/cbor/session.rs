//! Session lifecycle and focus, CBOR (`docs/v2/plugin-abi.md` §6.2).

use serde::{Deserialize, Serialize};

use crate::cbor::codec::is_false;

/// The pointer width a dependent read-plan stage uses for this session
/// (`docs/v2/plugin-abi.md` §6.2).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum Bitness {
    #[serde(rename = "x86")]
    X86,
    #[serde(rename = "x64")]
    X64,
}

/// `VOSU_OP_SESSION_ATTACHED` (host -> plugin, no reply). The last four
/// fields are an additive, backward-compatible growth (A1b/DA,
/// `docs/v2/plugin-abi.md` §6.2, §1): process facts the matcher and core
/// already have. Absent means unknown, never the empty string (§6.2
/// amended): `#[serde(default)]` lets an older-shape (pre-amendment)
/// encoded message, which has none of these keys, still decode, each
/// field defaulting to `None` rather than collapsing a missing key to
/// `""`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SessionAttached {
    /// the public id (naming contract)
    pub session: String,
    /// this plugin instance's `vosu_session_id` for every later message
    pub handle: u64,
    pub generation: u64,
    pub pid: u32,
    pub bitness: Bitness,
    /// The executable name the matcher saw: Windows the image file name,
    /// Linux the kernel `comm` value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The full executable path (`ProcessRecord::path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Windows: the raw OS command line. Linux: argv joined by spaces,
    /// exactly `ProcessRecord::command_line`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_line: Option<String>,
    /// Linux only: the exact NUL-split argv.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
}

/// `VOSU_OP_SESSION_DETACHED` (host -> plugin, no reply).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum DetachReason {
    #[serde(rename = "exited")]
    Exited,
    #[serde(rename = "lost")]
    Lost,
    #[serde(rename = "superseded")]
    Superseded,
    #[serde(rename = "declined")]
    Declined,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SessionDetached {
    pub handle: u64,
    pub reason: DetachReason,
}

/// `VOSU_OP_SESSION_IDENTIFY` (plugin -> host, no reply). `focus_priority`
/// is `bool|omitted`; omitting it is equivalent to sending `false`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct SessionIdentify {
    pub handle: u64,
    pub game: String,
    pub variant: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub focus_priority: bool,
}

/// `VOSU_OP_SESSION_DECLINE` (plugin -> host, no reply): valid only before
/// this plugin's own `SESSION_IDENTIFY` for the handle (D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct SessionDecline {
    pub handle: u64,
}

/// `VOSU_OP_FOCUS_CHANGED` (host -> plugin, no reply). `foreground` is
/// additive (A1b/DA, `docs/v2/plugin-abi.md` §6.2, ME): `true` when the OS
/// foreground window's pid is this session's pid, sent only where core has
/// a real foreground source (Windows) and absent elsewhere — never
/// collapsed to `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct FocusChanged {
    pub handle: u64,
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<bool>,
}
