//! Owned, decoded/encoded forms of the five flat hot-path opcodes
//! (`docs/v2/plugin-abi.md` §3, §6.3, §6.6, §6.7); `publish` (below) adds
//! the borrowed, allocation-free `VOSU_OP_PUBLISH_FIELD`/`VOSU_OP_PUBLISH_BATCH`
//! decode and the reusable batch writer (D4, S2), split out to keep this
//! file under the maintained-file length limit.
//!
//! `decode_*` validates the raw bytes with [`crate::validate`] first, then
//! copies every trailing byte range into owned `String`/`Vec<u8>` data (and,
//! for a `VOSU_VALUE_CBOR`/`RESOURCE_REF`/`TREE_REF` leaf, decodes the
//! embedded CBOR too) so a caller never holds a borrow into the original
//! frame buffer. `encode_*` lays the same data back out as one contiguous
//! buffer: header, then every array of fixed-size structs this header
//! names, then each element's own trailing variable-length bytes, padding
//! with zero bytes to the next element's required alignment where a
//! variable-length run would otherwise misalign it (`docs/v2/plugin-abi.md`
//! §3 "Alignment").

use core::mem::size_of;

use crate::ResourceRef;
use crate::cbor::{self, CborValue};
use crate::flat::demand::vosu_demand_changed_header;
use crate::flat::pointer::vosu_pointer_ref;
use crate::flat::publish::{vosu_field_publish_header, vosu_publish_batch_header, vosu_scalar};
use crate::flat::read_plan::{
    vosu_plan_base_flat, vosu_plan_base_kind, vosu_plan_op, vosu_read_error_code,
    vosu_read_plan_result_header, vosu_read_plan_submit_header, vosu_read_result_flat,
    vosu_read_result_kind, vosu_stage_base, vosu_stage_base_kind, vosu_stage_header,
};
use crate::message::MessageError;
use crate::validate::{self, read_flat_header};
use crate::value::vosu_value_kind;

mod publish;
pub use publish::{
    PublishBatchRef, PublishBatchWriter, PublishFieldRef, PublishValueRef,
    decode_publish_batch_ref, decode_publish_field_ref,
};

fn read_header_at<T: Copy>(buffer: &[u8], offset: usize) -> Result<T, MessageError> {
    validate::read_header_at(buffer, offset).map_err(MessageError::from)
}

fn owned_str(buffer: &[u8], offset: u32, len: u32) -> Result<String, MessageError> {
    let range = offset as usize..(offset as usize + len as usize);
    String::from_utf8(buffer[range].to_vec())
        .map_err(|err| MessageError::InvalidUtf8(err.utf8_error()))
}

fn owned_bytes(buffer: &[u8], offset: u32, len: u32) -> Vec<u8> {
    buffer[offset as usize..(offset as usize + len as usize)].to_vec()
}

/// A namespace plus an RFC 6901 JSON Pointer below it
/// ([`vosu_pointer_ref`]'s decoded form).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSelector {
    pub ns: String,
    pub pointer: String,
}

/// `vosu_field_publish_header::value_kind` plus its payload, decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum PublishValue {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    /// UTF-8
    String(String),
    /// opaque
    Bytes(Vec<u8>),
    /// a CBOR-encoded subtree replacing the whole value at `pointer`
    Cbor(CborValue),
    ResourceRef(ResourceRef),
    TreeRef(ResourceRef),
}

/// `VOSU_OP_PUBLISH_FIELD`, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct PublishField {
    pub handle: u64,
    pub ns: String,
    pub pointer: String,
    pub value: PublishValue,
}

/// `VOSU_OP_PUBLISH_BATCH`, decoded (D4).
#[derive(Debug, Clone, PartialEq)]
pub struct PublishBatch {
    pub handle: u64,
    pub entries: Vec<PublishField>,
}

/// `VOSU_OP_DEMAND_CHANGED`, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct DemandChanged {
    pub handle: u64,
    pub regular: bool,
    pub precise: bool,
    pub selectors: Vec<FieldSelector>,
}

/// `vosu_plan_base_flat`, decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanBase {
    Absolute(u64),
    Module { name: String, offset: i64 },
}

/// `vosu_stage_base`'s `kind`/`ref_stage`/`ref_op`, decoded; `offset` is
/// kept on [`Stage`] itself, since it applies after resolving either kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageBase {
    Root,
    Dependent { ref_stage: u32, ref_op: u32 },
}

