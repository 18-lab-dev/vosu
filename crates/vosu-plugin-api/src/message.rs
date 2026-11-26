//! `Message`: a typed union spanning the flat hot-path payloads and every
//! CBOR opcode (`docs/v2/plugin-abi.md` §9's sketch), and `HostServices`,
//! the safe surface over the single `vosu_host_services::send` entry
//! point.

pub mod flat;
mod host_services;

pub use host_services::HostServices;

use core::fmt;

use crate::cbor::{self, CborError};
use crate::flags::VOSU_READ_PLAN_SYNC;
use crate::opcode::vosu_opcode;
use crate::validate::{self, ValidationError};
use crate::{
    ActionCall, ActionCancel, ActionInvoke, ActionProgress, ActionResult, ArtifactDeclare,
    ArtifactDeclared, ArtifactOpen, ArtifactRelease, FocusChanged, LogRecord,
    ReadPlanCancel, ResourceResolve, ResourceResolved, RootDeclare, ScanRequest, ScanResult,
    SessionAttached, SessionDecline, SessionDetached, SessionIdentify, SettingsChanged,
    StateSubscribe, StateUpdate, StreamChunk, StreamClose, StreamRead, TimerCancel, TimerFired,
    TimerSet, TreeList, TreeLookup,
};

/// Every way decoding or encoding a [`Message`] can fail.
#[derive(Debug)]
pub enum MessageError {
    /// `vosu_message.flags` set a bit this opcode never recognizes (D4).
    Flat(ValidationError),
    Cbor(CborError),
    /// A flat payload's `{offset, len}` byte range held bytes that are not
    /// valid UTF-8 where the spec requires a string.
    InvalidUtf8(core::str::Utf8Error),
}

impl From<ValidationError> for MessageError {
    fn from(err: ValidationError) -> Self {
        Self::Flat(err)
    }
}

impl From<CborError> for MessageError {
    fn from(err: CborError) -> Self {
        Self::Cbor(err)
    }
}

impl fmt::Display for MessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flat(err) => write!(f, "flat payload error: {err:?}"),
            Self::Cbor(err) => write!(f, "CBOR payload error: {err}"),
            Self::InvalidUtf8(err) => write!(f, "invalid UTF-8 in a flat string field: {err}"),
        }
    }
}

impl std::error::Error for MessageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Flat(_) => None,
            Self::Cbor(err) => Some(err),
            Self::InvalidUtf8(err) => Some(err),
        }
    }
}

/// A host <-> plugin message, decoded into owned Rust data: spans both the
/// hot-path flat opcodes (`crate::flat`, `crate::message::flat`) and every
/// CBOR opcode (`docs/v2/plugin-abi.md` §9).
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    PublishField(flat::PublishField),
    PublishBatch(flat::PublishBatch),
    DemandChanged(flat::DemandChanged),
    ReadPlanSubmit(flat::ReadPlanSubmit),
    ReadPlanResult(flat::ReadPlanResult),
    ReadPlanCancel(ReadPlanCancel),
    SessionAttached(SessionAttached),
    SessionDetached(SessionDetached),
    SessionIdentify(SessionIdentify),
    FocusChanged(FocusChanged),
    SessionDecline(SessionDecline),
    ScanRequest(ScanRequest),
    ScanResult(ScanResult),
    ResourceResolve(ResourceResolve),
    ResourceResolved(ResourceResolved),
    TreeList(TreeList),
    TreeLookup(TreeLookup),
    StreamRead(StreamRead),
    StreamChunk(StreamChunk),
    RootDeclare(RootDeclare),
    StreamClose(StreamClose),
    ActionInvoke(ActionInvoke),
    ActionResult(ActionResult),
    ActionCancel(ActionCancel),
    ActionProgress(ActionProgress),
    ActionCall(ActionCall),
    SettingsChanged(SettingsChanged),
    LogRecord(LogRecord),
    TimerSet(TimerSet),
    TimerCancel(TimerCancel),
    TimerFired(TimerFired),
    StateSubscribe(StateSubscribe),
    StateUpdate(StateUpdate),
    ArtifactDeclare(ArtifactDeclare),
    ArtifactDeclared(ArtifactDeclared),
    ArtifactOpen(ArtifactOpen),
    ArtifactRelease(ArtifactRelease),
}

