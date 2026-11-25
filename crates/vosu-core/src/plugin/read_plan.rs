//! Read-plan executor (`docs/v2/plugin-abi.md` §6.3) and the opt-in
//! synchronous-delivery mechanism (D5, §4): executes one decoded
//! `VOSU_OP_READ_PLAN_SUBMIT` against a `vosu_plugin_api::memory::ProcessMemory`
//! backend, honoring a deadline, and produces the matching
//! `VOSU_OP_READ_PLAN_RESULT`.
//!
//! This module is deliberately standalone: it knows nothing about
//! `PluginInstanceId`, the session registry, or `PluginHostHandler`. Wiring
//! it into `PluginHostHandler::handle` — routing a decoded
//! `Message::ReadPlanSubmit` here with the right session's `ProcessMemory`
//! and `ProcessBitness`, delivering a [`deliver_sync`] result back through
//! `send`'s own return value, resolving `PlanBase::Module` against the
//! session's real module list, and answering `VOSU_OP_READ_PLAN_CANCEL`
//! (now that [`OutstandingPlans`] frees a slot and tracks its owning
//! session itself) — is later wiring, not this phase's scope.
//!
//! Two execution entry points exist:
//! - [`execute`] operates on an already-decoded [`ReadPlanSubmit`] and
//!   allocates freely; it exists for tests and callers that already hold
//!   the typed form for other reasons.
//! - [`execute_into`]/[`execute_bytes`] walk the raw, already-validated
//!   flat submit buffer directly and write the `VOSU_OP_READ_PLAN_RESULT`
//!   straight into a caller-provided, reusable `Vec<u8>` — no owned
//!   `ReadPlanSubmit`/`Stage`/`PlanOp`/`String` intermediate form at all
//!   (`docs/v2/plugin-abi.md` §3 picked the flat layout precisely so a read
//!   plan costs "writing a small struct"; this is the path that actually
//!   delivers that). Both entry points batch a stage's sibling reads into
//!   one `ProcessMemory::read_many` call, falling back to per-op
//!   `read_exact` only if that batch call itself fails, and check
//!   `deadline` once per stage rather than before every individual read.
//!
//! `vosu_plugin_api::validate_read_plan_ref_op_width` needs the plan's raw
//! encoded buffer: [`execute_into`] (which has it) calls that function
//! directly; [`execute`] (which only has the decoded form) re-derives the
//! identical bitness-aware check via [`ref_op_width_ok`] instead.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use vosu_plugin_api::memory::{MemoryError, ProcessBitness, ProcessMemory, ReadRequest};
use vosu_plugin_api::{
    MessageError, PlanBase, PlanOp, ReadErrorCode, ReadPlanResult, ReadPlanSubmit, ReadResult,
    Stage, StageBase, VOSU_READ_PLAN_MAX_BYTES, read_flat_header, validate_flags,
    validate_read_plan_ref_op_width, validate_read_plan_submit, vosu_opcode, vosu_plan_base_flat,
    vosu_plan_base_kind, vosu_plan_id, vosu_plan_op, vosu_read_error_code,
    vosu_read_plan_result_header, vosu_read_plan_submit_header, vosu_read_result_flat,
    vosu_read_result_kind, vosu_stage_base_kind, vosu_stage_header, vosu_status,
};

/// Per-instance bound on asynchronous (non-`VOSU_READ_PLAN_SYNC`) read
/// plans outstanding at once (`docs/v2/plugin-abi.md` §5, §6.3): a
/// `READ_PLAN_SUBMIT` beyond it is refused with `VOSU_ERR_INVALID_ARGUMENT`
/// before any read executes.
pub const MAX_OUTSTANDING_ASYNC_PLANS: usize = 64;

/// Caps the byte length of a single error message embedded in a
/// [`ReadResult::Error`], on top of the running total every message in one
/// plan shares ([`ErrorBudget`]): `vosu_plugin_api::validate_read_plan_submit`'s
/// pre-read worst-case `VOSU_READ_PLAN_MAX_BYTES` bound assumes every op's
/// reply is at most its declared `length` plus a fixed per-result header,
/// so a failed op's zero-length reply frees exactly that much room; a
/// verbose `MemoryError::Display` is truncated to whichever is smaller.
const MAX_ERROR_MESSAGE_BYTES: usize = 256;

const RESULT_HEADER_SIZE: usize = size_of::<vosu_read_plan_result_header>();
const RESULT_ENTRY_SIZE: usize = size_of::<vosu_read_result_flat>();

/// Resolves a read plan's `PlanBase::Module` root to that module's loaded
/// base address in the target process (`docs/v2/plugin-abi.md` §6.3: "a
/// module name plus offset, resolved by core against that session's
/// module list so the plugin never computes an ASLR-adjusted address
/// itself"). `None` when the named module is not currently loaded: every
/// op whose stage resolves (directly or transitively, through a dependent
/// chain) from this root reports [`ReadResult::NotReady`] (mirroring
/// `src/process.rs`'s `ReadStatus::NotReady`) rather than rejecting the
/// whole submission.
pub trait ModuleResolver {
    fn resolve(&self, module: &str) -> Option<u64>;
}

