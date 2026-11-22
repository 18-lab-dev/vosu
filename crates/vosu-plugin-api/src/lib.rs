//! vosu 2.0 plugin ABI v1: `repr(C)` types mirroring `include/vosu_plugin.h`,
//! field by field, plus the message codecs and validation built on top of
//! them.
//!
//! This crate is the implementer-grade contract described in
//! `docs/v2/plugin-abi.md` §1–§5 and §9; the canonical layout table is §3.
//! Every public type here must read as the same contract as the C header:
//! same name, same field order, same size. Keep both in sync (header
//! comment, `include/vosu_plugin.h:1-5`).
#![expect(
    non_camel_case_types,
    reason = "mirrors include/vosu_plugin.h's type and enumerator names exactly, so the header and these Rust types read as the same contract rather than two spellings of it"
)]
mod envelope;
mod flags;
mod flat;
mod limits;
mod opcode;
mod primitives;
mod status;
mod validate;
mod value;
mod vtable;

pub use envelope::{vosu_bytes, vosu_message};
pub use flags::VOSU_READ_PLAN_SYNC;
pub use flat::demand::vosu_demand_changed_header;
pub use flat::pointer::vosu_pointer_ref;
pub use flat::publish::{vosu_field_publish_header, vosu_publish_batch_header};
pub use flat::read_plan::{
    vosu_plan_base_flat, vosu_plan_base_kind, vosu_plan_op, vosu_read_error_code,
    vosu_read_plan_result_header, vosu_read_plan_submit_header, vosu_read_result_flat,
    vosu_read_result_kind, vosu_stage_base, vosu_stage_base_kind, vosu_stage_header,
};
pub use limits::{
    VOSU_FRAME_MAX_BYTES, VOSU_PLUGIN_ABI_MAJOR, VOSU_PUBLISH_BATCH_MAX_BYTES,
    VOSU_READ_PLAN_MAX_BYTES, VOSU_STREAM_CHUNK_MAX_BYTES,
};
pub use opcode::vosu_opcode;
pub use primitives::{
    vosu_invocation_id, vosu_plan_id, vosu_resource_id, vosu_session_id, vosu_timer_id,
};
pub use status::vosu_status;
pub use validate::{
    ValidationError, read_flat_header, validate_array_range, validate_bytes_range,
    validate_demand_changed, validate_flags, validate_frame_len, validate_publish_batch,
    validate_publish_field, validate_read_plan_len, validate_read_plan_result,
    validate_read_plan_submit, validate_stream_chunk_len,
};
pub use value::vosu_value_kind;
pub use vtable::{VosuPluginEntryFn, vosu_host_services, vosu_plugin_vtable};