/// `vosu_plan_op`, decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanOp {
    pub offset: i64,
    pub length: u32,
}

/// `vosu_stage_header`, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct Stage {
    pub base: StageBase,
    /// added after resolving either base kind, before this stage's ops apply
    pub base_offset: i64,
    pub ops: Vec<PlanOp>,
}

/// [`Stage`], borrowed: one stage's `ops` as a slice instead of an owned
/// `Vec`, so `encode_submit_into` builds a `VOSU_OP_READ_PLAN_SUBMIT`
/// payload straight from a caller's own `&[PlanOp]` without ever owning a
/// `Stage`/`Vec<Stage>` (D17; `crate::sdk::plan_memory`'s own module doc
/// comment).
#[cfg(feature = "unstable-sync-reads")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageRef<'a> {
    pub base: StageBase,
    pub base_offset: i64,
    pub ops: &'a [PlanOp],
}

/// `VOSU_OP_READ_PLAN_SUBMIT`, decoded. `sync` mirrors
/// `vosu_message.flags`'s `VOSU_READ_PLAN_SYNC` bit (D5), carried outside
/// the flat payload itself.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadPlanSubmit {
    pub plan: u64,
    pub handle: u64,
    pub root: PlanBase,
    pub stages: Vec<Stage>,
    pub sync: bool,
}

/// Mirrors `src/process.rs`'s `MemoryError` subset
/// ([`vosu_read_error_code`]'s decoded form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadErrorCode {
    Backend,
    PermissionDenied,
    ReadFailed,
    AddressOverflow,
}

impl From<vosu_read_error_code> for ReadErrorCode {
    fn from(value: vosu_read_error_code) -> Self {
        match value {
            vosu_read_error_code::VOSU_READ_ERROR_BACKEND => Self::Backend,
            vosu_read_error_code::VOSU_READ_ERROR_PERMISSION_DENIED => Self::PermissionDenied,
            vosu_read_error_code::VOSU_READ_ERROR_READ_FAILED => Self::ReadFailed,
            vosu_read_error_code::VOSU_READ_ERROR_ADDRESS_OVERFLOW => Self::AddressOverflow,
        }
    }
}

impl From<ReadErrorCode> for vosu_read_error_code {
    fn from(value: ReadErrorCode) -> Self {
        match value {
            ReadErrorCode::Backend => Self::VOSU_READ_ERROR_BACKEND,
            ReadErrorCode::PermissionDenied => Self::VOSU_READ_ERROR_PERMISSION_DENIED,
            ReadErrorCode::ReadFailed => Self::VOSU_READ_ERROR_READ_FAILED,
            ReadErrorCode::AddressOverflow => Self::VOSU_READ_ERROR_ADDRESS_OVERFLOW,
        }
    }
}

/// `vosu_read_result_flat`, decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadResult {
    Ok(Vec<u8>),
    NotReady,
    Unsupported,
    Error {
        code: ReadErrorCode,
        message: String,
    },
}

/// `VOSU_OP_READ_PLAN_RESULT`, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadPlanResult {
    pub plan: u64,
    pub results: Vec<ReadResult>,
}

