//! Memory read plans: `VOSU_OP_READ_PLAN_SUBMIT` / `VOSU_OP_READ_PLAN_RESULT`
//! flat payloads (`include/vosu_plugin.h:321-446`).
//!
//! A plan is one base address plus an ordered list of stages. Stage 0's
//! base is the plan's own `root`. A later stage's base may instead be
//! "dependent": the pointer-width value read by an earlier stage's
//! identified op, plus an additional offset, becomes that stage's base.
//! Each stage batches independent `{offset, length}` ops relative to its
//! own base (`docs/v2/plugin-abi.md` §6.3).

/// The plan's root base kind (`include/vosu_plugin.h:336-339`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum vosu_plan_base_kind {
    VOSU_BASE_ABSOLUTE = 0,
    VOSU_BASE_MODULE = 1,
}

/// The plan's root base: either a previously obtained absolute
/// target-process address, or a module name plus a byte offset from that
/// module's base, resolved by the host at read time. 32 bytes
/// (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_plan_base_flat {
    /// a [`vosu_plan_base_kind`] value
    pub kind: u32,
    /// `VOSU_BASE_MODULE`: module file name, e.g. "osu!.exe"
    pub module_name_offset: u32,
    pub module_name_len: u32,
    /// keeps `address`/`module_offset` 8-byte aligned; always 0
    pub reserved: u32,
    /// `VOSU_BASE_ABSOLUTE`
    pub address: u64,
    /// `VOSU_BASE_MODULE`
    pub module_offset: i64,
}

/// One independent `{offset, length}` read relative to a stage's resolved
/// base. 16 bytes (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_plan_op {
    /// relative to the stage's resolved base
    pub offset: i64,
    /// bytes to read at base + offset
    pub length: u32,
    /// always 0
    pub reserved: u32,
}

/// Whether a stage's base is the plan's own root, or read from an earlier
/// stage's op (`include/vosu_plugin.h:360-363`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum vosu_stage_base_kind {
    /// this stage's base is the plan's `root`
    VOSU_STAGE_BASE_ROOT = 0,
    /// this stage's base is read from an earlier stage's op
    VOSU_STAGE_BASE_DEPENDENT = 1,
}

/// 24 bytes (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_stage_base {
    /// a [`vosu_stage_base_kind`] value
    pub kind: u32,
    /// `VOSU_STAGE_BASE_DEPENDENT`: index of the earlier stage (< this
    /// stage's own index)
    pub ref_stage: u32,
    /// `VOSU_STAGE_BASE_DEPENDENT`: op index within that stage; its read
    /// bytes, interpreted as a pointer-width value per the session's
    /// bitness, become the resolved address
    pub ref_op: u32,
    /// keeps `offset` 8-byte aligned; always 0
    pub reserved: u32,
    /// added after resolving either base kind, before this stage's ops
    /// apply
    pub offset: i64,
}

/// 32 bytes, `base` embedded at offset 0 (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_stage_header {
    pub base: vosu_stage_base,
    /// `vosu_plan_op[op_count]`
    pub ops_offset: u32,
    pub op_count: u32,
}

/// `VOSU_OP_READ_PLAN_SUBMIT`: flat payload. One submission batches every
/// stage this plugin needs into a single message; the host executes the
/// whole dependent chain and answers with exactly one
/// `VOSU_OP_READ_PLAN_RESULT`. 56 bytes (`docs/v2/plugin-abi.md` §3).
///
/// `vosu_message.flags` on this opcode may set `VOSU_READ_PLAN_SYNC`
/// ([`crate::VOSU_READ_PLAN_SYNC`], D5).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_read_plan_submit_header {
    /// caller-assigned; never reused for another plan within this plugin
    /// instance's lifetime
    pub plan: u64,
    pub handle: u64,
    pub root: vosu_plan_base_flat,
    /// `vosu_stage_header[stage_count]`
    pub stages_offset: u32,
    pub stage_count: u32,
}

/// Mirrors the subset of `src/process.rs`'s `MemoryError` that can occur at
/// a raw, fixed-length read plan (`include/vosu_plugin.h:412-420`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum vosu_read_error_code {
    /// `MemoryError::Backend`
    VOSU_READ_ERROR_BACKEND = 1,
    /// `MemoryError::PermissionDenied`
    VOSU_READ_ERROR_PERMISSION_DENIED = 2,
    /// `MemoryError::Read`
    VOSU_READ_ERROR_READ_FAILED = 3,
    /// `MemoryError::AddressOverflow`
    VOSU_READ_ERROR_ADDRESS_OVERFLOW = 4,
}

/// Mirrors `src/process.rs`'s `ReadStatus` vocabulary
/// (`include/vosu_plugin.h:422-427`).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum vosu_read_result_kind {
    VOSU_READ_OK = 0,
    /// mirrors `src/process.rs` `ReadStatus::NotReady`
    VOSU_READ_NOT_READY = 1,
    VOSU_READ_UNSUPPORTED = 2,
    VOSU_READ_ERROR = 3,
}

/// One per-op read outcome. 24 bytes (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_read_result_flat {
    /// a [`vosu_read_result_kind`] value
    pub kind: u32,
    /// `VOSU_READ_OK`: offset of exactly the op's `length` bytes
    pub bytes_offset: u32,
    pub bytes_len: u32,
    /// `VOSU_READ_ERROR`: a [`vosu_read_error_code`] value
    pub error_code: u32,
    pub error_message_offset: u32,
    pub error_message_len: u32,
}

/// `VOSU_OP_READ_PLAN_RESULT`: flat payload. `results` is flat, in the same
/// (stage, op) traversal order as the matching submission. 16 bytes
/// (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_read_plan_result_header {
    pub plan: u64,
    /// `vosu_read_result_flat[result_count]`
    pub results_offset: u32,
    pub result_count: u32,
}
