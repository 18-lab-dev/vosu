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
use vosu_plugin_api::read_plan_profile::{Phase, Span};

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

/// One stage's place in the plan: its header, its dependency depth (0 for a
/// root-based stage, one more than its `ref_stage`'s otherwise) and the
/// flat result index of its first op.
#[derive(Clone, Copy)]
struct StageSlot {
    header: vosu_stage_header,
    level: u32,
    first_op: u32,
}

/// Per-thread scratch for [`plan_levels`], taken out of [`SCHEDULE`] for one
/// execution and put back afterwards, so a warmed executor allocates
/// nothing (and a reentrant call merely starts with empty scratch).
#[derive(Default)]
struct Schedule {
    /// Indexed by stage.
    slots: Vec<StageSlot>,
    /// `level << 32 | stage`, sorted: every level's stages, in stage order.
    order: Vec<u64>,
}

thread_local! {
    static SCHEDULE: std::cell::Cell<Schedule> = const {
        std::cell::Cell::new(Schedule { slots: Vec::new(), order: Vec::new() })
    };
}

/// Fills `schedule` for the validated plan and returns its level count.
/// Every stage of one level depends only on lower levels, so a whole level
/// can be read in one native batch once the level below it has finished.
fn plan_levels(
    payload: &[u8],
    header: &vosu_read_plan_submit_header,
    schedule: &mut Schedule,
) -> u32 {
    let stage_size = size_of::<vosu_stage_header>();
    schedule.slots.clear();
    schedule.order.clear();
    let mut first_op = 0u32;
    let mut levels = 0u32;
    for index in 0..header.stage_count as usize {
        let stage: vosu_stage_header =
            read_struct_at(payload, header.stages_offset as usize + index * stage_size);
        let level = match vosu_stage_base_kind::try_from(stage.base.kind) {
            Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_ROOT) => 0,
            // `validate_read_plan_submit` proved `ref_stage < index`.
            Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_DEPENDENT) => {
                schedule.slots[stage.base.ref_stage as usize].level + 1
            }
            Err(()) => {
                unreachable!(
                    "validate_read_plan_submit already rejected an invalid stage base kind"
                )
            }
        };
        levels = levels.max(level + 1);
        schedule.slots.push(StageSlot {
            header: stage,
            level,
            first_op,
        });
        schedule.order.push(u64::from(level) << 32 | index as u64);
        first_op += stage.op_count;
    }
    schedule.order.sort_unstable();
    levels
}

fn resolve_stage_base_raw(
    stage: &vosu_stage_header,
    slots: &[StageSlot],
    out: &[u8],
    root_base: &Result<u64, RawFailure>,
    bitness: ProcessBitness,
) -> Result<u64, RawFailure> {
    let unresolved = match vosu_stage_base_kind::try_from(stage.base.kind) {
        Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_ROOT) => (*root_base)?,
        Ok(vosu_stage_base_kind::VOSU_STAGE_BASE_DEPENDENT) => {
            // The referenced stage is on a lower level, so its entry is final.
            let ref_index = slots[stage.base.ref_stage as usize].first_op + stage.base.ref_op;
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

fn entry_offset(global_index: usize) -> usize {
    RESULT_HEADER_SIZE + global_index * RESULT_ENTRY_SIZE
}

fn write_entry(out: &mut [u8], global_index: usize, entry: &vosu_read_result_flat) {
    write_struct_at(out, entry_offset(global_index), entry);
}

fn read_entry(out: &[u8], global_index: usize) -> vosu_read_result_flat {
    read_struct_at(out, entry_offset(global_index))
}

/// Resolves one op's address relative to `base` (op-level, message-bearing
/// on failure).
fn resolve_op_address(base: u64, offset: i64) -> Option<usize> {
    add_signed(base, offset).and_then(|address| usize::try_from(address).ok())
}

fn op_overflow_message(base: u64, offset: i64) -> String {
    format!("address arithmetic overflow: {base:#x} + {offset:#x}")
}

/// An op's error message, written to its entry only once the whole plan has
/// run: messages are charged against the [`ErrorBudget`] in (stage, op)
/// order, as [`super::execute_stage_ops`] charges them, although levels run
/// stages out of that order. Only failures allocate one.
struct PendingMessage {
    global_index: u32,
    /// An op-level address overflow, charged before the stage's read
    /// failures (`execute_stage_ops`'s address-resolution loop).
    overflow: bool,
    message: String,
}

/// Reads `length` bytes at `address` into a freshly-appended region at the
/// tail of `out`, writing the matching entry: on success the region stays
/// and the entry points at it; on failure the region is truncated away, so
/// a failed read never costs its `length` bytes of output space (the
/// plugin-abi §6.3 worst-case accounting bounds every *successful* op's
/// reply to its declared `length`).
fn read_one_into_tail_raw(
    memory: &dyn ProcessMemory,
    address: usize,
    length: u32,
    global_index: u32,
    out: &mut Vec<u8>,
    pending: &mut Vec<PendingMessage>,
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
        Ok(()) => write_entry(
            out,
            global_index as usize,
            &ok_entry(region_start as u32, length),
        ),
        Err(error) => {
            if !memory.is_alive() {
                return Err(ReadPlanError::TargetGone);
            }
            out.truncate(region_start);
            write_entry(
                out,
                global_index as usize,
                &error_entry(error_code_for(&error), 0, 0),
            );
            pending.push(PendingMessage {
                global_index,
                overflow: false,
                message: error.to_string(),
            });
        }
    }
    Ok(())
}

/// The values every level-processing function below needs but none of
/// them mutate.
struct RawContext<'a> {
    payload: &'a [u8],
    memory: &'a dyn ProcessMemory,
    deadline: &'a RawDeadline,
}