/// Every way [`execute`]/[`execute_into`] can reject a submission outright
/// (no partial result; `docs/v2/plugin-abi.md` §5: "a non-`VOSU_OK` status
/// means the call itself was rejected").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadPlanError {
    /// A dependent stage's `ref_op` names an op whose declared `length`
    /// does not equal the session's pointer width — a plan validation
    /// error, not a silent truncation or zero-extension (§6.3).
    RefOpWrongWidth,
    /// Execution exceeded its deadline (§5 `VOSU_ERR_TIMED_OUT`).
    TimedOut,
    /// The target session's process was already dead before any read
    /// executed (§5 `VOSU_ERR_TARGET_GONE`).
    TargetGone,
    /// The platform's coarse monotonic clock was unavailable.
    ClockUnavailable,
}

impl From<ReadPlanError> for vosu_status {
    fn from(error: ReadPlanError) -> Self {
        match error {
            ReadPlanError::RefOpWrongWidth => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            ReadPlanError::TimedOut => vosu_status::VOSU_ERR_TIMED_OUT,
            ReadPlanError::TargetGone => vosu_status::VOSU_ERR_TARGET_GONE,
            ReadPlanError::ClockUnavailable => vosu_status::VOSU_ERR_BACKEND,
        }
    }
}

/// Every way [`execute_bytes`]/[`execute_into`] can fail, layering
/// [`ReadPlanError`] over `vosu_plugin_api`'s own validation/decode
/// failures.
#[derive(Debug)]
pub enum ExecuteBytesError {
    /// `payload`/`flags` did not validate as a `VOSU_OP_READ_PLAN_SUBMIT`.
    Message(MessageError),
    Execute(ReadPlanError),
}

impl From<ExecuteBytesError> for vosu_status {
    fn from(error: ExecuteBytesError) -> Self {
        match error {
            ExecuteBytesError::Message(_) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            ExecuteBytesError::Execute(error) => error.into(),
        }
    }
}

/// Re-derives `vosu_plugin_api::validate_read_plan_ref_op_width`'s check
/// against the decoded form, for [`execute`] (which has no raw buffer to
/// hand that function) and for `host/read_plans.rs`'s own synchronous
/// pre-submit validation (P1-3: an asynchronous plan validates this
/// before admission, not inside the worker, where there is no longer a
/// synchronous caller to report `VOSU_ERR_INVALID_ARGUMENT` to): every
/// dependent stage's `ref_op` must name an earlier stage's op whose
/// declared `length` equals `pointer_width`. `Message::decode` already
/// bounds-checked every `ref_stage`/`ref_op` index through
/// `vosu_plugin_api::validate_read_plan_submit`, so indexing here never
/// panics.
pub(crate) fn ref_op_width_ok(submit: &ReadPlanSubmit, pointer_width: u32) -> bool {
    submit.stages.iter().all(|stage| match stage.base {
        StageBase::Root => true,
        StageBase::Dependent { ref_stage, ref_op } => {
            submit.stages[ref_stage as usize].ops[ref_op as usize].length == pointer_width
        }
    })
}

/// Tracks how many more error-message bytes a plan's result may still
/// spend without pushing the encoded `VOSU_OP_READ_PLAN_RESULT` past
/// `VOSU_READ_PLAN_MAX_BYTES` (`docs/v2/plugin-abi.md` §6.3). Seeded with
/// the slack `validate_read_plan_submit`'s own worst-case accounting
/// leaves (every op assumed to succeed at its full declared `length`);
/// every op that does *not* end up `Ok` frees its `length` bytes back
/// into the budget before that op's own message (if any) is charged
/// against it, so the running total can never exceed the validated cap.
struct ErrorBudget {
    remaining: usize,
}

impl ErrorBudget {
    fn new(remaining: usize) -> Self {
        Self { remaining }
    }

    /// Call once for every op that does not end up `Ok`: its `length`
    /// bytes of data were never written, freeing that much room.
    fn recover(&mut self, length: u32) {
        self.remaining = self.remaining.saturating_add(length as usize);
    }