fn decode_value(
    buffer: &[u8],
    value_kind: u32,
    scalar: vosu_scalar,
    data_offset: u32,
    data_len: u32,
) -> Result<PublishValue, MessageError> {
    let kind = vosu_value_kind::try_from(value_kind)
        .map_err(|()| MessageError::Flat(crate::ValidationError::InvalidEnumValue))?;
    Ok(match kind {
        vosu_value_kind::VOSU_VALUE_NULL => PublishValue::Null,
        // SAFETY: `scalar` was just classified by `value_kind`, which
        // names exactly which union member the producer wrote; every
        // other member's bits may be anything, but this read never uses
        // them.
        vosu_value_kind::VOSU_VALUE_BOOL => PublishValue::Bool(unsafe { scalar.b } != 0),
        // SAFETY: see the `VOSU_VALUE_BOOL` arm above.
        vosu_value_kind::VOSU_VALUE_I64 => PublishValue::I64(unsafe { scalar.i64 }),
        // SAFETY: see the `VOSU_VALUE_BOOL` arm above.
        vosu_value_kind::VOSU_VALUE_U64 => PublishValue::U64(unsafe { scalar.u64 }),
        // SAFETY: see the `VOSU_VALUE_BOOL` arm above.
        vosu_value_kind::VOSU_VALUE_F64 => PublishValue::F64(unsafe { scalar.f64 }),
        vosu_value_kind::VOSU_VALUE_STRING => {
            PublishValue::String(owned_str(buffer, data_offset, data_len)?)
        }
        vosu_value_kind::VOSU_VALUE_BYTES => {
            PublishValue::Bytes(owned_bytes(buffer, data_offset, data_len))
        }
        vosu_value_kind::VOSU_VALUE_CBOR => {
            PublishValue::Cbor(cbor::decode(&owned_bytes(buffer, data_offset, data_len))?)
        }
        vosu_value_kind::VOSU_VALUE_RESOURCE_REF => {
            PublishValue::ResourceRef(cbor::decode(&owned_bytes(buffer, data_offset, data_len))?)
        }
        vosu_value_kind::VOSU_VALUE_TREE_REF => {
            PublishValue::TreeRef(cbor::decode(&owned_bytes(buffer, data_offset, data_len))?)
        }
    })
}

fn decode_field_header(
    buffer: &[u8],
    header: &vosu_field_publish_header,
) -> Result<PublishField, MessageError> {
    Ok(PublishField {
        handle: header.handle,
        ns: owned_str(buffer, header.ns_offset, header.ns_len)?,
        pointer: owned_str(buffer, header.pointer_offset, header.pointer_len)?,
        value: decode_value(
            buffer,
            header.value_kind,
            header.scalar,
            header.data_offset,
            header.data_len,
        )?,
    })
}

pub fn decode_publish_field(buffer: &[u8]) -> Result<PublishField, MessageError> {
    validate::validate_publish_field(buffer)?;
    let header: vosu_field_publish_header = read_flat_header(buffer)?;
    decode_field_header(buffer, &header)
}

pub fn decode_publish_batch(buffer: &[u8]) -> Result<PublishBatch, MessageError> {
    validate::validate_publish_batch(buffer)?;
    let header: vosu_publish_batch_header = read_flat_header(buffer)?;
    let elem_size = size_of::<vosu_field_publish_header>();
    let mut entries = Vec::with_capacity(header.entry_count as usize);
    for index in 0..header.entry_count as usize {
        let entry: vosu_field_publish_header =
            read_header_at(buffer, header.entries_offset as usize + index * elem_size)?;
        entries.push(decode_field_header(buffer, &entry)?);
    }
    Ok(PublishBatch {
        handle: header.handle,
        entries,
    })
}

pub fn decode_demand_changed(buffer: &[u8]) -> Result<DemandChanged, MessageError> {
    validate::validate_demand_changed(buffer)?;
    let header: vosu_demand_changed_header = read_flat_header(buffer)?;
    let elem_size = size_of::<vosu_pointer_ref>();
    let mut selectors = Vec::with_capacity(header.selector_count as usize);
    for index in 0..header.selector_count as usize {
        let selector: vosu_pointer_ref =
            read_header_at(buffer, header.selectors_offset as usize + index * elem_size)?;
        selectors.push(FieldSelector {
            ns: owned_str(buffer, selector.ns_offset, selector.ns_len)?,
            pointer: owned_str(buffer, selector.pointer_offset, selector.pointer_len)?,
        });
    }
    Ok(DemandChanged {
        handle: header.handle,
        regular: header.regular != 0,
        precise: header.precise != 0,
        selectors,
    })
}

