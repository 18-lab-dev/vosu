//! Read-plan executor (`docs/v2/plugin-abi.md` §6.3) and the opt-in
//! synchronous-delivery mechanism (D5, §4): executes one decoded
//! `VOSU_OP_READ_PLAN_SUBMIT` against a `vosu_sys::process::ProcessMemory`
//! backend, honoring a deadline, and produces the matching
//! `VOSU_OP_READ_PLAN_RESULT`.
//!
//! This module is deliberately standalone: it knows nothing about
//! `PluginInstanceId`, the session registry, or `PluginHostHandler`. Wiring
//! it into `PluginHostHandler::handle` — routing a decoded
//! `Message::ReadPlanSubmit` here with the right session's `ProcessMemory`
//! and `ProcessBitness`, tracking [`OutstandingPlans`] per instance,
//! delivering a [`deliver_sync`] result back through `send`'s own return
//! value, resolving `PlanBase::Module` against the session's real module
//! list, and answering `VOSU_OP_READ_PLAN_CANCEL` — is later wiring, not
//! this phase's scope.
//!
//! `vosu_plugin_api::validate_read_plan_ref_op_width` needs the plan's raw
//! encoded buffer, which `Message::decode` (run once, by the caller,
//! before a [`ReadPlanSubmit`] ever reaches this module) already consumed;
//! [`ref_op_width_ok`] re-derives the same bitness-aware check directly
//! against the decoded [`Stage`]/[`PlanOp`] values instead.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use vosu_plugin_api::{
    Message, MessageError, PlanBase, PlanOp, ReadErrorCode, ReadPlanResult, ReadPlanSubmit,
    ReadResult, Stage, StageBase, vosu_opcode, vosu_plan_id, vosu_status,
};
use vosu_sys::process::{MemoryError, ProcessBitness, ProcessMemory};

/// Per-instance bound on asynchronous (non-`VOSU_READ_PLAN_SYNC`) read
/// plans outstanding at once (`docs/v2/plugin-abi.md` §5, §6.3): a
/// `READ_PLAN_SUBMIT` beyond it is refused with `VOSU_ERR_INVALID_ARGUMENT`
/// before any read executes.
pub const MAX_OUTSTANDING_ASYNC_PLANS: usize = 64;

/// Caps the byte length of an error message embedded in a
/// [`ReadResult::Error`]. `vosu_plugin_api::validate_read_plan_submit`'s
/// pre-read worst-case `VOSU_READ_PLAN_MAX_BYTES` bound
/// (`crates/vosu-plugin-api/src/validate.rs`) assumes every op's reply is
/// at most its declared `length` plus a fixed per-result header; it does
/// not itself bound an error message's length, so a verbose
/// `MemoryError::Display` is truncated here rather than left free to grow
/// an otherwise within-bound result past that cap.
const MAX_ERROR_MESSAGE_BYTES: usize = 256;

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

/// Every way [`execute`] can reject a submission outright (no partial
/// result; `docs/v2/plugin-abi.md` §5: "a non-`VOSU_OK` status means the
/// call itself was rejected").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadPlanError {
    /// A dependent stage's `ref_op` names an op whose declared `length`
    /// does not equal the session's pointer width — a plan validation
    /// error, not a silent truncation or zero-extension (§6.3).
    RefOpWrongWidth,
    /// Execution exceeded its deadline (§5 `VOSU_ERR_TIMED_OUT`).
    TimedOut,
}

impl From<ReadPlanError> for vosu_status {
    fn from(error: ReadPlanError) -> Self {
        match error {
            ReadPlanError::RefOpWrongWidth => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            ReadPlanError::TimedOut => vosu_status::VOSU_ERR_TIMED_OUT,
        }
    }
}