    /// Truncates `message` to whichever is smaller, [`MAX_ERROR_MESSAGE_BYTES`]
    /// or the budget's own remaining room, and charges what it spends.
    fn charge_message(&mut self, mut message: String) -> String {
        let cap = self.remaining.min(MAX_ERROR_MESSAGE_BYTES);
        if message.len() > cap {
            let mut end = cap;
            while end > 0 && !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        self.remaining = self.remaining.saturating_sub(message.len());
        message
    }
}

/// `ReadResult::Error`'s `code` for `error`; shared by every path that maps
/// a [`MemoryError`] to a read result.
fn error_code_for(error: &MemoryError) -> ReadErrorCode {
    match error {
        MemoryError::Backend { .. } => ReadErrorCode::Backend,
        MemoryError::PermissionDenied { .. } => ReadErrorCode::PermissionDenied,
        MemoryError::Read { .. } => ReadErrorCode::ReadFailed,
        MemoryError::AddressOverflow { .. } => ReadErrorCode::AddressOverflow,
        // `ProcessMemory::read_exact`/`read_many` never produce these two
        // (they decode the osu!-specific managed heap, a layer above the
        // raw read-plan boundary); mapped defensively rather than panicking.
        MemoryError::Corrupt { .. } | MemoryError::InvalidUtf16 { .. } => ReadErrorCode::Backend,
    }
}

fn memory_error_to_result(error: MemoryError, budget: &mut ErrorBudget) -> ReadResult {
    let code = error_code_for(&error);
    ReadResult::Error {
        code,
        message: budget.charge_message(error.to_string()),
    }
}

/// The message every op that did not get to run before `deadline` passed
/// reports (`docs/v2/plugin-abi.md` §5's `VOSU_ERR_TIMED_OUT`, mirrored
/// per read: a stage that already finished keeps its result, so a
/// deadline mid-plan never rejects the whole submission, only the reads
/// it did not reach in time).
const DEADLINE_MESSAGE: &str = "plan deadline exceeded";

/// A `VOSU_READ_ERROR`/`VOSU_READ_ERROR_BACKEND` result for one op that
/// did not get to run before `deadline` passed.
fn timeout_result(budget: &mut ErrorBudget) -> ReadResult {
    ReadResult::Error {
        code: ReadErrorCode::Backend,
        message: budget.charge_message(DEADLINE_MESSAGE.to_owned()),
    }
}

/// Every op in `submit`, flattened in (stage, op) order, as
/// `ReadResult::NotReady` — the target process was already dead before
/// any read executed (`docs/v2/plugin-abi.md` §5: "Per-read failure
/// inside an otherwise-accepted plan is reported per read", mirroring
/// `src/process.rs`'s own `ReadStatus::NotReady`); an asynchronous plan
/// has no synchronous caller left to report a whole-call rejection to by
/// the time a worker discovers this, so [`execute`] answers per read
/// instead of rejecting the call.
fn not_ready_result(submit: &ReadPlanSubmit) -> ReadPlanResult {
    ReadPlanResult {
        plan: submit.plan,
        results: submit
            .stages
            .iter()
            .flat_map(|stage| stage.ops.iter())
            .map(|_| ReadResult::NotReady)
            .collect(),
    }
}

/// Strips a propagated failure's message to empty: a dependent stage's
/// base resolution (or the plan's own root) fans one underlying failure
/// out to every op it blocks, so only the first, genuinely new occurrence
/// of a message is worth its budget — every copy distributed to the ops it
/// blocks carries none.
fn strip_message(result: &ReadResult) -> ReadResult {
    match result {
        ReadResult::Ok(bytes) => ReadResult::Ok(bytes.clone()),
        ReadResult::NotReady => ReadResult::NotReady,
        ReadResult::Unsupported => ReadResult::Unsupported,
        ReadResult::Error { code, .. } => ReadResult::Error {
            code: *code,
            message: String::new(),
        },
    }
}

/// Adds a signed byte offset to an address, reporting
/// `ReadErrorCode::AddressOverflow` instead of wrapping or panicking
/// (`docs/v2/plugin-abi.md` §6.3's `base`/`offset` arithmetic).
fn checked_add_signed(base: u64, offset: i64) -> Result<u64, ReadResult> {
    u64::try_from(i128::from(base) + i128::from(offset)).map_err(|_| ReadResult::Error {
        code: ReadErrorCode::AddressOverflow,
        message: format!("address arithmetic overflow: {base:#x} + {offset:#x}"),
    })
}

/// Interprets `bytes` (already validated the same length as
/// `bitness.pointer_size()`) as a little-endian pointer-width value.
fn bytes_to_pointer(bytes: &[u8], bitness: ProcessBitness) -> u64 {
    match bitness {
        ProcessBitness::X86 => u64::from(u32::from_le_bytes(bytes.try_into().unwrap_or([0; 4]))),
        ProcessBitness::X64 => u64::from_le_bytes(bytes.try_into().unwrap_or([0; 8])),
    }
}

fn resolve_root(root: &PlanBase, modules: &dyn ModuleResolver) -> Result<u64, ReadResult> {
    match root {
        PlanBase::Absolute(address) => Ok(*address),
        PlanBase::Module { name, offset } => {
            let base = modules.resolve(name).ok_or(ReadResult::NotReady)?;
            checked_add_signed(base, *offset)
        }
    }
}

fn resolve_stage_base(
    stage: &Stage,
    per_stage: &[Vec<ReadResult>],
    root_base: &Result<u64, ReadResult>,
    bitness: ProcessBitness,
) -> Result<u64, ReadResult> {
    let unresolved = match &stage.base {
        StageBase::Root => root_base.clone()?,
        StageBase::Dependent { ref_stage, ref_op } => {
            match &per_stage[*ref_stage as usize][*ref_op as usize] {
                ReadResult::Ok(bytes) => bytes_to_pointer(bytes, bitness),
                other => return Err(other.clone()),
            }
        }
    };
    checked_add_signed(unresolved, stage.base_offset)
}

/// F2 (`docs/v2/plugin-abi.md` §5, §6.3, D5): an async plan has no
/// synchronous caller left to report a whole-call rejection to by the
/// time a worker discovers a dead target or a passed deadline, so it
/// answers per read instead ([`DeadlineMode::PerRead`]); a
/// `VOSU_READ_PLAN_SYNC` submission still has exactly that caller, so the
/// spec's whole-call `VOSU_ERR_TARGET_GONE`/`VOSU_ERR_TIMED_OUT` apply
/// there instead ([`DeadlineMode::RejectCall`]). The per-read conversion
/// (P1-3) stays async-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineMode {
    PerRead,
    RejectCall,
}