pub fn decode_read_plan_submit(buffer: &[u8], sync: bool) -> Result<ReadPlanSubmit, MessageError> {
    validate::validate_read_plan_submit(buffer)?;
    let header: vosu_read_plan_submit_header = read_flat_header(buffer)?;
    let root = match vosu_plan_base_kind::try_from(header.root.kind) {
        Ok(vosu_plan_base_kind::VOSU_BASE_ABSOLUTE) => PlanBase::Absolute(header.root.address),
        Ok(vosu_plan_base_kind::VOSU_BASE_MODULE) => PlanBase::Module {
            name: owned_str(
                buffer,
                header.root.module_name_offset,
                header.root.module_name_len,
            )?,
            offset: header.root.module_offset,
        },
        Err(()) => unreachable!("validate_read_plan_submit already rejected an invalid kind"),
    };

    let stage_size = size_of::<vosu_stage_header>();
    let op_size = size_of::<vosu_plan_op>();
    let mut stages = Vec::with_capacity(header.stage_count as usize);
    for stage_index in 0..header.stage_count as usize {
        let stage: vosu_stage_header = read_header_at(
            buffer,
            header.stages_offset as usize + stage_index * stage_size,
        )?;
        let base = match vosu_stage_base_kind::try_from(stage.base.kind) {
            Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_ROOT) => StageBase::Root,
            Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_DEPENDENT) => StageBase::Dependent {
                ref_stage: stage.base.ref_stage,
                ref_op: stage.base.ref_op,
            },
            Err(()) => unreachable!("validate_read_plan_submit already rejected an invalid kind"),
        };
        let mut ops = Vec::with_capacity(stage.op_count as usize);
        for op_index in 0..stage.op_count as usize {
            let op: vosu_plan_op =
                read_header_at(buffer, stage.ops_offset as usize + op_index * op_size)?;
            ops.push(PlanOp {
                offset: op.offset,
                length: op.length,
            });
        }
        stages.push(Stage {
            base,
            base_offset: stage.base.offset,
            ops,
        });
    }

    Ok(ReadPlanSubmit {
        plan: header.plan,
        handle: header.handle,
        root,
        stages,
        sync,
    })
}

pub fn decode_read_plan_result(buffer: &[u8]) -> Result<ReadPlanResult, MessageError> {
    validate::validate_read_plan_result(buffer)?;
    let header: vosu_read_plan_result_header = read_flat_header(buffer)?;
    let elem_size = size_of::<vosu_read_result_flat>();
    let mut results = Vec::with_capacity(header.result_count as usize);
    for index in 0..header.result_count as usize {
        let result: vosu_read_result_flat =
            read_header_at(buffer, header.results_offset as usize + index * elem_size)?;
        let kind = vosu_read_result_kind::try_from(result.kind)
            .unwrap_or_else(|()| unreachable!("validate_read_plan_result already rejected it"));
        results.push(match kind {
            vosu_read_result_kind::VOSU_READ_OK => {
                ReadResult::Ok(owned_bytes(buffer, result.bytes_offset, result.bytes_len))
            }
            vosu_read_result_kind::VOSU_READ_NOT_READY => ReadResult::NotReady,
            vosu_read_result_kind::VOSU_READ_UNSUPPORTED => ReadResult::Unsupported,
            vosu_read_result_kind::VOSU_READ_ERROR => ReadResult::Error {
                code: vosu_read_error_code::try_from(result.error_code)
                    .unwrap_or_else(|()| {
                        unreachable!("validate_read_plan_result already rejected it")
                    })
                    .into(),
                message: owned_str(
                    buffer,
                    result.error_message_offset,
                    result.error_message_len,
                )?,
            },
        });
    }
    Ok(ReadPlanResult {
        plan: header.plan,
        results,
    })
}

/// Pads `buf` with zero bytes until its length is a multiple of `align`.
fn pad_to(buf: &mut Vec<u8>, align: usize) {
    let remainder = buf.len() % align;
    if remainder != 0 {
        buf.resize(buf.len() + (align - remainder), 0);
    }
}

fn push_struct<T: Copy>(buf: &mut Vec<u8>, value: &T) {
    // SAFETY: every flat header struct's fields are plain integers/arrays/
    // nested structs of those, or the `vosu_scalar` union, so every byte
    // position has a valid value for *some* field; reading them as raw
    // bytes is well-defined only because every constructor in this crate
    // also initializes every one of those bytes (`encode_value`'s `Bool`
    // arm writes `vosu_scalar`'s full `u64`, not just its `b` byte, for
    // exactly this reason; see M1 in this crate's conformance history).
    // This mirrors how `read_header_at` writes them back with
    // `read_unaligned`.
    let bytes =
        unsafe { core::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) };
    buf.extend_from_slice(bytes);
}

