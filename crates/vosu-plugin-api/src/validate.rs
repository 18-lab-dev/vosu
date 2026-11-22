//! Flat payload validation (`docs/v2/plugin-abi.md` §3, §6.3, §6.6, D4):
//! every `{offset, len}` pair inside a flat header must name a byte range
//! *inside* its own payload buffer, the universal and read-plan size caps,
//! the element alignment each array of structs needs, and the rule that an
//! unrecognized `vosu_message.flags` bit is always rejected.
//!
//! These checks operate on the raw, untrusted payload bytes a plugin or
//! `vosu plugin-host` frame carries — never on a value some other code
//! already trusted — so every arithmetic step here is checked rather than
//! assumed to fit.

use core::mem::{align_of, size_of};
use core::ops::Range;

use crate::flags::VOSU_READ_PLAN_SYNC;
use crate::flat::demand::vosu_demand_changed_header;
use crate::flat::pointer::vosu_pointer_ref;
use crate::flat::publish::{vosu_field_publish_header, vosu_publish_batch_header};
use crate::flat::read_plan::{
    vosu_plan_base_kind, vosu_plan_op, vosu_read_error_code, vosu_read_plan_result_header,
    vosu_read_plan_submit_header, vosu_read_result_flat, vosu_read_result_kind,
    vosu_stage_base_kind, vosu_stage_header,
};
use crate::limits::{VOSU_FRAME_MAX_BYTES, VOSU_READ_PLAN_MAX_BYTES, VOSU_STREAM_CHUNK_MAX_BYTES};
use crate::opcode::vosu_opcode;
use crate::value::vosu_value_kind;

/// Every way a flat payload or its envelope can fail validation
/// (`docs/v2/plugin-abi.md` §5's `VOSU_ERR_INVALID_ARGUMENT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationError {
    /// The encoded message exceeds `VOSU_FRAME_MAX_BYTES`.
    FrameTooLarge,
    /// The encoded read-plan submission or result exceeds
    /// `VOSU_READ_PLAN_MAX_BYTES`.
    ReadPlanTooLarge,
    /// A requested `VOSU_OP_STREAM_READ` length exceeds
    /// `VOSU_STREAM_CHUNK_MAX_BYTES`.
    StreamChunkTooLarge,
    /// The buffer is too short to hold a fixed-size header at all.
    HeaderTooShort,
    /// `offset + len` overflowed, or the struct/array byte size could not
    /// be represented.
    Overflow,
    /// `offset + len` lands outside the payload buffer.
    OutOfBounds,
    /// `offset` does not satisfy the element's required alignment.
    Misaligned,
    /// `vosu_message.flags` set a bit this opcode never recognizes.
    UnrecognizedFlags,
    /// A `kind`/enum-typed `u32` field holds a value outside its enum.
    InvalidEnumValue,
    /// A dependent stage's `ref_stage` does not precede its own index.
    DependentStageNotEarlier,
    /// A `VOSU_OP_PUBLISH_BATCH` entry's `handle` differs from the batch's
    /// own `handle` (D4).
    BatchHandleMismatch,
}

/// Rejects `flags` carrying any bit this opcode does not recognize (D4):
/// only `VOSU_OP_READ_PLAN_SUBMIT` recognizes any bit at all, and that bit
/// is exactly [`VOSU_READ_PLAN_SYNC`].
pub fn validate_flags(opcode: vosu_opcode, flags: u32) -> Result<(), ValidationError> {
    let recognized = if matches!(opcode, vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT) {
        VOSU_READ_PLAN_SYNC
    } else {
        0
    };
    if flags & !recognized != 0 {
        return Err(ValidationError::UnrecognizedFlags);
    }
    Ok(())
}

/// Caps an encoded message at the universal `VOSU_FRAME_MAX_BYTES`;
/// `VOSU_PUBLISH_BATCH_MAX_BYTES` is the same cap under another name (D4),
/// so a `VOSU_OP_PUBLISH_BATCH` payload uses this same function.
pub fn validate_frame_len(encoded_len: usize) -> Result<(), ValidationError> {
    if encoded_len > VOSU_FRAME_MAX_BYTES as usize {
        return Err(ValidationError::FrameTooLarge);
    }
    Ok(())
}