/// Executes one stage's independent ops against `memory`, batching every
/// address-resolved op into a single [`ProcessMemory::read_many`] call and
/// falling back to per-op `read_exact` only if that call itself fails
/// (P1: one syscall per stage, not one per op); the fallback loop checks
/// `deadline` before each of its own reads (F3), since a batch failure can
/// fall back to hundreds of individually slow reads that a once-per-stage
/// check would otherwise let run unbounded. In [`DeadlineMode::PerRead`],
/// never rejects the whole submission: an op that does not get to run
/// before `deadline` passes reports [`timeout_result`] instead (P1-3),
/// exactly like any other per-read failure. In
/// [`DeadlineMode::RejectCall`] (F2), the first op that does not get to
/// run before `deadline` passes rejects the whole call with
/// [`ReadPlanError::TimedOut`] instead, matching D5/§6.3.
fn execute_stage_ops(
    memory: &dyn ProcessMemory,
    stage_base: &Result<u64, ReadResult>,
    ops: &[PlanOp],
    budget: &mut ErrorBudget,
    deadline: Instant,
    mode: DeadlineMode,
) -> Result<Vec<ReadResult>, ReadPlanError> {
    let base = match stage_base {
        Ok(base) => *base,
        Err(failure) => {
            if memory.reads_pin_process_identity() && !memory.is_alive() {
                return Err(ReadPlanError::TargetGone);
            }
            let failure = strip_message(failure);
            return Ok(ops
                .iter()
                .map(|op| {
                    budget.recover(op.length);
                    failure.clone()
                })
                .collect());
        }
    };

    let mut results: Vec<Option<ReadResult>> = ops.iter().map(|_| None).collect();
    let mut participants: Vec<(usize, usize)> = Vec::new();
    let mut buffers: Vec<Vec<u8>> = Vec::new();

    for (index, op) in ops.iter().enumerate() {
        let address = checked_add_signed(base, op.offset).and_then(|address| {
            usize::try_from(address).map_err(|_| ReadResult::Error {
                code: ReadErrorCode::AddressOverflow,
                message: format!("address {address:#x} exceeds this platform's usize range"),
            })
        });
        match address {
            Ok(address) => {
                participants.push((index, address));
                buffers.push(vec![0u8; op.length as usize]);
            }
            Err(failure) => {
                budget.recover(op.length);
                results[index] = Some(match failure {
                    ReadResult::Error { code, message } => ReadResult::Error {
                        code,
                        message: budget.charge_message(message),
                    },
                    other => other,
                });
            }
        }
    }

    enum Outcome {
        Read(Result<(), MemoryError>),
        TimedOut,
    }
    let mut outcomes: Vec<Outcome> = participants.iter().map(|_| Outcome::Read(Ok(()))).collect();
    if !participants.is_empty() {
        let mut requests: Vec<ReadRequest<'_>> = participants
            .iter()
            .zip(buffers.iter_mut())
            .map(|(&(_, address), buffer)| ReadRequest::new(address, buffer))
            .collect();
        if memory.read_many(&mut requests).is_err() {
            if !memory.is_alive() {
                return Err(ReadPlanError::TargetGone);
            }
            drop(requests);
            let mut timed_out = false;
            for (outcome, (&(_, address), buffer)) in outcomes
                .iter_mut()
                .zip(participants.iter().zip(buffers.iter_mut()))
            {
                if timed_out || Instant::now() >= deadline {
                    if mode == DeadlineMode::RejectCall {
                        return Err(ReadPlanError::TimedOut);
                    }
                    timed_out = true;
                    *outcome = Outcome::TimedOut;
                    continue;
                }
                *outcome = Outcome::Read(memory.read_exact(address, buffer));
                if matches!(outcome, Outcome::Read(Err(_))) && !memory.is_alive() {
                    return Err(ReadPlanError::TargetGone);
                }
            }
        }
    }

    for ((index, _), (outcome, buffer)) in participants
        .into_iter()
        .zip(outcomes.into_iter().zip(buffers))
    {
        results[index] = Some(match outcome {
            Outcome::Read(Ok(())) => ReadResult::Ok(buffer),
            Outcome::Read(Err(error)) => {
                budget.recover(ops[index].length);
                memory_error_to_result(error, budget)
            }
            Outcome::TimedOut => {
                budget.recover(ops[index].length);
                timeout_result(budget)
            }
        });
    }

    Ok(results
        .into_iter()
        .map(|result| {
            result.expect("every op index is filled by either the address loop or the read loop")
        })
        .collect())
}

fn error_budget_slack(submit: &ReadPlanSubmit) -> usize {
    let op_count: usize = submit.stages.iter().map(|stage| stage.ops.len()).sum();
    let sum_lengths: u64 = submit
        .stages
        .iter()
        .flat_map(|stage| stage.ops.iter())
        .map(|op| u64::from(op.length))
        .sum();
    let used = RESULT_HEADER_SIZE
        + RESULT_ENTRY_SIZE * op_count
        + usize::try_from(sum_lengths).unwrap_or(usize::MAX);
    (VOSU_READ_PLAN_MAX_BYTES as usize).saturating_sub(used)
}