fn encode_value(value: &PublishValue) -> Result<(u32, vosu_scalar, Vec<u8>), MessageError> {
    Ok(match value {
        PublishValue::Null => (
            vosu_value_kind::VOSU_VALUE_NULL as u32,
            vosu_scalar { u64: 0 },
            Vec::new(),
        ),
        PublishValue::Bool(b) => (
            vosu_value_kind::VOSU_VALUE_BOOL as u32,
            // `u64`, not `b`: a union field only initializes the bytes its
            // own type covers, and `push_struct` reads every byte of this
            // struct, including `scalar`'s other 7 bytes, so a bare `u8`
            // write here would leave them uninitialized (M1). Both ABI
            // targets are little-endian, so `b`'s value still lands at
            // byte 0.
            vosu_scalar { u64: u64::from(*b) },
            Vec::new(),
        ),
        PublishValue::I64(v) => (
            vosu_value_kind::VOSU_VALUE_I64 as u32,
            vosu_scalar { i64: *v },
            Vec::new(),
        ),
        PublishValue::U64(v) => (
            vosu_value_kind::VOSU_VALUE_U64 as u32,
            vosu_scalar { u64: *v },
            Vec::new(),
        ),
        PublishValue::F64(v) => (
            vosu_value_kind::VOSU_VALUE_F64 as u32,
            vosu_scalar { f64: *v },
            Vec::new(),
        ),
        PublishValue::String(s) => (
            vosu_value_kind::VOSU_VALUE_STRING as u32,
            vosu_scalar { u64: 0 },
            s.clone().into_bytes(),
        ),
        PublishValue::Bytes(b) => (
            vosu_value_kind::VOSU_VALUE_BYTES as u32,
            vosu_scalar { u64: 0 },
            b.clone(),
        ),
        PublishValue::Cbor(v) => (
            vosu_value_kind::VOSU_VALUE_CBOR as u32,
            vosu_scalar { u64: 0 },
            cbor::encode(v)?,
        ),
        PublishValue::ResourceRef(v) => (
            vosu_value_kind::VOSU_VALUE_RESOURCE_REF as u32,
            vosu_scalar { u64: 0 },
            cbor::encode(v)?,
        ),
        PublishValue::TreeRef(v) => (
            vosu_value_kind::VOSU_VALUE_TREE_REF as u32,
            vosu_scalar { u64: 0 },
            cbor::encode(v)?,
        ),
    })
}

/// Lays out one `vosu_field_publish_header` at `header_offset` inside `buf`
/// (`buf` must already be `header_offset` bytes long), then appends `ns`,
/// `pointer`, and the value's trailing data after `buf`'s current end,
/// returning the fully-formed header.
fn build_field_header(
    buf: &mut Vec<u8>,
    handle: u64,
    ns: &str,
    pointer: &str,
    value: &PublishValue,
) -> Result<vosu_field_publish_header, MessageError> {
    let (value_kind, scalar, data) = encode_value(value)?;
    let ns_offset = buf.len() as u32;
    buf.extend_from_slice(ns.as_bytes());
    let pointer_offset = buf.len() as u32;
    buf.extend_from_slice(pointer.as_bytes());
    let data_offset = buf.len() as u32;
    buf.extend_from_slice(&data);
    Ok(vosu_field_publish_header {
        handle,
        ns_offset,
        ns_len: ns.len() as u32,
        pointer_offset,
        pointer_len: pointer.len() as u32,
        value_kind,
        reserved: 0,
        scalar,
        data_offset,
        data_len: data.len() as u32,
    })
}

pub fn encode_publish_field(message: &PublishField) -> Result<Vec<u8>, MessageError> {
    let header_size = size_of::<vosu_field_publish_header>();
    let mut trailing = Vec::new();
    let header = build_field_header(
        &mut trailing,
        message.handle,
        &message.ns,
        &message.pointer,
        &message.value,
    )?;
    let mut buf = Vec::with_capacity(header_size + trailing.len());
    push_struct(&mut buf, &rebase_field_header(&header, header_size as u32));
    buf.extend_from_slice(&trailing);
    Ok(buf)
}

fn rebase_field_header(header: &vosu_field_publish_header, base: u32) -> vosu_field_publish_header {
    vosu_field_publish_header {
        ns_offset: header.ns_offset + base,
        pointer_offset: header.pointer_offset + base,
        data_offset: header.data_offset + base,
        ..*header
    }
}