/// Caps an encoded read-plan submission or result at
/// `VOSU_READ_PLAN_MAX_BYTES` (64 KiB), tighter than the universal frame
/// cap (`docs/v2/plugin-abi.md` §6.3).
pub fn validate_read_plan_len(encoded_len: usize) -> Result<(), ValidationError> {
    if encoded_len > VOSU_READ_PLAN_MAX_BYTES as usize {
        return Err(ValidationError::ReadPlanTooLarge);
    }
    Ok(())
}

/// Caps a `VOSU_OP_STREAM_READ` request's `len` (and so its
/// `VOSU_OP_STREAM_CHUNK` reply's `data`) at `VOSU_STREAM_CHUNK_MAX_BYTES`.
pub fn validate_stream_chunk_len(requested_len: u32) -> Result<(), ValidationError> {
    if requested_len > VOSU_STREAM_CHUNK_MAX_BYTES {
        return Err(ValidationError::StreamChunkTooLarge);
    }
    Ok(())
}

/// Validates one `{offset, len}` pair against `buffer_len` and the
/// `align`ment its addressed element requires, returning the byte range it
/// names on success.
fn validate_range(
    buffer_len: usize,
    offset: u32,
    len: u32,
    align: u32,
) -> Result<Range<usize>, ValidationError> {
    if align > 1 && !offset.is_multiple_of(align) {
        return Err(ValidationError::Misaligned);
    }
    let end = offset.checked_add(len).ok_or(ValidationError::Overflow)?;
    if end as usize > buffer_len {
        return Err(ValidationError::OutOfBounds);
    }
    Ok(offset as usize..end as usize)
}

/// Validates a raw byte run: no alignment requirement
/// (`docs/v2/plugin-abi.md` §3 "Alignment").
pub fn validate_bytes_range(
    buffer_len: usize,
    offset: u32,
    len: u32,
) -> Result<Range<usize>, ValidationError> {
    validate_range(buffer_len, offset, len, 1)
}

/// Validates an array of `count` `T`s at `offset`, using `T`'s own
/// `repr(C)` size and alignment as the required element alignment — the
/// same rule `docs/v2/plugin-abi.md` §3 states by name per struct (4 for
/// [`vosu_pointer_ref`]/[`vosu_read_result_flat`], 8 for
/// [`vosu_plan_op`]/[`vosu_stage_header`]/[`vosu_field_publish_header`]).
pub fn validate_array_range<T>(
    buffer_len: usize,
    offset: u32,
    count: u32,
) -> Result<Range<usize>, ValidationError> {
    let elem_size = u32::try_from(size_of::<T>()).map_err(|_| ValidationError::Overflow)?;
    let align = u32::try_from(align_of::<T>()).map_err(|_| ValidationError::Overflow)?;
    let total_len = elem_size
        .checked_mul(count)
        .ok_or(ValidationError::Overflow)?;
    validate_range(buffer_len, offset, total_len, align)
}

/// Reads a `Copy` flat header type out of untrusted bytes without relying
/// on the buffer's own alignment.
pub(crate) fn read_header_at<T: Copy>(buffer: &[u8], offset: usize) -> Result<T, ValidationError> {
    let size = size_of::<T>();
    let end = offset.checked_add(size).ok_or(ValidationError::Overflow)?;
    if end > buffer.len() {
        return Err(ValidationError::HeaderTooShort);
    }
    // SAFETY: `T` is always one of this crate's flat-payload header
    // structs: every field is `u8`/`u32`/`u64`/`i64`, a fixed-size array of
    // those, a nested struct of only such fields, or the `vosu_scalar`
    // union of them — every bit pattern of every field is a valid value,
    // so there is no uninitialized padding this read could depend on.
    // `buffer[offset..end]` was just bounds-checked above, and
    // `read_unaligned` accepts any pointer alignment, which this untrusted
    // buffer does not otherwise guarantee.
    Ok(unsafe { buffer.as_ptr().add(offset).cast::<T>().read_unaligned() })
}

