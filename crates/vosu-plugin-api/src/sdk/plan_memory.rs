//! `PlanMemory`: opt-in synchronous reads (D5), behind the unpublished
//! `unstable-sync-reads` feature.
//!
//! `VOSU_READ_PLAN_SYNC` makes the host deliver the matching
//! `VOSU_OP_READ_PLAN_RESULT` back on the submitting thread itself, before
//! `send` returns (`docs/v2/plugin-abi.md` §6.3) — for an in-process
//! plugin, by calling this same SDK's `dispatch` glue directly, reentrant
//! on this very call stack. `PlanMemory` routes that one message to a
//! thread-local slot before the plugin's own `on_message` ever sees it
//! (the dispatch glue checks this slot first), then retrieves it here once
//! `send` returns — exactly "the SDK routes flagged results to a per-plan
//! slot before user code sees them" (`docs/v2/implementation-plan.md`
//! D5). It refuses to run from a thread the ordinary, serialized
//! `on_message` is itself running on (the dispatch glue marks that thread
//! for the call's duration): submitting a synchronous plan there would
//! either deadlock (a `process`-runtime plugin's single dispatch thread)
//! or at best race the host's own documented refusal
//! (`include/vosu_plugin.h:396-399`).

use std::cell::Cell;

use crate::message::HostServices;
use crate::message::flat::ReadPlanResult;
use crate::{PlanBase, Stage, vosu_plan_id, vosu_session_id, vosu_status};

thread_local! {
    /// Set for the duration of an ordinary (non-synchronous) dispatch call
    /// on this thread, by the SDK's dispatch glue.
    pub(crate) static ON_DISPATCH_THREAD: Cell<bool> = const { Cell::new(false) };
    /// The plan this thread is waiting on a synchronous result for, if any.
    static EXPECTING_PLAN: Cell<Option<u64>> = const { Cell::new(None) };
    /// Where the dispatch glue stashes that result once it arrives,
    /// instead of forwarding it to the plugin's own `on_message`.
    static SYNC_SLOT: Cell<Option<ReadPlanResult>> = const { Cell::new(None) };
}

/// Called by the SDK's dispatch glue before forwarding a message to the
/// plugin: if this thread is waiting on `result.plan`'s synchronous
/// result, stashes it and reports "handled" (the plugin's own
/// `on_message` never sees it); otherwise reports "not mine" so dispatch
/// proceeds normally.
pub(crate) fn intercept(result: &ReadPlanResult) -> bool {
    let expected = EXPECTING_PLAN.with(Cell::get);
    if expected != Some(result.plan) {
        return false;
    }
    SYNC_SLOT.with(|slot| slot.set(Some(result.clone())));
    true
}

/// Every way [`PlanMemory::read`] can fail without ever submitting a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanMemoryError {
    /// This call would run on the host's own serialized dispatch thread
    /// (`docs/v2/plugin-abi.md` §4); a plugin's own reader thread is
    /// required instead.
    OnDispatchThread,
    /// `send` rejected the submission itself.
    Rejected(vosu_status),
    /// `send` returned `VOSU_OK` but no synchronous result arrived — this
    /// SDK's own bug, since the host guarantees delivery before `send`
    /// returns, or `VOSU_ERR_TIMED_OUT` otherwise.
    NoResult,
}

/// A plugin's own reader-thread handle for submitting a `VOSU_READ_PLAN_SYNC`
/// read plan and blocking for its result inline (D5).
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanMemory;

impl PlanMemory {
    pub fn new() -> Self {
        Self
    }

    /// Submits `root`/`stages` as plan `plan` for `handle` with
    /// `VOSU_READ_PLAN_SYNC` set, and returns its result once `host.send`
    /// returns — on this calling thread, never the host's own dispatch
    /// thread (refused with [`PlanMemoryError::OnDispatchThread`] before
    /// even submitting, mirroring the host's own refusal,
    /// `include/vosu_plugin.h:396-399`).
    pub fn read(
        &self,
        host: &impl HostServices,
        plan: vosu_plan_id,
        handle: vosu_session_id,
        root: PlanBase,
        stages: Vec<Stage>,
    ) -> Result<ReadPlanResult, PlanMemoryError> {
        if ON_DISPATCH_THREAD.with(Cell::get) {
            return Err(PlanMemoryError::OnDispatchThread);
        }
        EXPECTING_PLAN.with(|cell| cell.set(Some(plan)));
        SYNC_SLOT.with(|slot| slot.set(None));
        let status = host.submit_read_plan(plan, handle, root, stages, true);
        EXPECTING_PLAN.with(|cell| cell.set(None));
        if status != vosu_status::VOSU_OK {
            return Err(PlanMemoryError::Rejected(status));
        }
        SYNC_SLOT
            .with(|slot| slot.take())
            .ok_or(PlanMemoryError::NoResult)
    }
}