pub fn encode_publish_batch(message: &PublishBatch) -> Result<Vec<u8>, MessageError> {
    let batch_header_size = size_of::<vosu_publish_batch_header>();
    let entry_size = size_of::<vosu_field_publish_header>();
    let entries_offset = batch_header_size as u32;
    let entries_array_end = entries_offset + entry_size as u32 * message.entries.len() as u32;

    let mut trailing = Vec::new();
    let mut entry_headers = Vec::with_capacity(message.entries.len());
    for entry in &message.entries {
        let mut entry_trailing = Vec::new();
        let header = build_field_header(
            &mut entry_trailing,
            entry.handle,
            &entry.ns,
            &entry.pointer,
            &entry.value,
        )?;
        let base = entries_array_end + trailing.len() as u32;
        entry_headers.push(rebase_field_header(&header, base));
        trailing.extend_from_slice(&entry_trailing);
    }

    let mut buf = Vec::with_capacity(entries_array_end as usize + trailing.len());
    push_struct(
        &mut buf,
        &vosu_publish_batch_header {
            handle: message.handle,
            entries_offset,
            entry_count: message.entries.len() as u32,
        },
    );
    for entry_header in &entry_headers {
        push_struct(&mut buf, entry_header);
    }
    buf.extend_from_slice(&trailing);
    Ok(buf)
}

pub fn encode_demand_changed(message: &DemandChanged) -> Result<Vec<u8>, MessageError> {
    let header_size = size_of::<vosu_demand_changed_header>();
    let elem_size = size_of::<vosu_pointer_ref>();
    let selectors_offset = header_size as u32;
    let selectors_array_end = selectors_offset + elem_size as u32 * message.selectors.len() as u32;

    let mut trailing = Vec::new();
    let mut selector_structs = Vec::with_capacity(message.selectors.len());
    for selector in &message.selectors {
        let ns_offset = selectors_array_end + trailing.len() as u32;
        trailing.extend_from_slice(selector.ns.as_bytes());
        let pointer_offset = selectors_array_end + trailing.len() as u32;
        trailing.extend_from_slice(selector.pointer.as_bytes());
        selector_structs.push(vosu_pointer_ref {
            ns_offset,
            ns_len: selector.ns.len() as u32,
            pointer_offset,
            pointer_len: selector.pointer.len() as u32,
        });
    }

    let mut buf = Vec::with_capacity(selectors_array_end as usize + trailing.len());
    push_struct(
        &mut buf,
        &vosu_demand_changed_header {
            handle: message.handle,
            regular: u8::from(message.regular),
            precise: u8::from(message.precise),
            reserved: [0; 6],
            selectors_offset,
            selector_count: message.selectors.len() as u32,
        },
    );
    for selector in &selector_structs {
        push_struct(&mut buf, selector);
    }
    buf.extend_from_slice(&trailing);
    Ok(buf)
}

