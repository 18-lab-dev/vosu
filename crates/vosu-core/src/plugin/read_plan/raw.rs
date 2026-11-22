//! The zero-allocation, raw-buffer read-plan executor
//! ([`execute_into`]/[`execute_bytes`]): split out of `read_plan.rs` so
//! that module stays under the maintained-file line limit
//! (`docs/architecture/source-map.md`).
//!
//! Operates directly on an already-validated `VOSU_OP_READ_PLAN_SUBMIT`
//! buffer — no owned `ReadPlanSubmit`/`Stage`/`PlanOp`/`String`
//! intermediate form — and writes the matching `VOSU_OP_READ_PLAN_RESULT`
//! straight into a caller-provided, reusable `Vec<u8>`. See
//! `super::read_plan`'s module doc comment for the two-entry-point design.

use super::*;

/// Reads a flat header struct out of an already-validated buffer at
/// `offset`. Every call site in this module only uses offsets
/// `vosu_plugin_api::validate_read_plan_submit`/`validate_read_plan_ref_op_width`
/// (or, for a result entry this same function already wrote, this
/// module's own bounded writes) have already bounds-checked.
fn read_struct_at<T: Copy>(buffer: &[u8], offset: usize) -> T {
    debug_assert!(offset + size_of::<T>() <= buffer.len());
    // SAFETY: `T` is always one of `vosu_plugin_api`'s `repr(C)` flat
    // header structs (plain integers/arrays, no uninitialized padding),
    // and `offset` names a range this module's own validation or prior
    // writes already guarantee lies inside `buffer`.
    unsafe { buffer.as_ptr().add(offset).cast::<T>().read_unaligned() }
}

fn write_struct_at<T: Copy>(buffer: &mut [u8], offset: usize, value: &T) {
    let size = size_of::<T>();
    debug_assert!(offset + size <= buffer.len());
    // SAFETY: writes exactly `size_of::<T>()` bytes at a range the caller
    // has already sized `buffer` to include; `T` is a `repr(C)` struct of
    // plain integers, so every byte position is a valid write target.
    unsafe {
        core::ptr::copy_nonoverlapping(
            std::ptr::from_ref(value).cast::<u8>(),
            buffer.as_mut_ptr().add(offset),
            size,
        );
    }
}

/// A base-resolution failure distributed, message-less, to every op a
/// blocked stage covers (see [`strip_message`]'s doc comment for why).
/// `execute_into` never needs `Unsupported`: nothing on the raw read path
/// originates it (only the plugin-side decoders above this boundary do).
#[derive(Debug, Clone, Copy)]
enum RawFailure {
    NotReady,
    Error(ReadErrorCode),
}

fn add_signed(base: u64, offset: i64) -> Option<u64> {
    u64::try_from(i128::from(base) + i128::from(offset)).ok()
}

fn checked_add_signed_raw(base: u64, offset: i64) -> Result<u64, RawFailure> {
    add_signed(base, offset).ok_or(RawFailure::Error(ReadErrorCode::AddressOverflow))
}

fn module_name_str<'a>(
    payload: &'a [u8],
    root: &vosu_plan_base_flat,
) -> Result<&'a str, std::str::Utf8Error> {
    let start = root.module_name_offset as usize;
    let end = start + root.module_name_len as usize;
    std::str::from_utf8(&payload[start..end])
}

fn resolve_root_raw(
    payload: &[u8],
    root: &vosu_plan_base_flat,
    modules: &dyn ModuleResolver,
) -> Result<u64, RawFailure> {
    match vosu_plan_base_kind::try_from(root.kind) {
        Ok(vosu_plan_base_kind::VOSU_BASE_ABSOLUTE) => Ok(root.address),
        Ok(vosu_plan_base_kind::VOSU_BASE_MODULE) => {
            let name = module_name_str(payload, root)
                .expect("execute_into validates the root module name's UTF-8 before this runs");
            let base = modules.resolve(name).ok_or(RawFailure::NotReady)?;
            checked_add_signed_raw(base, root.module_offset)
        }
        Err(()) => unreachable!("validate_read_plan_submit already rejected an invalid root kind"),
    }
}

/// Sum of every earlier stage's `op_count`, i.e. the flat result index
/// `target_stage`'s own first op lands at.
fn stage_start_index(payload: &[u8], stages_offset: u32, target_stage: u32) -> u32 {
    let stage_size = size_of::<vosu_stage_header>();
    (0..target_stage)
        .map(|stage_index| {
            let stage: vosu_stage_header = read_struct_at(
                payload,
                stages_offset as usize + stage_index as usize * stage_size,
            );
            stage.op_count
        })
        .sum()
}

