//! Payloads of the private lifecycle frames between core and one
//! `vosu plugin-host` generation (`docs/v2/plugin-abi.md` §7.6, A5.2/A5.8):
//! `CREATE`, `READY` and `FAILED`, the startup stages a failure names, and
//! the private exit statuses a child uses before its channel exists.
//!
//! Every payload is CBOR with exactly the frozen keys; decoding refuses an
//! unknown or missing key. Both sides are the same executable generation,
//! so there is no version negotiation, and core still treats everything a
//! child sends as an untrusted claim: [`Failed`]'s message length and
//! [`Ready`]'s descriptor header are checked on decode.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vosu_plugin_api::{CborBytes, Init, decode, encode};

/// Exit status of a child that refused its arguments (stage `arguments`).
pub const EXIT_ARGUMENTS: i32 = 70;
/// Exit status of a child built as a different executable generation than
/// the core that started it (stage `generation`).
pub const EXIT_GENERATION: i32 = 71;
/// Exit status of a child whose parent check failed (stage `parent`).
pub const EXIT_PARENT: i32 = 72;
/// Exit status of a child that refused its inherited endpoints (stage
/// `endpoints`).
pub const EXIT_ENDPOINTS: i32 = 73;
/// Exit status after the child reported `FAILED` over its channel.
pub const EXIT_REPORTED: i32 = 74;
/// Exit status after the channel ended without a controlled `DESTROY`
/// (core gone, a core protocol violation, or a stalled write).
pub const EXIT_CHANNEL_LOST: i32 = 75;
/// Exit status after a status waiter's backstop expired (§7.6: the child
/// closes the channel and exits rather than fabricate a status).
pub const EXIT_TRANSPORT_TIMEOUT: i32 = 76;

/// The longest `FAILED` message, in bytes (§7.6).
pub const MAX_FAILED_MESSAGE: usize = 512;
/// The longest `FAILED` code, in bytes. Codes are short fixed identifiers.
pub const MAX_FAILED_CODE: usize = 64;

/// The §7.6 startup stage a failure belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StartupStage {
    /// Core could not start the child (including a Linux pre-exec step).
    Spawn,
    /// Windows job creation or assignment.
    Job,
    Arguments,
    /// A build-identity mismatch, for example after an in-place update.
    Generation,
    Parent,
    Endpoints,
    Security,
    /// The library's SHA-256 differs from the approved digest.
    Approval,
    Library,
    Abi,
    /// The plugin's `create` failed, or `init` exceeded the frame cap.
    Create,
    /// No `READY` or `FAILED` within the startup deadline.
    Timeout,
    /// The child exited, or broke its channel, before `READY` without
    /// reporting `FAILED`.
    Crash,
}

impl StartupStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Job => "job",
            Self::Arguments => "arguments",
            Self::Generation => "generation",
            Self::Parent => "parent",
            Self::Endpoints => "endpoints",
            Self::Security => "security",
            Self::Approval => "approval",
            Self::Library => "library",
            Self::Abi => "abi",
            Self::Create => "create",
            Self::Timeout => "timeout",
            Self::Crash => "crash",
        }
    }

    /// The stages a child may name in `FAILED`. Every other stage is
    /// detected by core itself, so a `FAILED` naming one is a violation.
    fn from_child(text: &str) -> Option<Self> {
        Some(match text {
            "security" => Self::Security,
            "approval" => Self::Approval,
            "library" => Self::Library,
            "abi" => Self::Abi,
            "create" => Self::Create,
            _ => return None,
        })
    }

    /// The stage a child's exit status names when it exited before its
    /// channel existed, or `None` for any other status.
    pub fn from_exit_status(code: i32) -> Option<Self> {
        Some(match code {
            EXIT_ARGUMENTS => Self::Arguments,
            EXIT_GENERATION => Self::Generation,
            EXIT_PARENT => Self::Parent,
            EXIT_ENDPOINTS => Self::Endpoints,
            _ => return None,
        })
    }

    /// Whether a failure at this stage is one crash event during an
    /// automatic restart (§7.6 startup table). Every other stage is a
    /// deterministic refusal: `failed`, never retried, never a crash.
    pub fn is_crash_class(self) -> bool {
        matches!(self, Self::Create | Self::Timeout | Self::Crash)
    }
}

/// One failed startup, as core records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupFailure {
    pub stage: StartupStage,
    pub code: String,
    pub errno: Option<i32>,
    pub message: String,
}

impl StartupFailure {
    pub fn new(stage: StartupStage, code: &str, message: impl Into<String>) -> Self {
        Self {
            stage,
            code: code.to_owned(),
            errno: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StartupFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "stage {} ({}){}: {}",
            self.stage.as_str(),
            self.code,
            match self.errno {
                Some(errno) => format!(", errno {errno}"),
                None => String::new(),
            },
            self.message
        )
    }
}