/// Reads the fixed-size flat header at the start of `buffer`.
pub fn read_flat_header<T: Copy>(buffer: &[u8]) -> Result<T, ValidationError> {
    read_header_at(buffer, 0)
}

/// Validates a standalone `VOSU_OP_PUBLISH_FIELD` payload's trailing
/// `ns`/`pointer`/`data` byte ranges and `value_kind` discriminant.
pub fn validate_publish_field(buffer: &[u8]) -> Result<(), ValidationError> {
    validate_frame_len(buffer.len())?;
    let header: vosu_field_publish_header = read_flat_header(buffer)?;
    validate_publish_field_header(buffer.len(), &header)
}

fn validate_publish_field_header(
    buffer_len: usize,
    header: &vosu_field_publish_header,
) -> Result<(), ValidationError> {
    validate_bytes_range(buffer_len, header.ns_offset, header.ns_len)?;
    validate_bytes_range(buffer_len, header.pointer_offset, header.pointer_len)?;
    validate_bytes_range(buffer_len, header.data_offset, header.data_len)?;
    if vosu_value_kind::try_from(header.value_kind).is_err() {
        return Err(ValidationError::InvalidEnumValue);
    }
    Ok(())
}

/// Validates a `VOSU_OP_PUBLISH_BATCH` payload: the entry array's bounds
/// and alignment, then every entry's own trailing data and that its
/// `handle` matches the batch's (D4; overlap-between-entries and
/// uncovered-field checks are host-side semantic rules, not layout
/// validation, and are out of this crate's scope).
pub fn validate_publish_batch(buffer: &[u8]) -> Result<(), ValidationError> {
    validate_frame_len(buffer.len())?;
    let header: vosu_publish_batch_header = read_flat_header(buffer)?;
    let entries = validate_array_range::<vosu_field_publish_header>(
        buffer.len(),
        header.entries_offset,
        header.entry_count,
    )?;
    let elem_size = size_of::<vosu_field_publish_header>();
    for index in 0..header.entry_count as usize {
        let entry: vosu_field_publish_header =
            read_header_at(buffer, entries.start + index * elem_size)?;
        if entry.handle != header.handle {
            return Err(ValidationError::BatchHandleMismatch);
        }
        validate_publish_field_header(buffer.len(), &entry)?;
    }
    Ok(())
}

/// Validates a `VOSU_OP_DEMAND_CHANGED` payload's `selectors` array and
/// every selector's own `ns`/`pointer` trailing bytes.
pub fn validate_demand_changed(buffer: &[u8]) -> Result<(), ValidationError> {
    validate_frame_len(buffer.len())?;
    let header: vosu_demand_changed_header = read_flat_header(buffer)?;
    let selectors = validate_array_range::<vosu_pointer_ref>(
        buffer.len(),
        header.selectors_offset,
        header.selector_count,
    )?;
    let elem_size = size_of::<vosu_pointer_ref>();
    for index in 0..header.selector_count as usize {
        let selector: vosu_pointer_ref =
            read_header_at(buffer, selectors.start + index * elem_size)?;
        validate_bytes_range(buffer.len(), selector.ns_offset, selector.ns_len)?;
        validate_bytes_range(buffer.len(), selector.pointer_offset, selector.pointer_len)?;
    }
    Ok(())
}

