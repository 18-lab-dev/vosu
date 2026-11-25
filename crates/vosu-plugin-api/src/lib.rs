//! vosu 2.0 plugin ABI v1: `repr(C)` types mirroring `include/vosu_plugin.h`,
//! field by field, plus the message codecs and validation built on top of
//! them.
//!
//! This crate is the implementer-grade contract described in
//! `docs/v2/plugin-abi.md` §1–§5 and §9; the canonical layout table is §3.
//! Every public type here must read as the same contract as the C header:
//! same name, same field order, same size. Keep both in sync (header
//! comment, `include/vosu_plugin.h:1-5`).
#![forbid(unsafe_op_in_unsafe_fn)]
#![expect(
    non_camel_case_types,
    reason = "mirrors include/vosu_plugin.h's type and enumerator names exactly, so the header and these Rust types read as the same contract rather than two spellings of it"
)]
mod capability;
pub mod catalog;
mod cbor;
mod envelope;
mod flags;
mod flat;
mod limits;
/// Unified package validation, explicit registration context and pure
/// requirement matching. Asset-backed namespace extraction/catalog comparison
/// are separate from intrinsic validity and application host support.
pub mod manifest;
#[forbid(unsafe_code)]
pub mod memory;
mod message;
mod opcode;
mod primitives;
#[doc(hidden)]
pub mod read_plan_profile;
mod sdk;
mod status;
mod validate;
mod value;
mod vtable;

pub use capability::{CapabilityError, CapabilityId, covers, field_to_pointer, parse_capability};
pub use cbor::action::{
    ActionCall, ActionCancel, ActionInvoke, ActionProgress, ActionResult, ActionStatus, Caller,
    CallerKind,
};
pub use cbor::common::ErrorInfo;
pub use cbor::log::{LogLevel, LogRecord};
pub use cbor::read_plan::ReadPlanCancel;
pub use cbor::resource::{
    ResourceRef, ResourceResolve, ResourceResolved, RootDeclare, StreamChunk, StreamClose,
    StreamRead, TreeEntry, TreeList, TreeLookup,
};
pub use cbor::scan::{ScanRequest, ScanResult};
pub use cbor::session::{
    Bitness, DetachReason, FocusChanged, SessionAttached, SessionDecline, SessionDetached,
    SessionIdentify,
};
pub use cbor::settings::SettingsChanged;
pub use cbor::state::{Selector, StateEntry, StateSubscribe, StateUpdate};
pub use cbor::timer::{TimerCancel, TimerFired, TimerSet};
pub use cbor::{CborBytes, CborError, CborValue, decode, encode};
pub use envelope::{vosu_bytes, vosu_message};
pub use flags::VOSU_READ_PLAN_SYNC;
pub use flat::demand::vosu_demand_changed_header;
pub use flat::pointer::vosu_pointer_ref;
pub use flat::publish::{vosu_field_publish_header, vosu_publish_batch_header, vosu_scalar};
pub use flat::read_plan::{
    vosu_plan_base_flat, vosu_plan_base_kind, vosu_plan_op, vosu_read_error_code,
    vosu_read_plan_result_header, vosu_read_plan_submit_header, vosu_read_result_flat,
    vosu_read_result_kind, vosu_stage_base, vosu_stage_base_kind, vosu_stage_header,
};
pub use limits::{
    VOSU_FRAME_MAX_BYTES, VOSU_PLUGIN_ABI_MAJOR, VOSU_PUBLISH_BATCH_MAX_BYTES,
    VOSU_READ_PLAN_MAX_BYTES, VOSU_STREAM_CHUNK_MAX_BYTES,
};
pub use manifest::{
    ActionDeclaration, ActionSessionMode, Capability, MatchTable, PluginManifest, ResourceKind,
    Runtime, Target,
};
#[cfg(feature = "unstable-sync-reads")]
pub use message::flat::StageRef;
pub use message::flat::{
    CborAppend, DemandChanged, FieldSelector, PlanBase, PlanOp, PublishBatch, PublishBatchRef,
    PublishBatchWriter, PublishField, PublishFieldRef, PublishValue, PublishValueRef,
    ReadErrorCode, ReadPlanResult, ReadPlanSubmit, ReadResult, Stage, StageBase,
    decode_publish_batch_ref, decode_publish_field_ref,
};
pub use message::{HostServices, Message, MessageError};
pub use opcode::{VOSU_OPCODE_PRIVATE_BIT, vosu_opcode};
pub use primitives::{
    vosu_invocation_id, vosu_plan_id, vosu_resource_id, vosu_session_id, vosu_timer_id,
};
#[doc(hidden)]
pub use sdk::{__vtable_for_concurrent, __vtable_for_serialized};
pub use sdk::{ConcurrentPlugin, Init, Plugin, PluginError, VtableHostServices};
#[cfg(feature = "unstable-sync-reads")]
pub use sdk::{FlatReadResults, PlanMemory, PlanMemoryError, SyncReadBuffers};
pub use status::vosu_status;
pub use validate::{
    ValidationError, read_flat_header, validate_array_range, validate_bytes_range,
    validate_demand_changed, validate_flags, validate_frame_len, validate_publish_batch,
    validate_publish_field, validate_read_plan_len, validate_read_plan_ref_op_width,
    validate_read_plan_result, validate_read_plan_submit, validate_stream_chunk_len,
};
pub use value::vosu_value_kind;
pub use vtable::{VosuPluginEntryFn, vosu_host_services, vosu_plugin_vtable};