pub fn encode_read_plan_submit(message: &ReadPlanSubmit) -> Result<Vec<u8>, MessageError> {
    let header_size = size_of::<vosu_read_plan_submit_header>();
    let mut buf = vec![0u8; header_size];

    let root = match &message.root {
        PlanBase::Absolute(address) => vosu_plan_base_flat {
            kind: vosu_plan_base_kind::VOSU_BASE_ABSOLUTE as u32,
            module_name_offset: 0,
            module_name_len: 0,
            reserved: 0,
            address: *address,
            module_offset: 0,
        },
        PlanBase::Module { name, offset } => {
            let module_name_offset = buf.len() as u32;
            buf.extend_from_slice(name.as_bytes());
            vosu_plan_base_flat {
                kind: vosu_plan_base_kind::VOSU_BASE_MODULE as u32,
                module_name_offset,
                module_name_len: name.len() as u32,
                reserved: 0,
                address: 0,
                module_offset: *offset,
            }
        }
    };

    pad_to(&mut buf, align_of_val_stage());
    let stages_offset = buf.len() as u32;
    let stage_size = size_of::<vosu_stage_header>();
    buf.resize(buf.len() + stage_size * message.stages.len(), 0);

    let mut ops_trailing = Vec::new();
    let mut stage_structs = Vec::with_capacity(message.stages.len());
    for stage in &message.stages {
        pad_to(&mut ops_trailing, align_of_val_op());
        let ops_offset =
            stages_offset + (stage_size * message.stages.len()) as u32 + ops_trailing.len() as u32;
        for op in &stage.ops {
            push_struct(
                &mut ops_trailing,
                &vosu_plan_op {
                    offset: op.offset,
                    length: op.length,
                    reserved: 0,
                },
            );
        }
        let (kind, ref_stage, ref_op) = match stage.base {
            StageBase::Root => (vosu_stage_base_kind::VOSU_STAGE_BASE_ROOT as u32, 0, 0),
            StageBase::Dependent { ref_stage, ref_op } => (
                vosu_stage_base_kind::VOSU_STAGE_BASE_DEPENDENT as u32,
                ref_stage,
                ref_op,
            ),
        };
        stage_structs.push(vosu_stage_header {
            base: vosu_stage_base {
                kind,
                ref_stage,
                ref_op,
                reserved: 0,
                offset: stage.base_offset,
            },
            ops_offset,
            op_count: stage.ops.len() as u32,
        });
    }

    let stages_region =
        &mut buf[stages_offset as usize..stages_offset as usize + stage_size * stage_structs.len()];
    let mut cursor = stages_region;
    for stage_header in &stage_structs {
        // SAFETY: a `repr(C)` flat header struct of plain integers/arrays/
        // unions has no padding this byte read could expose uninitialized
        // memory from; `stage_size` is exactly `size_of::<vosu_stage_header>()`.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                (stage_header as *const vosu_stage_header).cast::<u8>(),
                stage_size,
            )
        };
        cursor[..stage_size].copy_from_slice(bytes);
        cursor = &mut cursor[stage_size..];
    }
    buf.extend_from_slice(&ops_trailing);

    let header = vosu_read_plan_submit_header {
        plan: message.plan,
        handle: message.handle,
        root,
        stages_offset,
        stage_count: message.stages.len() as u32,
    };
    // SAFETY: `vosu_read_plan_submit_header` is a `repr(C)` flat header
    // of plain integers/structs with no padding this byte read could
    // expose uninitialized memory from; `header_size` is exactly
    // `size_of::<vosu_read_plan_submit_header>()`.
    let header_bytes = unsafe {
        core::slice::from_raw_parts(
            (&header as *const vosu_read_plan_submit_header).cast::<u8>(),
            header_size,
        )
    };
    buf[..header_size].copy_from_slice(header_bytes);
    Ok(buf)
}

/// Writes `plan`/`handle`/`root`/`stages` straight into `buf` as a
/// `VOSU_OP_READ_PLAN_SUBMIT` payload (this type's own layout, matching
/// [`encode_read_plan_submit`]), `buf.clear()`ed first but keeping its
/// existing capacity: a caller that reuses the same `buf` across repeated
/// submissions of a same-shaped plan allocates nothing once `buf`'s
/// capacity has grown to fit. Unlike [`encode_read_plan_submit`], there is
/// no owned `ReadPlanSubmit`/`Stage` intermediate and, since each stage's
/// own `ops_offset` is computed and written in place as soon as that
/// stage's own `ops` are appended, no `ops_trailing` scratch buffer either
/// — every byte `buf` grows to is already final (D17;
/// `crate::sdk::plan_memory::SyncReadBuffers`'s own doc comment).
#[cfg(feature = "unstable-sync-reads")]
pub fn encode_submit_into(
    buf: &mut Vec<u8>,
    plan: u64,
    handle: u64,
    root: &PlanBase,
    stages: &[StageRef<'_>],
) {
    buf.clear();
    let header_size = size_of::<vosu_read_plan_submit_header>();
    buf.resize(header_size, 0);

    let root_flat = match root {
        PlanBase::Absolute(address) => vosu_plan_base_flat {
            kind: vosu_plan_base_kind::VOSU_BASE_ABSOLUTE as u32,
            module_name_offset: 0,
            module_name_len: 0,
            reserved: 0,
            address: *address,
            module_offset: 0,
        },
        PlanBase::Module { name, offset } => {
            let module_name_offset = buf.len() as u32;
            buf.extend_from_slice(name.as_bytes());
            vosu_plan_base_flat {
                kind: vosu_plan_base_kind::VOSU_BASE_MODULE as u32,
                module_name_offset,
                module_name_len: name.len() as u32,
                reserved: 0,
                address: 0,
                module_offset: *offset,
            }
        }
    };

    pad_to(buf, align_of_val_stage());
    let stages_offset = buf.len() as u32;
    let stage_size = size_of::<vosu_stage_header>();
    buf.resize(buf.len() + stage_size * stages.len(), 0);

    for (index, stage) in stages.iter().enumerate() {
        pad_to(buf, align_of_val_op());
        let ops_offset = buf.len() as u32;
        for op in stage.ops {
            push_struct(
                buf,
                &vosu_plan_op {
                    offset: op.offset,
                    length: op.length,
                    reserved: 0,
                },
            );
        }
        let (kind, ref_stage, ref_op) = match stage.base {
            StageBase::Root => (vosu_stage_base_kind::VOSU_STAGE_BASE_ROOT as u32, 0, 0),
            StageBase::Dependent { ref_stage, ref_op } => (
                vosu_stage_base_kind::VOSU_STAGE_BASE_DEPENDENT as u32,
                ref_stage,
                ref_op,
            ),
        };
        let stage_header = vosu_stage_header {
            base: vosu_stage_base {
                kind,
                ref_stage,
                ref_op,
                reserved: 0,
                offset: stage.base_offset,
            },
            ops_offset,
            op_count: stage.ops.len() as u32,
        };
        let region_start = stages_offset as usize + index * stage_size;
        // SAFETY: a `repr(C)` flat header struct of plain integers/arrays/
        // unions has no padding this byte read could expose uninitialized
        // memory from; `stage_size` is exactly `size_of::<vosu_stage_header>()`,
        // and `region_start..region_start + stage_size` was reserved by
        // the `buf.resize` above.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                (&stage_header as *const vosu_stage_header).cast::<u8>(),
                stage_size,
            )
        };
        buf[region_start..region_start + stage_size].copy_from_slice(bytes);
    }

    let header = vosu_read_plan_submit_header {
        plan,
        handle,
        root: root_flat,
        stages_offset,
        stage_count: stages.len() as u32,
    };
    // SAFETY: see `encode_read_plan_submit`'s identical header byte copy.
    let header_bytes = unsafe {
        core::slice::from_raw_parts(
            (&header as *const vosu_read_plan_submit_header).cast::<u8>(),
            header_size,
        )
    };
    buf[..header_size].copy_from_slice(header_bytes);
}