/// Upper bound on how many ops one [`Batch`] covers (matching
/// `vosu-sys`'s Linux coalescer capacity): stack arrays of this size, not a
/// `Vec`, hold a batch's ops and `ReadRequest`s, so a level of any size
/// allocates nothing (F1); a larger level is read in several batches.
const BATCH_CAPACITY: usize = 64;

/// Address-resolved ops of one level awaiting one `read_many` call.
struct Batch {
    /// `(global_index, address, length)`; `0..len` initialized.
    ops: [MaybeUninit<(u32, usize, u32)>; BATCH_CAPACITY],
    len: usize,
}

impl Batch {
    fn new() -> Self {
        Self {
            ops: [const { MaybeUninit::uninit() }; BATCH_CAPACITY],
            len: 0,
        }
    }

    /// Queues one op, first reading the batch if it is full.
    fn push(
        &mut self,
        ctx: &RawContext<'_>,
        op: (u32, usize, u32),
        out: &mut Vec<u8>,
        pending: &mut Vec<PendingMessage>,
    ) -> Result<(), ReadPlanError> {
        if self.len == BATCH_CAPACITY {
            self.flush(ctx, out, pending)?;
        }
        self.ops[self.len].write(op);
        self.len += 1;
        Ok(())
    }

    /// Reads every queued op with one [`ProcessMemory::read_many`] call into
    /// a region appended to `out`, falling back to per-op `read_exact` only
    /// if that call itself fails. The fallback loop checks `deadline` before
    /// each of its reads (F3): a batch failure can fall back to many
    /// individually slow reads.
    fn flush(
        &mut self,
        ctx: &RawContext<'_>,
        out: &mut Vec<u8>,
        pending: &mut Vec<PendingMessage>,
    ) -> Result<(), ReadPlanError> {
        let count = std::mem::take(&mut self.len);
        if count == 0 {
            return Ok(());
        }
        // SAFETY: `push` initialized exactly `0..count`, the tuple is `Copy`
        // with no drop glue, and nothing writes `ops` until this returns.
        let ops: &[(u32, usize, u32)] =
            unsafe { std::slice::from_raw_parts(self.ops.as_ptr().cast(), count) };
        let region_start = out.len();
        let region_len: usize = ops.iter().map(|&(_, _, length)| length as usize).sum();
        out.resize(region_start + region_len, 0);

        let batch_ok = {
            let mut requests: [MaybeUninit<ReadRequest<'_>>; BATCH_CAPACITY] =
                [const { MaybeUninit::uninit() }; BATCH_CAPACITY];
            let mut remaining: &mut [u8] = &mut out[region_start..];
            for (slot, &(_, address, length)) in requests.iter_mut().zip(ops) {
                let (dst, rest) = remaining.split_at_mut(length as usize);
                slot.write(ReadRequest::new(address, dst));
                remaining = rest;
            }
            // SAFETY: the loop above wrote exactly `count` slots, `0..count`,
            // each with one `.write(...)` call; `ReadRequest` has no `Drop`
            // impl, so the array's unwritten tail needs no cleanup when
            // this block (and `requests`) goes out of scope.
            let requests: &mut [ReadRequest<'_>] = unsafe {
                std::slice::from_raw_parts_mut(
                    requests.as_mut_ptr().cast::<ReadRequest<'_>>(),
                    count,
                )
            };
            let _read_profile = Span::new(Phase::Backend).ops(count);
            ctx.memory.read_many(requests).is_ok()
        };

        if batch_ok {
            let mut offset = region_start;
            for &(global_index, _, length) in ops {
                write_entry(out, global_index as usize, &ok_entry(offset as u32, length));
                offset += length as usize;
            }
            return Ok(());
        }
        if !ctx.memory.is_alive() {
            return Err(ReadPlanError::TargetGone);
        }
        // A failed batch may have touched its destinations. None of those
        // bytes are a successful per-op result: retract the region and
        // re-read each op independently, appending only what succeeds, so
        // every op reports exactly the status its own read gets.
        out.truncate(region_start);
        for &(global_index, address, length) in ops {
            if ctx.deadline.expired()? {
                return Err(ReadPlanError::TimedOut);
            }
            read_one_into_tail_raw(ctx.memory, address, length, global_index, out, pending)?;
        }
        Ok(())
    }
}

/// Queues every op of one stage on `batch`, or writes its failures:
/// a stage whose base failed reports that failure (message-less, see
/// [`strip_message`]) for every op, and an op whose address overflows
/// reports `AddressOverflow` with its own message.
fn queue_stage_raw(
    ctx: &RawContext<'_>,
    slot: &StageSlot,
    stage_base: Result<u64, RawFailure>,
    batch: &mut Batch,
    out: &mut Vec<u8>,
    pending: &mut Vec<PendingMessage>,
) -> Result<(), ReadPlanError> {
    let op_size = size_of::<vosu_plan_op>();
    let stage = &slot.header;
    let base = match stage_base {
        Err(failure) => {
            if !ctx.memory.is_alive() {
                return Err(ReadPlanError::TargetGone);
            }
            let entry = match failure {
                RawFailure::NotReady => not_ready_entry(),
                RawFailure::Error(code) => error_entry(code, 0, 0),
            };
            for op_index in 0..stage.op_count {
                write_entry(out, (slot.first_op + op_index) as usize, &entry);
            }
            return Ok(());
        }
        Ok(base) => base,
    };
    for op_index in 0..stage.op_count {
        let op: vosu_plan_op = read_struct_at(
            ctx.payload,
            stage.ops_offset as usize + op_index as usize * op_size,
        );
        let global_index = slot.first_op + op_index;
        match resolve_op_address(base, op.offset) {
            Some(address) => batch.push(ctx, (global_index, address, op.length), out, pending)?,
            None => {
                // A single-op stage has always probed the target first here.
                if stage.op_count == 1 && !ctx.memory.is_alive() {
                    return Err(ReadPlanError::TargetGone);
                }
                write_entry(
                    out,
                    global_index as usize,
                    &error_entry(ReadErrorCode::AddressOverflow, 0, 0),
                );
                pending.push(PendingMessage {
                    global_index,
                    overflow: true,
                    message: op_overflow_message(base, op.offset),
                });
            }
        }
    }
    Ok(())
}

/// Runs the plan level by level (the module docs): each level's stages
/// resolve their bases from lower levels' entries, and every op whose
/// address resolved is read in one [`Batch`]. `deadline` is checked once
/// per level, before any of its reads.
fn run_levels(
    ctx: &RawContext<'_>,
    schedule: &Schedule,
    root_base: &Result<u64, RawFailure>,
    bitness: ProcessBitness,
    out: &mut Vec<u8>,
    pending: &mut Vec<PendingMessage>,
) -> Result<(), ReadPlanError> {
    let mut batch = Batch::new();
    for level_stages in schedule.order.chunk_by(|a, b| a >> 32 == b >> 32) {
        if ctx
            .deadline
            .stage_expired((level_stages[0] >> 32) as usize)?
        {
            return Err(ReadPlanError::TimedOut);
        }
        for &key in level_stages {
            let slot = &schedule.slots[key as u32 as usize];
            let stage_base =
                resolve_stage_base_raw(&slot.header, &schedule.slots, out, root_base, bitness);
            queue_stage_raw(ctx, slot, stage_base, &mut batch, out, pending)?;
        }
        batch.flush(ctx, out, pending)?;
    }
    Ok(())
}

/// Charges every pending message against `budget` in (stage, op) order —
/// per stage, address overflows before read failures, every op that is not
/// `Ok` freeing its `length` first — then appends the messages to `out`
/// and points their entries at them.
fn write_pending_messages(
    payload: &[u8],
    slots: &[StageSlot],
    mut pending: Vec<PendingMessage>,
    mut budget: ErrorBudget,
    out: &mut Vec<u8>,
) {
    if pending.is_empty() {
        return;
    }
    pending.sort_unstable_by_key(|message| message.global_index);
    let op_size = size_of::<vosu_plan_op>();
    let op_length = |slot: &StageSlot, op_index: u32| -> u32 {
        let op: vosu_plan_op = read_struct_at(
            payload,
            slot.header.ops_offset as usize + op_index as usize * op_size,
        );
        op.length
    };
    let mut next = 0usize;
    for slot in slots {
        if next == pending.len() {
            break;
        }
        let end_op = slot.first_op + slot.header.op_count;
        let stage_end = next
            + pending[next..]
                .iter()
                .take_while(|message| message.global_index < end_op)
                .count();
        let stage_pending = &mut pending[next..stage_end];
        for message in stage_pending.iter_mut().filter(|message| message.overflow) {
            budget.recover(op_length(slot, message.global_index - slot.first_op));
            message.message = budget.charge_message(std::mem::take(&mut message.message));
        }
        let mut cursor = 0usize;
        for op_index in 0..slot.header.op_count {
            let global_index = slot.first_op + op_index;
            let own = stage_pending
                .get(cursor)
                .filter(|message| message.global_index == global_index);
            if own.is_some() {
                cursor += 1;
            }
            if own.is_some_and(|message| message.overflow) {
                continue;
            }
            let kind = read_entry(out, global_index as usize).kind;
            if kind == vosu_read_result_kind::VOSU_READ_OK as u32 {
                continue;
            }
            budget.recover(op_length(slot, op_index));
            if own.is_some() {
                let message = &mut stage_pending[cursor - 1];
                message.message = budget.charge_message(std::mem::take(&mut message.message));
            }
        }
        next = stage_end;
    }
    for message in pending {
        let offset = out.len() as u32;
        out.extend_from_slice(message.message.as_bytes());
        let index = message.global_index as usize;
        let mut entry = read_entry(out, index);
        entry.error_message_offset = offset;
        entry.error_message_len = message.message.len() as u32;
        write_entry(out, index, &entry);
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

        let total_ops = layout.total_ops;
        let total_len = layout.total_len;
        let used = RESULT_HEADER_SIZE
            + RESULT_ENTRY_SIZE * total_ops
            + usize::try_from(total_len).unwrap_or(usize::MAX);
        let budget = ErrorBudget::new((VOSU_READ_PLAN_MAX_BYTES as usize).saturating_sub(used));
        let mut schedule = SCHEDULE.take();
        let levels = plan_levels(payload, &header, &mut schedule);
        drop(validation_profile);
        let result = deadline.begin(levels).and_then(|()| {
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
            let mut pending = Vec::new();
            run_levels(&ctx, &schedule, &root_base, bitness, out, &mut pending)?;
            write_pending_messages(payload, &schedule.slots, pending, budget, out);
            Ok(())
        });
        SCHEDULE.set(schedule);
        result.map_err(ExecuteBytesError::Execute)?;

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