fn resolve_stage_base_raw(
    payload: &[u8],
    stages_offset: u32,
    stage: &vosu_stage_header,
    out: &[u8],
    root_base: &Result<u64, RawFailure>,
    bitness: ProcessBitness,
) -> Result<u64, RawFailure> {
    let unresolved = match vosu_stage_base_kind::try_from(stage.base.kind) {
        Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_ROOT) => (*root_base)?,
        Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_DEPENDENT) => {
            let ref_index =
                stage_start_index(payload, stages_offset, stage.base.ref_stage) + stage.base.ref_op;
            let entry: vosu_read_result_flat = read_struct_at(
                out,
                RESULT_HEADER_SIZE + ref_index as usize * RESULT_ENTRY_SIZE,
            );
            match vosu_read_result_kind::try_from(entry.kind) {
                Ok(vosu_read_result_kind::VOSU_READ_OK) => {
                    let bytes = &out[entry.bytes_offset as usize..][..entry.bytes_len as usize];
                    bytes_to_pointer(bytes, bitness)
                }
                Ok(vosu_read_result_kind::VOSU_READ_NOT_READY) => return Err(RawFailure::NotReady),
                Ok(vosu_read_result_kind::VOSU_READ_ERROR) => {
                    let code =
                        vosu_read_error_code::try_from(entry.error_code).unwrap_or_else(|()| {
                            unreachable!("execute_into only writes a valid error_code")
                        });
                    return Err(RawFailure::Error(code.into()));
                }
                Ok(vosu_read_result_kind::VOSU_READ_UNSUPPORTED) | Err(()) => {
                    unreachable!(
                        "execute_into never writes VOSU_READ_UNSUPPORTED or an invalid kind"
                    )
                }
            }
        }
        Err(()) => {
            unreachable!("validate_read_plan_submit already rejected an invalid stage base kind")
        }
    };
    checked_add_signed_raw(unresolved, stage.base.offset)
}

fn ok_entry(bytes_offset: u32, bytes_len: u32) -> vosu_read_result_flat {
    vosu_read_result_flat {
        kind: vosu_read_result_kind::VOSU_READ_OK as u32,
        bytes_offset,
        bytes_len,
        error_code: 0,
        error_message_offset: 0,
        error_message_len: 0,
    }
}

fn not_ready_entry() -> vosu_read_result_flat {
    vosu_read_result_flat {
        kind: vosu_read_result_kind::VOSU_READ_NOT_READY as u32,
        bytes_offset: 0,
        bytes_len: 0,
        error_code: 0,
        error_message_offset: 0,
        error_message_len: 0,
    }
}

fn error_entry(
    code: ReadErrorCode,
    message_offset: u32,
    message_len: u32,
) -> vosu_read_result_flat {
    vosu_read_result_flat {
        kind: vosu_read_result_kind::VOSU_READ_ERROR as u32,
        bytes_offset: 0,
        bytes_len: 0,
        error_code: vosu_read_error_code::from(code) as u32,
        error_message_offset: message_offset,
        error_message_len: message_len,
    }
}

fn write_entry(out: &mut [u8], global_index: usize, entry: &vosu_read_result_flat) {
    write_struct_at(
        out,
        RESULT_HEADER_SIZE + global_index * RESULT_ENTRY_SIZE,
        entry,
    );
}

fn append_message(out: &mut Vec<u8>, message: &str) -> (u32, u32) {
    let offset = out.len() as u32;
    out.extend_from_slice(message.as_bytes());
    (offset, message.len() as u32)
}

fn write_op_error_entry(
    out: &mut Vec<u8>,
    global_index: usize,
    code: ReadErrorCode,
    message: String,
    budget: &mut ErrorBudget,
) {
    let message = budget.charge_message(message);
    let (offset, len) = append_message(out, &message);
    write_entry(out, global_index, &error_entry(code, offset, len));
}

/// Resolves one op's address relative to `base`, as a single (op-level,
/// message-bearing on failure) step shared by the single- and multi-op
/// stage writers.
fn resolve_op_address(base: u64, offset: i64) -> Option<usize> {
    add_signed(base, offset).and_then(|address| usize::try_from(address).ok())
}

fn op_overflow_message(base: u64, offset: i64) -> String {
    format!("address arithmetic overflow: {base:#x} + {offset:#x}")
}