impl Message {
    /// The `vosu_opcode` this message carries.
    pub fn opcode(&self) -> vosu_opcode {
        match self {
            Self::PublishField(_) => vosu_opcode::VOSU_OP_PUBLISH_FIELD,
            Self::DemandChanged(_) => vosu_opcode::VOSU_OP_DEMAND_CHANGED,
            Self::ReadPlanSubmit(_) => vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT,
            Self::ReadPlanResult(_) => vosu_opcode::VOSU_OP_READ_PLAN_RESULT,
            Self::ReadPlanCancel(_) => vosu_opcode::VOSU_OP_READ_PLAN_CANCEL,
            Self::PublishBatch(_) => vosu_opcode::VOSU_OP_PUBLISH_BATCH,
            Self::SessionAttached(_) => vosu_opcode::VOSU_OP_SESSION_ATTACHED,
            Self::SessionDetached(_) => vosu_opcode::VOSU_OP_SESSION_DETACHED,
            Self::SessionIdentify(_) => vosu_opcode::VOSU_OP_SESSION_IDENTIFY,
            Self::FocusChanged(_) => vosu_opcode::VOSU_OP_FOCUS_CHANGED,
            Self::SessionDecline(_) => vosu_opcode::VOSU_OP_SESSION_DECLINE,
            Self::ScanRequest(_) => vosu_opcode::VOSU_OP_SCAN_REQUEST,
            Self::ScanResult(_) => vosu_opcode::VOSU_OP_SCAN_RESULT,
            Self::ResourceResolve(_) => vosu_opcode::VOSU_OP_RESOURCE_RESOLVE,
            Self::ResourceResolved(_) => vosu_opcode::VOSU_OP_RESOURCE_RESOLVED,
            Self::TreeList(_) => vosu_opcode::VOSU_OP_TREE_LIST,
            Self::TreeLookup(_) => vosu_opcode::VOSU_OP_TREE_LOOKUP,
            Self::StreamRead(_) => vosu_opcode::VOSU_OP_STREAM_READ,
            Self::StreamChunk(_) => vosu_opcode::VOSU_OP_STREAM_CHUNK,
            Self::RootDeclare(_) => vosu_opcode::VOSU_OP_ROOT_DECLARE,
            Self::StreamClose(_) => vosu_opcode::VOSU_OP_STREAM_CLOSE,
            Self::ActionInvoke(_) => vosu_opcode::VOSU_OP_ACTION_INVOKE,
            Self::ActionResult(_) => vosu_opcode::VOSU_OP_ACTION_RESULT,
            Self::ActionCancel(_) => vosu_opcode::VOSU_OP_ACTION_CANCEL,
            Self::ActionProgress(_) => vosu_opcode::VOSU_OP_ACTION_PROGRESS,
            Self::ActionCall(_) => vosu_opcode::VOSU_OP_ACTION_CALL,
            Self::SettingsChanged(_) => vosu_opcode::VOSU_OP_SETTINGS_CHANGED,
            Self::LogRecord(_) => vosu_opcode::VOSU_OP_LOG_RECORD,
            Self::TimerSet(_) => vosu_opcode::VOSU_OP_TIMER_SET,
            Self::TimerCancel(_) => vosu_opcode::VOSU_OP_TIMER_CANCEL,
            Self::TimerFired(_) => vosu_opcode::VOSU_OP_TIMER_FIRED,
            Self::StateSubscribe(_) => vosu_opcode::VOSU_OP_STATE_SUBSCRIBE,
            Self::StateUpdate(_) => vosu_opcode::VOSU_OP_STATE_UPDATE,
            Self::ArtifactDeclare(_) => vosu_opcode::VOSU_OP_ARTIFACT_DECLARE,
            Self::ArtifactDeclared(_) => vosu_opcode::VOSU_OP_ARTIFACT_DECLARED,
            Self::ArtifactOpen(_) => vosu_opcode::VOSU_OP_ARTIFACT_OPEN,
            Self::ArtifactRelease(_) => vosu_opcode::VOSU_OP_ARTIFACT_RELEASE,
        }
    }

    /// `vosu_message.flags` this message carries: only a
    /// `VOSU_READ_PLAN_SUBMIT` may set `VOSU_READ_PLAN_SYNC` (D5); every
    /// other opcode is always `0`.
    pub fn flags(&self) -> u32 {
        match self {
            Self::ReadPlanSubmit(submit) if submit.sync => VOSU_READ_PLAN_SYNC,
            _ => 0,
        }
    }