/// Every way [`execute_bytes`] can fail, layering [`ReadPlanError`] over
/// `Message`'s own decode/encode failures.
#[derive(Debug)]
pub enum ExecuteBytesError {
    /// `payload`/`flags` did not decode as a `VOSU_OP_READ_PLAN_SUBMIT`, or
    /// the produced `VOSU_OP_READ_PLAN_RESULT` failed to encode (the
    /// latter should not happen for a well-formed submission; see
    /// [`execute`]'s doc comment on `MAX_ERROR_MESSAGE_BYTES`).
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
/// against the decoded form: every dependent stage's `ref_op` must name an
/// earlier stage's op whose declared `length` equals `pointer_width`.
/// `Message::decode` already bounds-checked every `ref_stage`/`ref_op`
/// index through `vosu_plugin_api::validate_read_plan_submit`, so indexing
/// here never panics.
fn ref_op_width_ok(submit: &ReadPlanSubmit, pointer_width: u32) -> bool {
    submit.stages.iter().all(|stage| match stage.base {
        StageBase::Root => true,
        StageBase::Dependent { ref_stage, ref_op } => {
            submit.stages[ref_stage as usize].ops[ref_op as usize].length == pointer_width
        }
    })
}

fn truncate_message(mut message: String) -> String {
    if message.len() > MAX_ERROR_MESSAGE_BYTES {
        let mut end = MAX_ERROR_MESSAGE_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
    }
    message
}

fn memory_error_to_result(error: MemoryError) -> ReadResult {
    let code = match &error {
        MemoryError::Backend { .. } => ReadErrorCode::Backend,
        MemoryError::PermissionDenied { .. } => ReadErrorCode::PermissionDenied,
        MemoryError::Read { .. } => ReadErrorCode::ReadFailed,
        MemoryError::AddressOverflow { .. } => ReadErrorCode::AddressOverflow,
        // `ProcessMemory::read_exact`/`read_many` never produce these two
        // (they decode the osu!-specific managed heap, a layer above the
        // raw read-plan boundary); mapped defensively rather than panicking.
        MemoryError::Corrupt { .. } | MemoryError::InvalidUtf16 { .. } => ReadErrorCode::Backend,
    };
    ReadResult::Error {
        code,
        message: truncate_message(error.to_string()),
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
/// `bitness.pointer_size()` by [`ref_op_width_ok`]) as a little-endian
/// pointer-width value.
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

fn read_one(memory: &dyn ProcessMemory, base: u64, op: &PlanOp) -> ReadResult {
    let address = match checked_add_signed(base, op.offset) {
        Ok(address) => address,
        Err(failure) => return failure,
    };
    let Ok(address) = usize::try_from(address) else {
        return ReadResult::Error {
            code: ReadErrorCode::AddressOverflow,
            message: format!("address {address:#x} exceeds this platform's usize range"),
        };
    };
    let mut buffer = vec![0u8; op.length as usize];
    match memory.read_exact(address, &mut buffer) {
        Ok(()) => ReadResult::Ok(buffer),
        Err(error) => memory_error_to_result(error),
    }
}

/// Executes `submit`'s whole dependent chain against `memory` in (stage,
/// op) order, honoring `deadline` before every individual read, and
/// returns the decoded `VOSU_OP_READ_PLAN_RESULT` (`docs/v2/plugin-abi.md`
/// §6.3: "one submission is one round trip"). A per-read failure (an
/// unresolved module, a dependent chain blocked on an earlier failure, or
/// a [`MemoryError`]) is reported per read in the returned
/// [`ReadPlanResult`], never by rejecting the call; only
/// [`ReadPlanError::RefOpWrongWidth`] (checked before any read) and
/// [`ReadPlanError::TimedOut`] (checked before each read) reject the whole
/// submission.
pub fn execute(
    submit: &ReadPlanSubmit,
    memory: &dyn ProcessMemory,
    bitness: ProcessBitness,
    modules: &dyn ModuleResolver,
    deadline: Instant,
) -> Result<ReadPlanResult, ReadPlanError> {
    let pointer_width = u32::try_from(bitness.pointer_size()).unwrap_or(u32::MAX);
    if !ref_op_width_ok(submit, pointer_width) {
        return Err(ReadPlanError::RefOpWrongWidth);
    }

    let root_base = resolve_root(&submit.root, modules);
    let mut per_stage: Vec<Vec<ReadResult>> = Vec::with_capacity(submit.stages.len());

    for stage in &submit.stages {
        let stage_base = resolve_stage_base(stage, &per_stage, &root_base, bitness);
        let mut ops = Vec::with_capacity(stage.ops.len());
        for op in &stage.ops {
            if Instant::now() >= deadline {
                return Err(ReadPlanError::TimedOut);
            }
            let result = match &stage_base {
                Ok(base) => read_one(memory, *base, op),
                Err(failure) => failure.clone(),
            };
            ops.push(result);
        }
        per_stage.push(ops);
    }

    Ok(ReadPlanResult {
        plan: submit.plan,
        results: per_stage.into_iter().flatten().collect(),
    })
}

/// Byte-oriented entry point: decodes `payload`/`flags` as a
/// `VOSU_OP_READ_PLAN_SUBMIT`, executes it, and encodes the matching
/// `VOSU_OP_READ_PLAN_RESULT` payload. `vosu_plugin_api::message::flat`'s
/// own codec functions are private to that crate (only `Message`'s own
/// `decode`/`encode_payload` are public), so this is the one route from
/// raw bytes to a raw result this crate can reach.
pub fn execute_bytes(
    payload: &[u8],
    flags: u32,
    memory: &dyn ProcessMemory,
    bitness: ProcessBitness,
    modules: &dyn ModuleResolver,
    deadline: Instant,
) -> Result<Vec<u8>, ExecuteBytesError> {
    let message = Message::decode(vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT, flags, payload)
        .map_err(ExecuteBytesError::Message)?;
    let Message::ReadPlanSubmit(submit) = message else {
        unreachable!("VOSU_OP_READ_PLAN_SUBMIT always decodes to Message::ReadPlanSubmit")
    };
    let result =
        execute(&submit, memory, bitness, modules, deadline).map_err(ExecuteBytesError::Execute)?;
    Message::ReadPlanResult(result)
        .encode_payload()
        .map_err(ExecuteBytesError::Message)
}

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
/// submission arriving on that same thread. Wiring this into
/// `SerialWorker`/`WorkerPool`'s own job-running code is later wiring;
/// this type only provides the marker.
#[must_use]
pub struct DispatchThreadScope(());

impl DispatchThreadScope {
    pub fn enter() -> Self {
        ON_DISPATCH_THREAD.with(|flag| flag.set(true));
        Self(())
    }
}

impl Drop for DispatchThreadScope {
    fn drop(&mut self) {
        ON_DISPATCH_THREAD.with(|flag| flag.set(false));
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
/// otherwise runs [`execute`] directly, inline, under `deadline`.
/// `VOSU_READ_PLAN_SYNC` submissions never count against
/// [`OutstandingPlans`]'s cap: the result is delivered before `send`
/// returns, so it is never "outstanding".
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
    execute(submit, memory, bitness, modules, deadline).map_err(SyncReadPlanError::Execute)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanState {
    Outstanding,
    Cancelled,
}

/// Every way admitting a new asynchronous plan can be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutstandingPlansError {
    /// This instance already has [`MAX_OUTSTANDING_ASYNC_PLANS`]
    /// asynchronous plans awaiting their result.
    CapExceeded,
}

impl From<OutstandingPlansError> for vosu_status {
    fn from(_: OutstandingPlansError) -> Self {
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    }
}

/// Tracks one plugin instance's asynchronous (non-`VOSU_READ_PLAN_SYNC`)
/// read plans: the [`MAX_OUTSTANDING_ASYNC_PLANS`] cap (§5, §6.3), and
/// `VOSU_OP_READ_PLAN_CANCEL`'s "discard the result instead of delivering
/// it" rule.
#[derive(Debug, Default)]
pub struct OutstandingPlans {
    state: Mutex<HashMap<vosu_plan_id, PlanState>>,
}

impl OutstandingPlans {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<vosu_plan_id, PlanState>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admits `plan` as outstanding, refusing a submission that would
    /// exceed [`MAX_OUTSTANDING_ASYNC_PLANS`] before any read executes.
    pub fn begin(&self, plan: vosu_plan_id) -> Result<(), OutstandingPlansError> {
        let mut state = self.lock();
        if state.len() >= MAX_OUTSTANDING_ASYNC_PLANS {
            return Err(OutstandingPlansError::CapExceeded);
        }
        state.insert(plan, PlanState::Outstanding);
        Ok(())
    }

    /// `VOSU_OP_READ_PLAN_CANCEL`: the plugin no longer needs `plan`'s
    /// result. A `plan` already finished (and so no longer tracked) is a
    /// silent no-op — there is nothing left to discard.
    pub fn cancel(&self, plan: vosu_plan_id) {
        if let Some(entry) = self.lock().get_mut(&plan) {
            *entry = PlanState::Cancelled;
        }
    }

    /// Call once `plan`'s execution has actually finished: removes it from
    /// the outstanding set and reports whether its result should still be
    /// delivered or was cancelled meanwhile.
    pub fn finish(&self, plan: vosu_plan_id) -> FinishOutcome {
        match self.lock().remove(&plan) {
            Some(PlanState::Cancelled) => FinishOutcome::Cancelled,
            Some(PlanState::Outstanding) | None => FinishOutcome::Deliver,
        }
    }

    /// Current outstanding count, for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests;