fn align_of_val_stage() -> usize {
    core::mem::align_of::<vosu_stage_header>()
}
fn align_of_val_op() -> usize {
    core::mem::align_of::<vosu_plan_op>()
}

pub fn encode_read_plan_result(message: &ReadPlanResult) -> Result<Vec<u8>, MessageError> {
    let header_size = size_of::<vosu_read_plan_result_header>();
    let elem_size = size_of::<vosu_read_result_flat>();
    let results_offset = header_size as u32;
    let results_array_end = results_offset + elem_size as u32 * message.results.len() as u32;

    let mut trailing = Vec::new();
    let mut result_structs = Vec::with_capacity(message.results.len());
    for result in &message.results {
        let (kind, bytes, error_code, message_text) = match result {
            ReadResult::Ok(data) => (
                vosu_read_result_kind::VOSU_READ_OK,
                data.clone(),
                0u32,
                String::new(),
            ),
            ReadResult::NotReady => (
                vosu_read_result_kind::VOSU_READ_NOT_READY,
                Vec::new(),
                0,
                String::new(),
            ),
            ReadResult::Unsupported => (
                vosu_read_result_kind::VOSU_READ_UNSUPPORTED,
                Vec::new(),
                0,
                String::new(),
            ),
            ReadResult::Error { code, message } => (
                vosu_read_result_kind::VOSU_READ_ERROR,
                Vec::new(),
                vosu_read_error_code::from(*code) as u32,
                message.clone(),
            ),
        };
        let bytes_offset = results_array_end + trailing.len() as u32;
        trailing.extend_from_slice(&bytes);
        let error_message_offset = results_array_end + trailing.len() as u32;
        trailing.extend_from_slice(message_text.as_bytes());
        result_structs.push(vosu_read_result_flat {
            kind: kind as u32,
            bytes_offset,
            bytes_len: bytes.len() as u32,
            error_code,
            error_message_offset,
            error_message_len: message_text.len() as u32,
        });
    }

    let mut buf = Vec::with_capacity(results_array_end as usize + trailing.len());
    push_struct(
        &mut buf,
        &vosu_read_plan_result_header {
            plan: message.plan,
            results_offset,
            result_count: message.results.len() as u32,
        },
    );
    for result in &result_structs {
        push_struct(&mut buf, result);
    }
    buf.extend_from_slice(&trailing);
    Ok(buf)
}