    /// Decodes one `vosu_message`'s already-validated-length `payload`
    /// bytes, given its `opcode` and `flags`.
    pub fn decode(opcode: vosu_opcode, flags: u32, payload: &[u8]) -> Result<Self, MessageError> {
        validate::validate_flags(opcode, flags)?;
        let sync = flags & VOSU_READ_PLAN_SYNC != 0;
        Ok(match opcode {
            vosu_opcode::VOSU_OP_PUBLISH_FIELD => {
                Self::PublishField(flat::decode_publish_field(payload)?)
            }
            vosu_opcode::VOSU_OP_PUBLISH_BATCH => {
                Self::PublishBatch(flat::decode_publish_batch(payload)?)
            }
            vosu_opcode::VOSU_OP_DEMAND_CHANGED => {
                Self::DemandChanged(flat::decode_demand_changed(payload)?)
            }
            vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT => {
                Self::ReadPlanSubmit(flat::decode_read_plan_submit(payload, sync)?)
            }
            vosu_opcode::VOSU_OP_READ_PLAN_RESULT => {
                Self::ReadPlanResult(flat::decode_read_plan_result(payload)?)
            }
            vosu_opcode::VOSU_OP_READ_PLAN_CANCEL => Self::ReadPlanCancel(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_SESSION_ATTACHED => Self::SessionAttached(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_SESSION_DETACHED => Self::SessionDetached(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_SESSION_IDENTIFY => Self::SessionIdentify(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_FOCUS_CHANGED => Self::FocusChanged(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_SESSION_DECLINE => Self::SessionDecline(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_SCAN_REQUEST => Self::ScanRequest(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_SCAN_RESULT => Self::ScanResult(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_RESOURCE_RESOLVE => Self::ResourceResolve(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_RESOURCE_RESOLVED => {
                Self::ResourceResolved(cbor::decode(payload)?)
            }
            vosu_opcode::VOSU_OP_TREE_LIST => Self::TreeList(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_TREE_LOOKUP => Self::TreeLookup(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_STREAM_READ => Self::StreamRead(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_STREAM_CHUNK => Self::StreamChunk(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_ROOT_DECLARE => Self::RootDeclare(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_STREAM_CLOSE => Self::StreamClose(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_ACTION_INVOKE => Self::ActionInvoke(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_ACTION_RESULT => Self::ActionResult(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_ACTION_CANCEL => Self::ActionCancel(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_ACTION_PROGRESS => Self::ActionProgress(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_ACTION_CALL => Self::ActionCall(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_SETTINGS_CHANGED => Self::SettingsChanged(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_LOG_RECORD => Self::LogRecord(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_TIMER_SET => Self::TimerSet(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_TIMER_CANCEL => Self::TimerCancel(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_TIMER_FIRED => Self::TimerFired(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_STATE_SUBSCRIBE => Self::StateSubscribe(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_STATE_UPDATE => Self::StateUpdate(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_ARTIFACT_DECLARE => Self::ArtifactDeclare(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_ARTIFACT_DECLARED => {
                Self::ArtifactDeclared(cbor::decode(payload)?)
            }
            vosu_opcode::VOSU_OP_ARTIFACT_OPEN => Self::ArtifactOpen(cbor::decode(payload)?),
            vosu_opcode::VOSU_OP_ARTIFACT_RELEASE => Self::ArtifactRelease(cbor::decode(payload)?),
        })
    }

    /// Encodes this message's payload bytes (its opcode/flags are
    /// [`Message::opcode`]/[`Message::flags`]).
    pub fn encode_payload(&self) -> Result<Vec<u8>, MessageError> {
        Ok(match self {
            Self::PublishField(m) => flat::encode_publish_field(m)?,
            Self::PublishBatch(m) => flat::encode_publish_batch(m)?,
            Self::DemandChanged(m) => flat::encode_demand_changed(m)?,
            Self::ReadPlanSubmit(m) => flat::encode_read_plan_submit(m)?,
            Self::ReadPlanResult(m) => flat::encode_read_plan_result(m)?,
            Self::ReadPlanCancel(m) => cbor::encode(m)?,
            Self::SessionAttached(m) => cbor::encode(m)?,
            Self::SessionDetached(m) => cbor::encode(m)?,
            Self::SessionIdentify(m) => cbor::encode(m)?,
            Self::FocusChanged(m) => cbor::encode(m)?,
            Self::SessionDecline(m) => cbor::encode(m)?,
            Self::ScanRequest(m) => cbor::encode(m)?,
            Self::ScanResult(m) => cbor::encode(m)?,
            Self::ResourceResolve(m) => cbor::encode(m)?,
            Self::ResourceResolved(m) => cbor::encode(m)?,
            Self::TreeList(m) => cbor::encode(m)?,
            Self::TreeLookup(m) => cbor::encode(m)?,
            Self::StreamRead(m) => cbor::encode(m)?,
            Self::StreamChunk(m) => cbor::encode(m)?,
            Self::RootDeclare(m) => cbor::encode(m)?,
            Self::StreamClose(m) => cbor::encode(m)?,
            Self::ActionInvoke(m) => cbor::encode(m)?,
            Self::ActionResult(m) => cbor::encode(m)?,
            Self::ActionCancel(m) => cbor::encode(m)?,
            Self::ActionProgress(m) => cbor::encode(m)?,
            Self::ActionCall(m) => cbor::encode(m)?,
            Self::SettingsChanged(m) => cbor::encode(m)?,
            Self::LogRecord(m) => cbor::encode(m)?,
            Self::TimerSet(m) => cbor::encode(m)?,
            Self::TimerCancel(m) => cbor::encode(m)?,
            Self::TimerFired(m) => cbor::encode(m)?,
            Self::StateSubscribe(m) => cbor::encode(m)?,
            Self::StateUpdate(m) => cbor::encode(m)?,
            Self::ArtifactDeclare(m) => cbor::encode(m)?,
            Self::ArtifactDeclared(m) => cbor::encode(m)?,
            Self::ArtifactOpen(m) => cbor::encode(m)?,
            Self::ArtifactRelease(m) => cbor::encode(m)?,
        })
    }
}