fn process_single_op_raw(
    payload: &[u8],
    stage: &vosu_stage_header,
    base: u64,
    memory: &dyn ProcessMemory,
    global_index: usize,
    out: &mut Vec<u8>,
    budget: &mut ErrorBudget,
) {
    let op: vosu_plan_op = read_struct_at(payload, stage.ops_offset as usize);
    let Some(address) = resolve_op_address(base, op.offset) else {
        budget.recover(op.length);
        write_op_error_entry(
            out,
            global_index,
            ReadErrorCode::AddressOverflow,
            op_overflow_message(base, op.offset),
            budget,
        );
        return;
    };

    let region_start = out.len();
    out.resize(region_start + op.length as usize, 0);
    match memory.read_exact(
        address,
        &mut out[region_start..region_start + op.length as usize],
    ) {
        Ok(()) => write_entry(out, global_index, &ok_entry(region_start as u32, op.length)),
        Err(error) => {
            out.truncate(region_start);
            budget.recover(op.length);
            write_op_error_entry(
                out,
                global_index,
                error_code_for(&error),
                error.to_string(),
                budget,
            );
        }
    }
}

fn process_multi_op_raw(
    payload: &[u8],
    stage: &vosu_stage_header,
    base: u64,
    memory: &dyn ProcessMemory,
    global_index_start: usize,
    out: &mut Vec<u8>,
    budget: &mut ErrorBudget,
) {
    let op_size = size_of::<vosu_plan_op>();
    let mut participants: Vec<(usize, usize, usize, u32)> = Vec::new();

    for op_index in 0..stage.op_count as usize {
        let op: vosu_plan_op =
            read_struct_at(payload, stage.ops_offset as usize + op_index * op_size);
        match resolve_op_address(base, op.offset) {
            Some(address) => {
                let out_offset = out.len();
                out.resize(out_offset + op.length as usize, 0);
                participants.push((op_index, address, out_offset, op.length));
            }
            None => {
                budget.recover(op.length);
                write_op_error_entry(
                    out,
                    global_index_start + op_index,
                    ReadErrorCode::AddressOverflow,
                    op_overflow_message(base, op.offset),
                    budget,
                );
            }
        }
    }

    let Some(&(_, _, region_start, _)) = participants.first() else {
        return;
    };

    let mut remaining: &mut [u8] = &mut out[region_start..];
    let mut requests: Vec<ReadRequest<'_>> = Vec::with_capacity(participants.len());
    let mut cursor = region_start;
    for &(_, address, out_offset, length) in &participants {
        let gap = out_offset - cursor;
        let (_skip, rest) = remaining.split_at_mut(gap);
        let (slice, rest2) = rest.split_at_mut(length as usize);
        requests.push(ReadRequest::new(address, slice));
        remaining = rest2;
        cursor = out_offset + length as usize;
    }

    let batch_ok = memory.read_many(&mut requests).is_ok();
    drop(requests);

    if batch_ok {
        for &(op_index, _, out_offset, length) in &participants {
            write_entry(
                out,
                global_index_start + op_index,
                &ok_entry(out_offset as u32, length),
            );
        }
    } else {
        for &(op_index, address, out_offset, length) in &participants {
            let region = &mut out[out_offset..out_offset + length as usize];
            match memory.read_exact(address, region) {
                Ok(()) => write_entry(
                    out,
                    global_index_start + op_index,
                    &ok_entry(out_offset as u32, length),
                ),
                Err(error) => {
                    // Unlike the single-op path (which truncates its
                    // reservation away on failure), this op's `length`
                    // data bytes stay reserved-but-unused in `out`
                    // (reclaiming a non-tail region mid-buffer would need
                    // a compaction pass); recovering them into the error
                    // budget here would let messages grow past the
                    // validated worst-case cap.
                    write_op_error_entry(
                        out,
                        global_index_start + op_index,
                        error_code_for(&error),
                        error.to_string(),
                        budget,
                    );
                }
            }
        }
    }
}

fn process_stage_raw(
    payload: &[u8],
    stage: &vosu_stage_header,
    stage_base: Result<u64, RawFailure>,
    memory: &dyn ProcessMemory,
    global_index_start: usize,
    out: &mut Vec<u8>,
    budget: &mut ErrorBudget,
) {
    let op_size = size_of::<vosu_plan_op>();
    let base = match stage_base {
        Err(failure) => {
            for op_index in 0..stage.op_count as usize {
                let op: vosu_plan_op =
                    read_struct_at(payload, stage.ops_offset as usize + op_index * op_size);
                budget.recover(op.length);
                let entry = match failure {
                    RawFailure::NotReady => not_ready_entry(),
                    RawFailure::Error(code) => error_entry(code, 0, 0),
                };
                write_entry(out, global_index_start + op_index, &entry);
            }
            return;
        }
        Ok(base) => base,
    };

    match stage.op_count {
        0 => {}
        1 => process_single_op_raw(
            payload,
            stage,
            base,
            memory,
            global_index_start,
            out,
            budget,
        ),
        _ => process_multi_op_raw(
            payload,
            stage,
            base,
            memory,
            global_index_start,
            out,
            budget,
        ),
    }
}