/// Executes `submit`'s whole dependent chain against `memory` in (stage,
/// op) order, honoring `deadline` once per stage, and returns the decoded
/// `VOSU_OP_READ_PLAN_RESULT` (`docs/v2/plugin-abi.md` §6.3: "one
/// submission is one round trip"). Shared by [`execute`]
/// ([`DeadlineMode::PerRead`], the async path) and [`execute_sync`]
/// ([`DeadlineMode::RejectCall`], D5's `VOSU_READ_PLAN_SYNC` path, F2):
/// in `PerRead` mode a per-read failure (an unresolved module, a
/// dependent chain blocked on an earlier failure, a [`MemoryError`], the
/// target already dead, or a deadline passed mid-plan) is reported per
/// read in the returned [`ReadPlanResult`], never by rejecting the call
/// (P1-3: an accepted asynchronous plan has no synchronous caller left to
/// report a whole-call rejection to by the time a worker discovers any
/// of these); in `RejectCall` mode the target already being dead rejects
/// the call outright with [`ReadPlanError::TargetGone`], and a deadline
/// passed mid-plan rejects it with [`ReadPlanError::TimedOut`], matching
/// D5/§6.3's whole-call errors. Either mode also rejects
/// [`ReadPlanError::RefOpWrongWidth`] (checked before any read; a plan
/// validation error, not a runtime one) outright.
fn execute_core(
    submit: &ReadPlanSubmit,
    memory: &dyn ProcessMemory,
    bitness: ProcessBitness,
    modules: &dyn ModuleResolver,
    deadline: Instant,
    mode: DeadlineMode,
) -> Result<ReadPlanResult, ReadPlanError> {
    let pointer_width = u32::try_from(bitness.pointer_size()).unwrap_or(u32::MAX);
    if !ref_op_width_ok(submit, pointer_width) {
        return Err(ReadPlanError::RefOpWrongWidth);
    }

    let mut budget = ErrorBudget::new(error_budget_slack(submit));

    if !memory.reads_pin_process_identity() && !memory.is_alive() {
        return match mode {
            DeadlineMode::PerRead => Ok(not_ready_result(submit)),
            DeadlineMode::RejectCall => Err(ReadPlanError::TargetGone),
        };
    }

    let root_base = resolve_root(&submit.root, modules);
    let mut per_stage: Vec<Vec<ReadResult>> = Vec::with_capacity(submit.stages.len());

    for (index, stage) in submit.stages.iter().enumerate() {
        if Instant::now() >= deadline {
            if mode == DeadlineMode::RejectCall {
                return Err(ReadPlanError::TimedOut);
            }
            for remaining in &submit.stages[index..] {
                per_stage.push(
                    remaining
                        .ops
                        .iter()
                        .map(|op| {
                            budget.recover(op.length);
                            timeout_result(&mut budget)
                        })
                        .collect(),
                );
            }
            return Ok(ReadPlanResult {
                plan: submit.plan,
                results: per_stage.into_iter().flatten().collect(),
            });
        }
        let stage_base = resolve_stage_base(stage, &per_stage, &root_base, bitness);
        match execute_stage_ops(memory, &stage_base, &stage.ops, &mut budget, deadline, mode) {
            Ok(results) => per_stage.push(results),
            Err(ReadPlanError::TargetGone) if mode == DeadlineMode::PerRead => {
                return Ok(not_ready_result(submit));
            }
            Err(error) => return Err(error),
        }
    }

    Ok(ReadPlanResult {
        plan: submit.plan,
        results: per_stage.into_iter().flatten().collect(),
    })
}

/// The async (non-`VOSU_READ_PLAN_SYNC`) path's own entry point
/// (`DeadlineMode::PerRead`, `execute_core`'s own doc comment).
/// Allocates freely; prefer [`execute_into`] on a measured hot path.
pub fn execute(
    submit: &ReadPlanSubmit,
    memory: &dyn ProcessMemory,
    bitness: ProcessBitness,
    modules: &dyn ModuleResolver,
    deadline: Instant,
) -> Result<ReadPlanResult, ReadPlanError> {
    execute_core(
        submit,
        memory,
        bitness,
        modules,
        deadline,
        DeadlineMode::PerRead,
    )
}

/// `VOSU_READ_PLAN_SYNC`'s own entry point (D5, §6.3, F2):
/// [`DeadlineMode::RejectCall`], `execute_core`'s own doc comment —
/// unlike [`execute`], a dead target or a deadline passed mid-plan
/// rejects the whole call instead of answering per read, since a
/// synchronous submission still has the caller to report that to.
fn execute_sync(
    submit: &ReadPlanSubmit,
    memory: &dyn ProcessMemory,
    bitness: ProcessBitness,
    modules: &dyn ModuleResolver,
    deadline: Instant,
) -> Result<ReadPlanResult, ReadPlanError> {
    execute_core(
        submit,
        memory,
        bitness,
        modules,
        deadline,
        DeadlineMode::RejectCall,
    )
}

mod deadline;
mod raw;
pub(crate) use raw::ValidatedPlan;
pub use raw::{execute_bytes, execute_into, execute_sync_into};

