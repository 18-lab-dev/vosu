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
//!
//! Every unchecked offset `read_struct_at`/`write_struct_at` touch here is
//! bounds-checked only by a `debug_assert!`; the release build's memory
//! safety therefore rests entirely on `vosu_plugin_api::validate_read_plan_submit`/
//! `validate_read_plan_ref_op_width` having already rejected anything this
//! module's own arithmetic could walk out of bounds — see [`execute_into`]'s
//! first two calls.

use std::mem::MaybeUninit;

use super::deadline::RawDeadline;
use super::*;
use vosu_plugin_internal::read_plan_profile::{Phase, Span};

mod shape;

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
    previous_stage: Option<(u32, u32)>,
) -> Result<u64, RawFailure> {
    let unresolved = match vosu_stage_base_kind::try_from(stage.base.kind) {
        Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_ROOT) => (*root_base)?,
        Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_DEPENDENT) => {
            // Pointer chains normally reference the immediately preceding stage.
            // Its prefix is already known; avoid rescanning every earlier header.
            let start = match previous_stage {
                Some((index, start)) if index == stage.base.ref_stage => start,
                _ => stage_start_index(payload, stages_offset, stage.base.ref_stage),
            };
            let ref_index = start + stage.base.ref_op;
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

/// Reads `length` bytes at `address` into a freshly-appended region at the
/// tail of `out`, writing the matching entry: on success the region stays
/// and the entry points at it; on failure the just-reserved region is
/// truncated away first, so a failed read never costs its `length` bytes
/// of output space — shared by [`process_single_op_raw`] and the
/// multi-op fallback below so both recover [`ErrorBudget`] room on
/// failure exactly like [`super::execute_stage_ops`] does (the plugin-abi
/// §6.3 worst-case accounting bounds every *successful* op's reply to its
/// declared `length`; nothing here, success or failure, may exceed it).
fn read_one_into_tail_raw(
    memory: &dyn ProcessMemory,
    address: usize,
    length: u32,
    global_index: usize,
    out: &mut Vec<u8>,
    budget: &mut ErrorBudget,
) -> Result<(), ReadPlanError> {
    let region_start = out.len();
    out.resize(region_start + length as usize, 0);
    let read_profile = Span::new(Phase::Backend).ops(1);
    let result = memory.read_exact(
        address,
        &mut out[region_start..region_start + length as usize],
    );
    drop(read_profile);
    match result {
        Ok(()) => write_entry(out, global_index, &ok_entry(region_start as u32, length)),
        Err(error) => {
            if !memory.is_alive() {
                return Err(ReadPlanError::TargetGone);
            }
            out.truncate(region_start);
            budget.recover(length);
            write_op_error_entry(
                out,
                global_index,
                error_code_for(&error),
                error.to_string(),
                budget,
            );
        }
    }
    Ok(())
}

/// Bundles the three values every stage-processing function below needs
/// but none of them mutate, so adding `deadline` (F3) did not push any of
/// them over clippy's argument-count limit.
struct RawContext<'a> {
    payload: &'a [u8],
    memory: &'a dyn ProcessMemory,
    deadline: &'a RawDeadline,
}

fn process_single_op_raw(
    ctx: &RawContext<'_>,
    stage: &vosu_stage_header,
    base: u64,
    global_index: usize,
    out: &mut Vec<u8>,
    budget: &mut ErrorBudget,
) -> Result<(), ReadPlanError> {
    let op: vosu_plan_op = read_struct_at(ctx.payload, stage.ops_offset as usize);
    let Some(address) = resolve_op_address(base, op.offset) else {
        if !ctx.memory.is_alive() {
            return Err(ReadPlanError::TargetGone);
        }
        budget.recover(op.length);
        write_op_error_entry(
            out,
            global_index,
            ReadErrorCode::AddressOverflow,
            op_overflow_message(base, op.offset),
            budget,
        );
        return Ok(());
    };
    read_one_into_tail_raw(ctx.memory, address, op.length, global_index, out, budget)
}