/// A lifecycle payload that does not satisfy the frozen contract: always a
/// protocol violation by its sender.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("malformed control payload: {0}")]
    Cbor(String),
    #[error("library_sha256 is {0} bytes, not 32")]
    DigestLength(usize),
    #[error("FAILED names stage {0:?}, which only core detects")]
    Stage(String),
    #[error("FAILED message is {0} bytes, above the {MAX_FAILED_MESSAGE}-byte cap")]
    MessageLength(usize),
    #[error("FAILED code is {0} bytes, above the {MAX_FAILED_CODE}-byte cap")]
    CodeLength(usize),
}

/// `CREATE` (core → child): exactly the SDK-validated fields the child
/// needs. The child never parses `manifest.toml`.
#[derive(Debug, Clone)]
pub struct Create {
    pub generation: u64,
    pub plugin_id: String,
    /// Absolute, canonical path of the library for the running target.
    pub library: PathBuf,
    /// The approved digest the child checks the opened library against.
    pub library_sha256: [u8; 32],
    pub concurrent_dispatch: bool,
    pub sync_budget_ms: u32,
    /// Exactly the in-process `create` init.
    pub init: Init,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateWire {
    generation: u64,
    plugin_id: String,
    library: PathBuf,
    library_sha256: CborBytes,
    concurrent_dispatch: bool,
    sync_budget_ms: u32,
    init: Init,
}

impl Create {
    /// Encodes the payload; fails when it would exceed the frame cap (the
    /// `create` stage's "init over the frame cap").
    pub fn encode(&self) -> Result<Vec<u8>, ControlError> {
        encode(&CreateWire {
            generation: self.generation,
            plugin_id: self.plugin_id.clone(),
            library: self.library.clone(),
            library_sha256: CborBytes(self.library_sha256.to_vec()),
            concurrent_dispatch: self.concurrent_dispatch,
            sync_budget_ms: self.sync_budget_ms,
            init: self.init.clone(),
        })
        .map_err(|error| ControlError::Cbor(error.to_string()))
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ControlError> {
        let wire: CreateWire =
            decode(payload).map_err(|error| ControlError::Cbor(error.to_string()))?;
        let library_sha256 = <[u8; 32]>::try_from(wire.library_sha256.0.as_slice())
            .map_err(|_| ControlError::DigestLength(wire.library_sha256.0.len()))?;
        Ok(Self {
            generation: wire.generation,
            plugin_id: wire.plugin_id,
            library: wire.library,
            library_sha256,
            concurrent_dispatch: wire.concurrent_dispatch,
            sync_budget_ms: wire.sync_budget_ms,
            init: wire.init,
        })
    }
}

/// `READY` (child → core): the plugin's `create` succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ready {
    pub generation: u64,
    pub struct_size: u32,
    pub abi_version: u32,
}

impl Ready {
    pub fn encode(&self) -> Vec<u8> {
        encode(self).expect("a three-integer map is far below the frame cap")
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ControlError> {
        decode(payload).map_err(|error| ControlError::Cbor(error.to_string()))
    }
}

/// `FAILED` (child → core): a startup stage the child detected. A failure
/// before `CREATE` was read carries generation 0, which no real generation
/// uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub generation: u64,
    pub stage: StartupStage,
    pub code: String,
    pub errno: Option<i32>,
    pub message: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FailedWire {
    generation: u64,
    stage: String,
    code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    errno: Option<i32>,
    message: String,
}

impl Failed {
    /// Encodes the payload, truncating `message` to [`MAX_FAILED_MESSAGE`]
    /// bytes at a character boundary.
    pub fn encode(&self) -> Vec<u8> {
        let mut end = self.message.len().min(MAX_FAILED_MESSAGE);
        while !self.message.is_char_boundary(end) {
            end -= 1;
        }
        encode(&FailedWire {
            generation: self.generation,
            stage: self.stage.as_str().to_owned(),
            code: self.code.clone(),
            errno: self.errno,
            message: self.message[..end].to_owned(),
        })
        .expect("a bounded failure report is far below the frame cap")
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ControlError> {
        let wire: FailedWire =
            decode(payload).map_err(|error| ControlError::Cbor(error.to_string()))?;
        let stage = StartupStage::from_child(&wire.stage).ok_or(ControlError::Stage(wire.stage))?;
        if wire.message.len() > MAX_FAILED_MESSAGE {
            return Err(ControlError::MessageLength(wire.message.len()));
        }
        if wire.code.len() > MAX_FAILED_CODE {
            return Err(ControlError::CodeLength(wire.code.len()));
        }
        Ok(Self {
            generation: wire.generation,
            stage,
            code: wire.code,
            errno: wire.errno,
            message: wire.message,
        })
    }

    pub fn into_failure(self) -> StartupFailure {
        StartupFailure {
            stage: self.stage,
            code: self.code,
            errno: self.errno,
            message: self.message,
        }
    }
}

#[cfg(test)]
mod tests;