thread_local! {
    /// Set for the duration of this instance's own serialized `dispatch`
    /// call (or any other thread the host itself dispatches onto, e.g. the
    /// state-bus publisher), by whoever runs that call
    /// ([`DispatchThreadScope::enter`]). A `VOSU_READ_PLAN_SYNC`
    /// submission arriving while this flag is set is refused rather than
    /// either deadlocking (a dedicated per-instance worker thread) or
    /// running reentrantly out of the serialized order
    /// (`docs/v2/plugin-abi.md` §4, §6.3).
    static ON_DISPATCH_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// RAII marker a dispatch worker enters for the duration of one `dispatch`
/// call (or the state-bus publisher enters for the duration of one
/// publication), so [`deliver_sync`] can refuse a `VOSU_READ_PLAN_SYNC`
/// submission arriving on that same thread. Restores the thread-local's
/// previous value on drop (rather than unconditionally clearing it), so a
/// scope entered inside another scope leaves its enclosing scope's state
/// intact once it exits. Wiring this into `SerialWorker`/`WorkerPool`'s own
/// job-running code is later wiring; this type only provides the marker.
#[must_use]
pub struct DispatchThreadScope(bool);

impl DispatchThreadScope {
    pub fn enter() -> Self {
        let previous = ON_DISPATCH_THREAD.with(|flag| flag.replace(true));
        Self(previous)
    }
}

impl Drop for DispatchThreadScope {
    fn drop(&mut self) {
        ON_DISPATCH_THREAD.with(|flag| flag.set(self.0));
    }
}

/// Whether the calling thread is currently inside a [`DispatchThreadScope`].
#[must_use]
pub fn on_dispatch_thread() -> bool {
    ON_DISPATCH_THREAD.with(std::cell::Cell::get)
}

/// Every way a `VOSU_READ_PLAN_SYNC` submission can be refused or fail
/// before/instead of delivering a result (D5, §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncReadPlanError {
    /// "A plugin submits a synchronous plan from its own worker thread
    /// only: submitting one from any thread the host itself dispatches
    /// onto … is rejected with `VOSU_ERR_INVALID_ARGUMENT` in both
    /// runtimes" (`docs/v2/plugin-abi.md` §6.3).
    OnDispatchThread,
    Execute(ReadPlanError),
}

impl From<SyncReadPlanError> for vosu_status {
    fn from(error: SyncReadPlanError) -> Self {
        match error {
            SyncReadPlanError::OnDispatchThread => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            SyncReadPlanError::Execute(error) => error.into(),
        }
    }
}

/// Delivers a `VOSU_READ_PLAN_SYNC` submission's result on the calling
/// (submitting) thread, before returning — exactly what `send` must do
/// for this one opcode flag (D5): refuses before executing anything if the
/// calling thread is itself a dispatch thread ([`on_dispatch_thread`]),
/// otherwise runs `execute_sync` directly, inline, under `deadline`
/// (F2: whole-call `VOSU_ERR_TARGET_GONE`/`VOSU_ERR_TIMED_OUT`, never the
/// async path's per-read conversion). `VOSU_READ_PLAN_SYNC` submissions
/// never count against [`OutstandingPlans`]'s cap: the result is
/// delivered before `send` returns, so it is never "outstanding".
pub fn deliver_sync(
    submit: &ReadPlanSubmit,
    memory: &dyn ProcessMemory,
    bitness: ProcessBitness,
    modules: &dyn ModuleResolver,
    deadline: Instant,
) -> Result<ReadPlanResult, SyncReadPlanError> {
    if on_dispatch_thread() {
        return Err(SyncReadPlanError::OnDispatchThread);
    }
    execute_sync(submit, memory, bitness, modules, deadline).map_err(SyncReadPlanError::Execute)
}

/// Whether a just-finished plan's result should still be delivered, or was
/// cancelled meanwhile (`docs/v2/plugin-abi.md` §6.3): "if the result was
/// already in flight when the cancellation arrived, the plugin's
/// `dispatch` returns `VOSU_ERR_CANCELLED` for the late
/// `VOSU_OP_READ_PLAN_RESULT` rather than treating it as valid".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishOutcome {
    Deliver,
    Cancelled,
}

/// Outcome of [`OutstandingPlans::set_queue_ticket`] (N3): whether `plan`
/// is still outstanding (the ticket was recorded normally) or was
/// cancelled before this call could run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetQueueTicketOutcome {
    /// Recorded normally; `plan` is still outstanding.
    Recorded,
    /// `plan` was no longer outstanding by the time this ran — either a
    /// `READ_PLAN_CANCEL`/detach raced [`OutstandingPlans::begin`] and
    /// won before this call reached the lock, or (rarer: the job itself
    /// would have to run and call [`OutstandingPlans::finish`] in that
    /// same window) the plan already finished. Either way there is no
    /// ticket left to track; the caller removes the job from the pool
    /// itself using the `ticket` it already has, which is a safe no-op
    /// if the job already ran.
    CancelledMeanwhile,
}

/// Every way admitting a new asynchronous plan can be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutstandingPlansError {
    /// This instance already has [`MAX_OUTSTANDING_ASYNC_PLANS`]
    /// asynchronous plans awaiting their result.
    CapExceeded,
    /// `plan`'s id is already outstanding (or was cancelled and has not
    /// yet `finish`ed): re-submitting the same id would otherwise let a
    /// plugin keep any number of plans in flight under one id, bypassing
    /// the cap entirely.
    AlreadyOutstanding,
}