/// Upper bound on how many sibling ops one [`process_multi_op_raw`] batch
/// call covers at once (matching `src/process/linux.rs`'s own coalescer
/// capacity): a stack array of this size, not a `Vec`, holds the batch's
/// per-op metadata and `ReadRequest`s, so a multi-op stage allocates
/// nothing regardless of its total op count (F1) — `validate_read_plan_submit`'s
/// 24-bytes-per-op worst case already bounds one stage to about 2,730 ops,
/// so the largest validated stage costs a few dozen chunks, not a few
/// dozen allocations.
const MULTI_OP_CHUNK: usize = 64;

fn process_multi_op_chunk_raw<const CAP: usize>(
    ctx: &RawContext<'_>,
    stage: &vosu_stage_header,
    base: u64,
    global_index_start: usize,
    chunk: std::ops::Range<usize>,
    out: &mut Vec<u8>,
    budget: &mut ErrorBudget,
) -> Result<(), ReadPlanError> {
    let op_size = size_of::<vosu_plan_op>();
    // (op_index, address, out_offset, length) for each op in this chunk
    // whose address resolved — a plain stack array, never a `Vec`.
    let mut participants = [(0usize, 0usize, 0usize, 0u32); CAP];
    let mut participant_count = 0usize;
    // An address-overflow op's message is charged against the budget
    // immediately below (in the same op-index order [`super::execute_stage_ops`]'s
    // own address-resolution loop charges in, so the two paths' budget
    // truncation agrees, F2), but its bytes are appended to `out`'s tail
    // only in the final pass below — never interleaved with the
    // participant region a failed batch call may still truncate away.
    let mut deferred_overflows: Vec<(usize, String)> = Vec::new();

    for op_index in chunk {
        let op: vosu_plan_op =
            read_struct_at(ctx.payload, stage.ops_offset as usize + op_index * op_size);
        match resolve_op_address(base, op.offset) {
            Some(address) => {
                let out_offset = out.len();
                out.resize(out_offset + op.length as usize, 0);
                participants[participant_count] = (op_index, address, out_offset, op.length);
                participant_count += 1;
            }
            None => {
                budget.recover(op.length);
                let message = budget.charge_message(op_overflow_message(base, op.offset));
                deferred_overflows.push((op_index, message));
            }
        }
    }

    if participant_count > 0 {
        let region_start = participants[0].2;

        let batch_ok = {
            let mut requests: [MaybeUninit<ReadRequest<'_>>; CAP] =
                [const { MaybeUninit::uninit() }; CAP];
            let mut remaining: &mut [u8] = &mut out[region_start..];
            let mut cursor = region_start;
            for (slot, &(_, address, out_offset, length)) in
                requests.iter_mut().zip(&participants[..participant_count])
            {
                let gap = out_offset - cursor;
                let (_skip, rest) = remaining.split_at_mut(gap);
                let (slice, rest2) = rest.split_at_mut(length as usize);
                slot.write(ReadRequest::new(address, slice));
                remaining = rest2;
                cursor = out_offset + length as usize;
            }
            // SAFETY: the loop above wrote exactly `participant_count`
            // slots, `0..participant_count`, each with one `.write(...)`
            // call; `ReadRequest` has no `Drop` impl, so the array's
            // unwritten tail needs no cleanup when this block (and
            // `requests`) goes out of scope.
            let requests: &mut [ReadRequest<'_>] = unsafe {
                std::slice::from_raw_parts_mut(
                    requests.as_mut_ptr().cast::<ReadRequest<'_>>(),
                    participant_count,
                )
            };
            let _read_profile = Span::new(Phase::Backend).ops(participant_count);
            ctx.memory.read_many(requests).is_ok()
        };

        if batch_ok {
            for &(op_index, _, out_offset, length) in &participants[..participant_count] {
                write_entry(
                    out,
                    global_index_start + op_index,
                    &ok_entry(out_offset as u32, length),
                );
            }
        } else {
            if !ctx.memory.is_alive() {
                return Err(ReadPlanError::TargetGone);
            }
            // A failed batch may have touched its destinations. None of
            // those bytes are a successful per-op result: retract every
            // reserved byte and re-read each op independently, appending only
            // what actually succeeds, so a failure here recovers its
            // budget exactly like `read_one_into_tail_raw` (F2: matching
            // `super::execute_stage_ops`'s own fallback accounting). Safe
            // because nothing but this chunk's participants has been
            // appended past `region_start` yet — the overflow ops above
            // deferred their own tail writes past this point.
            out.truncate(region_start);
            for &(op_index, address, _, length) in &participants[..participant_count] {
                if ctx.deadline.expired()? {
                    return Err(ReadPlanError::TimedOut);
                }
                read_one_into_tail_raw(
                    ctx.memory,
                    address,
                    length,
                    global_index_start + op_index,
                    out,
                    budget,
                )?;
            }
        }
    }

    for (op_index, message) in deferred_overflows {
        let (offset, len) = append_message(out, &message);
        write_entry(
            out,
            global_index_start + op_index,
            &error_entry(ReadErrorCode::AddressOverflow, offset, len),
        );
    }
    Ok(())
}

fn process_multi_op_raw(
    ctx: &RawContext<'_>,
    stage: &vosu_stage_header,
    base: u64,
    global_index_start: usize,
    out: &mut Vec<u8>,
    budget: &mut ErrorBudget,
) -> Result<(), ReadPlanError> {
    let op_count = stage.op_count as usize;
    // Precise captures usually contain only the pointer and its current value.
    // Avoid clearing the full 64-op scratch array for those reads.
    // The same implementation retains all overflow, fallback and deadline rules.
    if op_count <= 4 {
        return process_multi_op_chunk_raw::<4>(
            ctx,
            stage,
            base,
            global_index_start,
            0..op_count,
            out,
            budget,
        );
    }
    let mut chunk_start = 0usize;
    while chunk_start < op_count {
        let chunk_end = (chunk_start + MULTI_OP_CHUNK).min(op_count);
        process_multi_op_chunk_raw::<MULTI_OP_CHUNK>(
            ctx,
            stage,
            base,
            global_index_start,
            chunk_start..chunk_end,
            out,
            budget,
        )?;
        chunk_start = chunk_end;
    }
    Ok(())
}

fn process_stage_raw(
    ctx: &RawContext<'_>,
    stage: &vosu_stage_header,
    stage_base: Result<u64, RawFailure>,
    global_index_start: usize,
    out: &mut Vec<u8>,
    budget: &mut ErrorBudget,
) -> Result<(), ReadPlanError> {
    let op_size = size_of::<vosu_plan_op>();
    let base = match stage_base {
        Err(failure) => {
            if !ctx.memory.is_alive() {
                return Err(ReadPlanError::TargetGone);
            }
            for op_index in 0..stage.op_count as usize {
                let op: vosu_plan_op =
                    read_struct_at(ctx.payload, stage.ops_offset as usize + op_index * op_size);
                budget.recover(op.length);
                let entry = match failure {
                    RawFailure::NotReady => not_ready_entry(),
                    RawFailure::Error(code) => error_entry(code, 0, 0),
                };
                write_entry(out, global_index_start + op_index, &entry);
            }
            return Ok(());
        }
        Ok(base) => base,
    };

    match stage.op_count {
        0 => Ok(()),
        1 => process_single_op_raw(ctx, stage, base, global_index_start, out, budget),
        _ => process_multi_op_raw(ctx, stage, base, global_index_start, out, budget),
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
    ValidatedPlan::new(payload, flags)?.execute(memory, bitness, modules, deadline, out)
}

/// Executes a fresh synchronous submission with the production coarse-budget
/// policy. Like [`execute_into`], validates the complete payload before reading.
/// This lets in-process hosts use the same deadline policy as `PluginHost`.
pub fn execute_sync_into(
    payload: &[u8],
    flags: u32,
    memory: &dyn ProcessMemory,
    bitness: ProcessBitness,
    modules: &dyn ModuleResolver,
    budget: std::time::Duration,
    out: &mut Vec<u8>,
) -> Result<(), ExecuteBytesError> {
    ValidatedPlan::new(payload, flags)?.execute_sync(memory, bitness, modules, budget, out)
}

/// Borrows the exact buffer whose structural validation succeeded. Keeping the
/// payload private prevents the host from accidentally executing a different or
/// mutated buffer after validating session ownership.
pub(crate) struct ValidatedPlan<'a> {
    payload: &'a [u8],
    header: vosu_read_plan_submit_header,
    layout: shape::Layout,
}

impl<'a> ValidatedPlan<'a> {
    pub(crate) fn new(payload: &'a [u8], flags: u32) -> Result<Self, ExecuteBytesError> {
        let _validation_profile = Span::new(Phase::Validation);
        let (header, layout) = shape::validate(payload, flags)?;
        Ok(Self {
            payload,
            header,
            layout,
        })
    }

    pub(crate) fn handle(&self) -> u64 {
        self.header.handle
    }

    pub(crate) fn execute(
        self,
        memory: &dyn ProcessMemory,
        bitness: ProcessBitness,
        modules: &dyn ModuleResolver,
        deadline: Instant,
        out: &mut Vec<u8>,
    ) -> Result<(), ExecuteBytesError> {
        self.execute_with_deadline(
            memory,
            bitness,
            modules,
            RawDeadline::Absolute(deadline),
            out,
        )
    }

    pub(crate) fn execute_sync(
        self,
        memory: &dyn ProcessMemory,
        bitness: ProcessBitness,
        modules: &dyn ModuleResolver,
        budget: std::time::Duration,
        out: &mut Vec<u8>,
    ) -> Result<(), ExecuteBytesError> {
        self.execute_with_deadline(memory, bitness, modules, RawDeadline::sync(budget), out)
    }

    fn execute_with_deadline(
        self,
        memory: &dyn ProcessMemory,
        bitness: ProcessBitness,
        modules: &dyn ModuleResolver,
        deadline: RawDeadline,
        out: &mut Vec<u8>,
    ) -> Result<(), ExecuteBytesError> {
        let validation_profile = Span::new(Phase::Validation);
        let Self {
            payload,
            header,
            layout,
        } = self;

        // Validate width before runtime errors. Native reads either pin the OS
        // process object or check its generation after reading, so a second
        // pre-plan health syscall adds no PID-reuse protection. Failed reads
        // still probe health to distinguish TARGET_GONE from address faults.
        let ref_width_ok = match bitness {
            ProcessBitness::X86 => layout.ref_width_x86,
            ProcessBitness::X64 => layout.ref_width_x64,
        };
        if !ref_width_ok {
            return Err(ExecuteBytesError::Execute(ReadPlanError::RefOpWrongWidth));
        }
        if (!memory.reads_pin_process_identity() || layout.total_len == 0) && !memory.is_alive() {
            return Err(ExecuteBytesError::Execute(ReadPlanError::TargetGone));
        }

        // The root's module name, if any, must be valid UTF-8 before any
        // per-stage work starts (`validate_read_plan_submit` only bounds-
        // checks the byte range, matching the owned decode path's own split
        // between structural validation and `owned_str`'s UTF-8 check).
        if vosu_plan_base_kind::try_from(header.root.kind)
            == Ok(vosu_plan_base_kind::VOSU_BASE_MODULE)
        {
            module_name_str(payload, &header.root)
                .map_err(|error| ExecuteBytesError::Message(MessageError::InvalidUtf8(error)))?;
        }

        let stage_size = size_of::<vosu_stage_header>();
        let total_ops = layout.total_ops;
        let total_len = layout.total_len;
        let used = RESULT_HEADER_SIZE
            + RESULT_ENTRY_SIZE * total_ops
            + usize::try_from(total_len).unwrap_or(usize::MAX);
        let mut budget = ErrorBudget::new((VOSU_READ_PLAN_MAX_BYTES as usize).saturating_sub(used));
        drop(validation_profile);
        deadline
            .begin(header.stage_count)
            .map_err(ExecuteBytesError::Execute)?;

        out.clear();
        out.resize(RESULT_HEADER_SIZE + RESULT_ENTRY_SIZE * total_ops, 0);
        let root_base = {
            let _root_profile = Span::new(Phase::RootResolution);
            resolve_root_raw(payload, &header.root, modules)
        };
        let ctx = RawContext {
            payload,
            memory,
            deadline: &deadline,
        };
        let mut global_index = 0usize;
        let mut previous_stage = None;
        for stage_index in 0..header.stage_count as usize {
            if deadline
                .stage_expired(stage_index)
                .map_err(ExecuteBytesError::Execute)?
            {
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
                previous_stage,
            );
            process_stage_raw(&ctx, &stage, stage_base, global_index, out, &mut budget)
                .map_err(ExecuteBytesError::Execute)?;
            previous_stage = Some((stage_index as u32, global_index as u32));
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