/// Validates a `VOSU_OP_READ_PLAN_SUBMIT` payload end to end: the
/// `VOSU_READ_PLAN_MAX_BYTES` cap, the root module name (if
/// `VOSU_BASE_MODULE`), every stage's own base kind and (for a dependent
/// stage) that `ref_stage` precedes it, and every stage's `ops` array
/// bounds (`docs/v2/plugin-abi.md` §6.3).
pub fn validate_read_plan_submit(buffer: &[u8]) -> Result<(), ValidationError> {
    validate_read_plan_len(buffer.len())?;
    let header: vosu_read_plan_submit_header = read_flat_header(buffer)?;

    match vosu_plan_base_kind::try_from(header.root.kind) {
        Ok(vosu_plan_base_kind::VOSU_BASE_MODULE) => {
            validate_bytes_range(
                buffer.len(),
                header.root.module_name_offset,
                header.root.module_name_len,
            )?;
        }
        Ok(vosu_plan_base_kind::VOSU_BASE_ABSOLUTE) => {}
        Err(()) => return Err(ValidationError::InvalidEnumValue),
    }

    let stages = validate_array_range::<vosu_stage_header>(
        buffer.len(),
        header.stages_offset,
        header.stage_count,
    )?;
    let stage_size = size_of::<vosu_stage_header>();
    let op_size = size_of::<vosu_plan_op>();
    for stage_index in 0..header.stage_count as usize {
        let stage: vosu_stage_header =
            read_header_at(buffer, stages.start + stage_index * stage_size)?;
        match vosu_stage_base_kind::try_from(stage.base.kind) {
            Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_DEPENDENT) => {
                if stage.base.ref_stage as usize >= stage_index {
                    return Err(ValidationError::DependentStageNotEarlier);
                }
            }
            Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_ROOT) => {}
            Err(()) => return Err(ValidationError::InvalidEnumValue),
        }
        let ops =
            validate_array_range::<vosu_plan_op>(buffer.len(), stage.ops_offset, stage.op_count)?;
        debug_assert!(ops.len() == stage.op_count as usize * op_size);
    }
    Ok(())
}