impl From<OutstandingPlansError> for vosu_status {
    fn from(_: OutstandingPlansError) -> Self {
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    }
}

#[derive(Debug, Default)]
struct PlansState {
    /// plan id -> the session handle it was submitted under.
    outstanding: HashMap<vosu_plan_id, u64>,
    /// Cancelled ids no longer count toward the cap (§6.3: a cancelled
    /// slot "is freed the moment" `READ_PLAN_CANCEL` arrives) but are
    /// still tracked here until `finish` so a late result can still be
    /// reported [`FinishOutcome::Cancelled`].
    cancelled: HashSet<vosu_plan_id>,
    /// plan id -> its still-queued `ScanWorkerPool` job ticket (F1), set
    /// once `host/read_plans.rs::submit_read_plan_async` has actually
    /// queued it. [`OutstandingPlans::cancel`]/[`OutstandingPlans::cancel_session`]
    /// hand this back so the host layer can remove the job from the
    /// pool's own per-key sub-queue immediately, instead of leaving a
    /// cancelled job to pile up there until a worker happens to reach
    /// it.
    queue_tickets: HashMap<vosu_plan_id, u64>,
    /// M5 B0: each admitted plan's reserved result delivery, released the
    /// moment its slot is (cancel, detach) or handed to the delivering job.
    result_slots: HashMap<vosu_plan_id, super::outbox::OutboxSlot>,
}

/// Tracks one plugin instance's asynchronous (non-`VOSU_READ_PLAN_SYNC`)
/// read plans: the [`MAX_OUTSTANDING_ASYNC_PLANS`] cap (§5, §6.3), which
/// session handle each belongs to, and `VOSU_OP_READ_PLAN_CANCEL`'s
/// "discard the result instead of delivering it" rule.
#[derive(Debug, Default)]
pub struct OutstandingPlans {
    state: Mutex<PlansState>,
}