/// Byte-oriented, zero-allocation entry point: validates `payload`/`flags`
/// as a `VOSU_OP_READ_PLAN_SUBMIT`, executes it directly against the raw
/// buffer (no owned `ReadPlanSubmit` ever built), and writes the matching
/// `VOSU_OP_READ_PLAN_RESULT` payload into `out` — `out.clear()`s first,
/// but keeps its existing capacity, so a caller that reuses the same `out`
/// across repeated submissions of the same plan allocates nothing once
/// that capacity has grown to fit (`tests::zero_alloc` proves this for a
/// single-stage and a dependent-stage plan).
pub fn execute_into(
    payload: &[u8],
    flags: u32,
    memory: &dyn ProcessMemory,
    bitness: ProcessBitness,
    modules: &dyn ModuleResolver,
    deadline: Instant,
    out: &mut Vec<u8>,
) -> Result<(), ExecuteBytesError> {
    validate_flags(vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT, flags)
        .map_err(|error| ExecuteBytesError::Message(error.into()))?;
    validate_read_plan_submit(payload).map_err(|error| ExecuteBytesError::Message(error.into()))?;
    let pointer_width = u32::try_from(bitness.pointer_size()).unwrap_or(u32::MAX);
    validate_read_plan_ref_op_width(payload, pointer_width)
        .map_err(|_| ExecuteBytesError::Execute(ReadPlanError::RefOpWrongWidth))?;

    if !memory.is_alive() {
        return Err(ExecuteBytesError::Execute(ReadPlanError::TargetGone));
    }

    let header: vosu_read_plan_submit_header =
        read_flat_header(payload).map_err(|error| ExecuteBytesError::Message(error.into()))?;

    // The root's module name, if any, must be valid UTF-8 before any
    // per-stage work starts (`validate_read_plan_submit` only bounds-
    // checks the byte range, matching the owned decode path's own split
    // between structural validation and `owned_str`'s UTF-8 check).
    if vosu_plan_base_kind::try_from(header.root.kind) == Ok(vosu_plan_base_kind::VOSU_BASE_MODULE)
    {
        module_name_str(payload, &header.root)
            .map_err(|error| ExecuteBytesError::Message(MessageError::InvalidUtf8(error)))?;
    }

    let stage_size = size_of::<vosu_stage_header>();
    let op_size = size_of::<vosu_plan_op>();
    let mut total_ops: usize = 0;
    let mut total_len: u64 = 0;
    for stage_index in 0..header.stage_count as usize {
        let stage: vosu_stage_header = read_struct_at(
            payload,
            header.stages_offset as usize + stage_index * stage_size,
        );
        total_ops += stage.op_count as usize;
        for op_index in 0..stage.op_count as usize {
            let op: vosu_plan_op =
                read_struct_at(payload, stage.ops_offset as usize + op_index * op_size);
            total_len += u64::from(op.length);
        }
    }
    let used = RESULT_HEADER_SIZE
        + RESULT_ENTRY_SIZE * total_ops
        + usize::try_from(total_len).unwrap_or(usize::MAX);
    let mut budget = ErrorBudget::new((VOSU_READ_PLAN_MAX_BYTES as usize).saturating_sub(used));

    out.clear();
    out.resize(RESULT_HEADER_SIZE + RESULT_ENTRY_SIZE * total_ops, 0);

    let root_base = resolve_root_raw(payload, &header.root, modules);
    let mut global_index = 0usize;
    for stage_index in 0..header.stage_count as usize {
        if Instant::now() >= deadline {
            return Err(ExecuteBytesError::Execute(ReadPlanError::TimedOut));
        }
        let stage: vosu_stage_header = read_struct_at(
            payload,
            header.stages_offset as usize + stage_index * stage_size,
        );
        let stage_base = resolve_stage_base_raw(
            payload,
            header.stages_offset,
            &stage,
            out,
            &root_base,
            bitness,
        );
        process_stage_raw(
            payload,
            &stage,
            stage_base,
            memory,
            global_index,
            out,
            &mut budget,
        );
        global_index += stage.op_count as usize;
    }

    write_struct_at(
        out,
        0,
        &vosu_read_plan_result_header {
            plan: header.plan,
            results_offset: RESULT_HEADER_SIZE as u32,
            result_count: total_ops as u32,
        },
    );
    Ok(())
}

/// Convenience wrapper over [`execute_into`] that allocates its own result
/// buffer; prefer [`execute_into`] with a reused buffer on a measured hot
/// path.
pub fn execute_bytes(
    payload: &[u8],
    flags: u32,
    memory: &dyn ProcessMemory,
    bitness: ProcessBitness,
    modules: &dyn ModuleResolver,
    deadline: Instant,
) -> Result<Vec<u8>, ExecuteBytesError> {
    let mut out = Vec::new();
    execute_into(payload, flags, memory, bitness, modules, deadline, &mut out)?;
    Ok(out)
}