/// Validates a `VOSU_OP_READ_PLAN_RESULT` payload: the `VOSU_READ_PLAN_MAX_BYTES`
/// cap, the `results` array bounds, and every result's own `kind`/
/// `error_code` discriminants and `bytes`/`error_message` trailing ranges.
pub fn validate_read_plan_result(buffer: &[u8]) -> Result<(), ValidationError> {
    validate_read_plan_len(buffer.len())?;
    let header: vosu_read_plan_result_header = read_flat_header(buffer)?;
    let results = validate_array_range::<vosu_read_result_flat>(
        buffer.len(),
        header.results_offset,
        header.result_count,
    )?;
    let elem_size = size_of::<vosu_read_result_flat>();
    for index in 0..header.result_count as usize {
        let result: vosu_read_result_flat =
            read_header_at(buffer, results.start + index * elem_size)?;
        let kind = vosu_read_result_kind::try_from(result.kind)
            .map_err(|()| ValidationError::InvalidEnumValue)?;
        if kind == vosu_read_result_kind::VOSU_READ_ERROR {
            vosu_read_error_code::try_from(result.error_code)
                .map_err(|()| ValidationError::InvalidEnumValue)?;
        }
        validate_bytes_range(buffer.len(), result.bytes_offset, result.bytes_len)?;
        validate_bytes_range(
            buffer.len(),
            result.error_message_offset,
            result.error_message_len,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_u8(buf: &mut Vec<u8>, value: u8) {
        buf.push(value);
    }
    fn push_u32(buf: &mut Vec<u8>, value: u32) {
        buf.extend_from_slice(&value.to_le_bytes());
    }
    fn push_u64(buf: &mut Vec<u8>, value: u64) {
        buf.extend_from_slice(&value.to_le_bytes());
    }
    fn push_i64(buf: &mut Vec<u8>, value: i64) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    /// `vosu_field_publish_header`, 48 bytes, field order per
    /// `docs/v2/plugin-abi.md` §3. `Default` gives every field the
    /// "empty/null" value, so a test only names the fields it exercises.
    #[derive(Default)]
    struct FieldPublishBytes {
        handle: u64,
        ns_offset: u32,
        ns_len: u32,
        pointer_offset: u32,
        pointer_len: u32,
        value_kind: u32,
        data_offset: u32,
        data_len: u32,
    }

    fn push_field_publish_header(buf: &mut Vec<u8>, fields: FieldPublishBytes) {
        push_u64(buf, fields.handle);
        push_u32(buf, fields.ns_offset);
        push_u32(buf, fields.ns_len);
        push_u32(buf, fields.pointer_offset);
        push_u32(buf, fields.pointer_len);
        push_u32(buf, fields.value_kind);
        push_u32(buf, 0); // reserved
        push_u64(buf, 0); // scalar
        push_u32(buf, fields.data_offset);
        push_u32(buf, fields.data_len);
    }

    #[test]
    fn publish_field_accepts_well_formed_trailing_data() {
        let mut buf = Vec::new();
        push_field_publish_header(
            &mut buf,
            FieldPublishBytes {
                ns_offset: 48,
                ns_len: 1,
                pointer_offset: 49,
                pointer_len: 2,
                ..Default::default()
            },
        );
        buf.extend_from_slice(b"n"); // ns @48, len 1
        buf.extend_from_slice(b"/p"); // pointer @49, len 2
        assert!(validate_publish_field(&buf).is_ok());
    }

    #[test]
    fn publish_field_rejects_ns_range_past_the_buffer() {
        let mut buf = Vec::new();
        push_field_publish_header(
            &mut buf,
            FieldPublishBytes {
                ns_offset: 48,
                ns_len: 10,
                ..Default::default()
            },
        );
        buf.extend_from_slice(b"n"); // only 1 byte present, ns_len claims 10
        assert_eq!(
            validate_publish_field(&buf),
            Err(ValidationError::OutOfBounds)
        );
    }

    #[test]
    fn publish_field_rejects_unknown_value_kind() {
        let mut buf = Vec::new();
        push_field_publish_header(
            &mut buf,
            FieldPublishBytes {
                value_kind: 99,
                ..Default::default()
            },
        );
        assert_eq!(
            validate_publish_field(&buf),
            Err(ValidationError::InvalidEnumValue)
        );
    }

    #[test]
    fn publish_batch_rejects_entry_handle_mismatch() {
        let mut buf = Vec::new();
        push_u64(&mut buf, 1); // batch handle
        push_u32(&mut buf, 16); // entries_offset
        push_u32(&mut buf, 1); // entry_count
        push_field_publish_header(
            &mut buf,
            FieldPublishBytes {
                handle: 2, // entry handle differs from the batch's
                ..Default::default()
            },
        );
        assert_eq!(
            validate_publish_batch(&buf),
            Err(ValidationError::BatchHandleMismatch)
        );
    }

    #[test]
    fn demand_changed_accepts_empty_selectors() {
        let mut buf = Vec::new();
        push_u64(&mut buf, 1); // handle
        push_u8(&mut buf, 1); // regular
        push_u8(&mut buf, 0); // precise
        buf.extend_from_slice(&[0; 6]); // reserved
        push_u32(&mut buf, 24); // selectors_offset (end of header; 0 entries)
        push_u32(&mut buf, 0); // selector_count
        assert!(validate_demand_changed(&buf).is_ok());
    }

    /// A read plan with one `VOSU_BASE_ABSOLUTE` stage and one op passes;
    /// a dependent stage whose `ref_stage` is not strictly earlier than
    /// its own index is rejected (`docs/v2/plugin-abi.md` §6.3).
    #[test]
    fn read_plan_submit_rejects_a_dependent_stage_referencing_itself() {
        let mut buf = Vec::new();
        push_u64(&mut buf, 1); // plan
        push_u64(&mut buf, 1); // handle
        push_u32(&mut buf, 0); // root.kind = VOSU_BASE_ABSOLUTE
        push_u32(&mut buf, 0); // root.module_name_offset
        push_u32(&mut buf, 0); // root.module_name_len
        push_u32(&mut buf, 0); // root.reserved
        push_u64(&mut buf, 0x1000); // root.address
        push_i64(&mut buf, 0); // root.module_offset
        push_u32(&mut buf, 56); // stages_offset (end of header)
        push_u32(&mut buf, 1); // stage_count
        // stage 0: VOSU_STAGE_BASE_DEPENDENT referencing itself (ref_stage = 0)
        push_u32(&mut buf, 1); // base.kind = VOSU_STAGE_BASE_DEPENDENT
        push_u32(&mut buf, 0); // base.ref_stage (not earlier than stage index 0)
        push_u32(&mut buf, 0); // base.ref_op
        push_u32(&mut buf, 0); // base.reserved
        push_i64(&mut buf, 0); // base.offset
        push_u32(&mut buf, 88); // ops_offset (end of stage array)
        push_u32(&mut buf, 0); // op_count
        assert_eq!(
            validate_read_plan_submit(&buf),
            Err(ValidationError::DependentStageNotEarlier)
        );
    }
}