impl OutstandingPlans {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PlansState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admits `plan` (submitted under `handle`'s session) as outstanding,
    /// refusing a submission that would exceed
    /// [`MAX_OUTSTANDING_ASYNC_PLANS`] or reuse an id that is already
    /// outstanding, before any read executes.
    pub fn begin(&self, plan: vosu_plan_id, handle: u64) -> Result<(), OutstandingPlansError> {
        self.admit(plan, handle, None)
    }

    /// [`Self::begin`], holding `result` — the plan's reserved delivery —
    /// until [`Self::finish_with_result`] hands it back, or a cancel or
    /// detach frees the plan's slot and drops it with it (§6.3).
    pub(crate) fn begin_with_result(
        &self,
        plan: vosu_plan_id,
        handle: u64,
        result: super::outbox::OutboxSlot,
    ) -> Result<(), OutstandingPlansError> {
        self.admit(plan, handle, Some(result))
    }

    fn admit(
        &self,
        plan: vosu_plan_id,
        handle: u64,
        result: Option<super::outbox::OutboxSlot>,
    ) -> Result<(), OutstandingPlansError> {
        let mut state = self.lock();
        if state.outstanding.contains_key(&plan) || state.cancelled.contains(&plan) {
            return Err(OutstandingPlansError::AlreadyOutstanding);
        }
        if state.outstanding.len() >= MAX_OUTSTANDING_ASYNC_PLANS {
            return Err(OutstandingPlansError::CapExceeded);
        }
        state.outstanding.insert(plan, handle);
        if let Some(result) = result {
            state.result_slots.insert(plan, result);
        }
        Ok(())
    }

    /// F1: records `plan`'s pool job ticket once it has actually been
    /// queued. N3: a `READ_PLAN_CANCEL`/detach can race this (arriving
    /// after [`Self::begin`] but before this call reaches the lock), in
    /// which case `plan` is already in `cancelled`, not `outstanding` —
    /// reported as `SetQueueTicketOutcome::CancelledMeanwhile` so the
    /// caller can still remove the now-orphaned job from the pool itself
    /// (it already has `ticket`) and `forget_cancelled` it, rather than
    /// leaving it to pile up in the pool's own per-key sub-queue until a
    /// worker happens to reach it — the same leak F1 closed, on this
    /// narrower race window.
    pub fn set_queue_ticket(&self, plan: vosu_plan_id, ticket: u64) -> SetQueueTicketOutcome {
        let mut state = self.lock();
        if state.outstanding.contains_key(&plan) {
            state.queue_tickets.insert(plan, ticket);
            SetQueueTicketOutcome::Recorded
        } else {
            SetQueueTicketOutcome::CancelledMeanwhile
        }
    }

    /// `VOSU_OP_READ_PLAN_CANCEL`: the plugin no longer needs `plan`'s
    /// result. Frees `plan`'s slot immediately (it stops counting against
    /// the cap right away) while remembering it was cancelled, so
    /// [`Self::finish`] still reports [`FinishOutcome::Cancelled`] once
    /// the in-flight execution actually completes. A `plan` already
    /// finished (and so no longer tracked) is a silent no-op. Returns
    /// `plan`'s own pool job ticket, if it still had one (F1): the caller
    /// removes the job from the shared pool's own per-key sub-queue
    /// immediately, rather than leaving it to pile up there until a
    /// worker happens to reach it.
    #[must_use]
    pub fn cancel(&self, plan: vosu_plan_id) -> Option<u64> {
        let (ticket, _released_after_unlock) = {
            let mut state = self.lock();
            state.outstanding.remove(&plan)?;
            state.cancelled.insert(plan);
            (
                state.queue_tickets.remove(&plan),
                state.result_slots.remove(&plan),
            )
        };
        ticket
    }

    /// F1: the host layer calls this once it has confirmed, through
    /// [`super::scan::ScanWorkerPool::cancel`]'s own return value, that
    /// `plan`'s pool job was still queued and so was removed before it
    /// could ever run — meaning its job closure's own [`Self::finish`]
    /// call (the only other thing that ever clears a `cancelled` entry)
    /// will now never happen. Without this, every plan cancelled while
    /// still queued would leak its own entry in `cancelled` forever.
    pub fn forget_cancelled(&self, plan: vosu_plan_id) {
        self.lock().cancelled.remove(&plan);
    }

    /// Whether `plan` was already cancelled (`VOSU_OP_READ_PLAN_CANCEL` or
    /// the owning session's detach) while it was queued or running —
    /// without resolving it the way [`Self::finish`] does (P2-7: a worker
    /// peeks this before spending time actually executing the plan, so a
    /// plugin that cancels work it no longer needs does not also cost the
    /// shared pool the time of running it).
    #[must_use]
    pub fn is_cancelled(&self, plan: vosu_plan_id) -> bool {
        self.lock().cancelled.contains(&plan)
    }

    /// `SESSION_DETACHED` (A1): cancels every plan still outstanding under
    /// `handle`, exactly as if the plugin had sent `READ_PLAN_CANCEL` for
    /// each one. Returns every freed plan's own id paired with its pool
    /// job ticket, for each one that still had one (F1): the caller
    /// removes each job from the shared pool's own per-key sub-queue
    /// immediately, and [`Self::forget_cancelled`]s it in turn if that
    /// removal actually found it still queued, for the same reason
    /// [`Self::cancel`] does.
    #[must_use]
    pub fn cancel_session(&self, handle: u64) -> Vec<(vosu_plan_id, u64)> {
        let mut released = Vec::new();
        let mut state = self.lock();
        let ids: Vec<vosu_plan_id> = state
            .outstanding
            .iter()
            .filter(|&(_, owner)| *owner == handle)
            .map(|(&id, _)| id)
            .collect();
        let mut tickets = Vec::new();
        for id in ids {
            state.outstanding.remove(&id);
            state.cancelled.insert(id);
            released.extend(state.result_slots.remove(&id));
            if let Some(ticket) = state.queue_tickets.remove(&id) {
                tickets.push((id, ticket));
            }
        }
        drop(state);
        drop(released);
        tickets
    }

    /// [`Self::finish`], handing back the reserved delivery
    /// [`Self::begin_with_result`] took when the result is to be delivered.
    pub(crate) fn finish_with_result(
        &self,
        plan: vosu_plan_id,
    ) -> Option<super::outbox::OutboxSlot> {
        let slot = self.lock().result_slots.remove(&plan);
        match self.finish(plan) {
            FinishOutcome::Deliver => slot,
            FinishOutcome::Cancelled => None,
        }
    }

    /// Call once `plan`'s execution has actually finished: removes it from
    /// the outstanding/cancelled set and reports whether its result should
    /// still be delivered or was cancelled meanwhile. N1: also removes
    /// `plan`'s own `queue_tickets` entry if it still had one — before
    /// this, a plan that ran to completion normally (never cancelled, so
    /// never freed by [`Self::cancel`]/[`Self::cancel_session`] either)
    /// left its ticket behind here forever: every async plan a plugin
    /// ever submits and lets finish normally, "many times per second per
    /// session" (`docs/v2/implementation-plan.md` section 10) on the hot
    /// path, grew this map without bound for the life of the instance.
    pub fn finish(&self, plan: vosu_plan_id) -> FinishOutcome {
        let mut state = self.lock();
        state.queue_tickets.remove(&plan);
        if state.cancelled.remove(&plan) {
            FinishOutcome::Cancelled
        } else {
            state.outstanding.remove(&plan);
            FinishOutcome::Deliver
        }
    }

    /// Current outstanding count (cancelled ids excluded), for tests and
    /// diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().outstanding.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Current `cancelled`-set size, for tests and diagnostics (N1):
    /// every entry here must eventually clear through [`Self::finish`]
    /// or [`Self::forget_cancelled`], never accumulate for the life of
    /// the instance.
    #[must_use]
    pub fn cancelled_len(&self) -> usize {
        self.lock().cancelled.len()
    }

    /// Current `queue_tickets` map size, for tests and diagnostics (N1):
    /// every entry here must eventually clear through [`Self::finish`],
    /// [`Self::cancel`], or [`Self::cancel_session`], never accumulate
    /// for the life of the instance.
    #[must_use]
    pub fn queue_tickets_len(&self) -> usize {
        self.lock().queue_tickets.len()
    }
}

#[cfg(test)]
mod tests;
