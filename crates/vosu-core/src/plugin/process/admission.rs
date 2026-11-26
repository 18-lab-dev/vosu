//! The plugin host's local admission for the per-instance outstanding
//! request caps (`docs/v2/plugin-abi.md` §5, §6.3, §6.4, §6.11, §6.12,
//! §7.2, §7.6 "Local refusal").
//!
//! Each cap is local to one instance, so the plugin host enforces it
//! itself before writing: a submission that would exceed 64 asynchronous
//! read plans, 64 scans, 64 cross-plugin action calls or 64 timers is
//! refused with `VOSU_ERR_INVALID_ARGUMENT` and never reaches the ring.
//! Core stays authoritative and applies the same caps again.
//!
//! The count mirrors core's own accounting, conservatively:
//!
//! - a slot is reserved before the frame is written and released again if
//!   core's `STATUS` is not `VOSU_OK` (a refused request produces no
//!   result, so nothing else would release it);
//! - an asynchronous plan's slot is freed by its `READ_PLAN_RESULT`, an
//!   accepted `READ_PLAN_CANCEL` (core frees the slot at once, §6.3), or
//!   its session's `SESSION_DETACHED` or accepted `SESSION_DECLINE`;
//! - a scan's slot by its `SCAN_RESULT` (same correlation id) or its
//!   session's detach or decline (§6.4: the only scan cancellation);
//! - an action call's slot only by its relayed `ACTION_RESULT` (§6.12:
//!   core always produces one);
//! - a timer by an accepted `TIMER_CANCEL`, or by its `TIMER_FIRED` when
//!   it is one-shot.
//!
//! Every release happens on the ring reader before the frame that caused
//! it is queued for `dispatch`, so a plugin that sees a result can submit
//! again at once. Plan ids and scan correlations are plugin-chosen and may
//! repeat, so they are counted, never refused as duplicates here: core
//! alone knows whether an id is still reserved.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use vosu_plugin_api::{Message, vosu_opcode, vosu_status};

use crate::plugin::read_plan::MAX_OUTSTANDING_ASYNC_PLANS;
use crate::plugin::scan::MAX_OUTSTANDING_SCANS;
use crate::plugin::timers::MAX_TIMERS_PER_INSTANCE;

/// §6.12: outstanding `ACTION_CALL`s per calling instance.
pub(crate) const MAX_OUTSTANDING_CALLS: usize = 64;

/// One admitted slot, held until core's status says whether it stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ticket {
    Plan {
        plan: u64,
    },
    Scan {
        correlation: u64,
    },
    Call {
        invocation: u64,
    },
    /// `previous`: the timer's one-shot flag before this `TIMER_SET`, or
    /// `None` when the id was new.
    Timer {
        timer: u64,
        previous: Option<bool>,
    },
}

/// What core's `STATUS` for one acknowledged send changes locally. The
/// ring reader applies it before the waiting thread sees the status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Settle {
    Nothing,
    /// Kept on `VOSU_OK`, released on any refusal.
    Slot(Ticket),
    /// `READ_PLAN_CANCEL`: core frees the plan's slot on `VOSU_OK`.
    CancelPlan(u64),
    /// `TIMER_CANCEL`: the timer is gone on `VOSU_OK`.
    CancelTimer(u64),
    /// `SESSION_DECLINE`: core cancels the session's plans and scans.
    Decline(u64),
    /// `STATE_SUBSCRIBE` naming a session: on `VOSU_OK` core replaced the
    /// subscription and clears what it had not yet delivered for it, so
    /// the delivery queue does the same (`super::delivery`).
    Resubscribe(u64),
}

/// A host → plugin frame's effect on admission, observed by the ring
/// reader before the frame is queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Release {
    Nothing,
    /// An asynchronous `READ_PLAN_RESULT` for `plan`.
    Plan(u64),
    /// A `SCAN_RESULT` with this correlation id.
    Scan(u64),
    /// A relayed `ACTION_RESULT` for this caller invocation.
    Call(u64),
    /// A `TIMER_FIRED`.
    Timer(u64),
    /// A `SESSION_DETACHED` for this handle.
    Session(u64),
}

#[derive(Debug, Default)]
struct State {
    /// plan id -> (session handle, admitted submissions under that id).
    plans: HashMap<u64, (u64, usize)>,
    plan_total: usize,
    /// (correlation id, session handle), one entry per admitted scan.
    scans: Vec<(u64, u64)>,
    calls: HashMap<u64, ()>,
    /// timer id -> one-shot.
    timers: HashMap<u64, bool>,
}

impl State {
    fn release_plan(&mut self, plan: u64) {
        if let Some((_, count)) = self.plans.get_mut(&plan) {
            *count -= 1;
            self.plan_total -= 1;
            if *count == 0 {
                self.plans.remove(&plan);
            }
        }
    }

    fn release_scan(&mut self, correlation: u64) {
        if let Some(index) = self.scans.iter().position(|(id, _)| *id == correlation) {
            self.scans.swap_remove(index);
        }
    }

    fn release_session(&mut self, handle: u64) {
        let mut freed = 0;
        self.plans.retain(|_, (owner, count)| {
            let keep = *owner != handle;
            if !keep {
                freed += *count;
            }
            keep
        });
        self.plan_total -= freed;
        self.scans.retain(|(_, owner)| *owner != handle);
    }
}

#[derive(Debug, Default)]
pub(crate) struct Admission {
    state: Mutex<State>,
}

const REFUSED: vosu_status = vosu_status::VOSU_ERR_INVALID_ARGUMENT;

impl Admission {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Every critical section is a handful of map operations with no
        // call that can panic part-way.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admits one decoded asynchronous (non-`SYNC`) message about to be
    /// written, returning what its status settles, or the local refusal.
    pub(crate) fn reserve(
        &self,
        message: &Message,
        correlation: u64,
    ) -> Result<Settle, vosu_status> {
        let mut state = self.lock();
        let ticket = match message {
            Message::ReadPlanSubmit(submit) => {
                if state.plan_total >= MAX_OUTSTANDING_ASYNC_PLANS {
                    return Err(REFUSED);
                }
                state
                    .plans
                    .entry(submit.plan)
                    .or_insert((submit.handle, 0))
                    .1 += 1;
                state.plan_total += 1;
                Ticket::Plan { plan: submit.plan }
            }
            Message::ScanRequest(request) => {
                if state.scans.len() >= MAX_OUTSTANDING_SCANS {
                    return Err(REFUSED);
                }
                state.scans.push((correlation, request.session));
                Ticket::Scan { correlation }
            }
            Message::ActionCall(call) => {
                if state.calls.contains_key(&call.invocation) {
                    // A reused invocation id: core refuses it, and this
                    // instance's existing slot for that id must survive.
                    return Ok(Settle::Nothing);
                }
                if state.calls.len() >= MAX_OUTSTANDING_CALLS {
                    return Err(REFUSED);
                }
                state.calls.insert(call.invocation, ());
                Ticket::Call {
                    invocation: call.invocation,
                }
            }
            Message::TimerSet(set) => {
                let one_shot = set.interval_ms.is_none();
                let previous = state.timers.get(&set.timer).copied();
                if previous.is_none() && state.timers.len() >= MAX_TIMERS_PER_INSTANCE {
                    return Err(REFUSED);
                }
                state.timers.insert(set.timer, one_shot);
                Ticket::Timer {
                    timer: set.timer,
                    previous,
                }
            }
            Message::ReadPlanCancel(cancel) => return Ok(Settle::CancelPlan(cancel.plan)),
            Message::TimerCancel(cancel) => return Ok(Settle::CancelTimer(cancel.timer)),
            Message::SessionDecline(decline) => return Ok(Settle::Decline(decline.handle)),
            Message::StateSubscribe(subscribe) => {
                return Ok(subscribe
                    .session
                    .map_or(Settle::Nothing, Settle::Resubscribe));
            }
            _ => return Ok(Settle::Nothing),
        };
        Ok(Settle::Slot(ticket))
    }

    /// Applies core's `status` for a send admitted with `settle`.
    pub(crate) fn settle(&self, settle: &Settle, status: vosu_status) {
        let accepted = status == vosu_status::VOSU_OK;
        let mut state = self.lock();
        match *settle {
            Settle::Slot(ticket) if !accepted => match ticket {
                Ticket::Plan { plan } => state.release_plan(plan),
                Ticket::Scan { correlation } => state.release_scan(correlation),
                Ticket::Call { invocation } => {
                    state.calls.remove(&invocation);
                }
                Ticket::Timer { timer, previous } => match previous {
                    Some(one_shot) => {
                        state.timers.insert(timer, one_shot);
                    }
                    None => {
                        state.timers.remove(&timer);
                    }
                },
            },
            Settle::CancelPlan(plan) if accepted => state.release_plan(plan),
            Settle::CancelTimer(timer) if accepted => {
                state.timers.remove(&timer);
            }
            Settle::Decline(handle) if accepted => state.release_session(handle),
            _ => {}
        }
    }

    /// Applies one host → plugin frame's release (ring reader only).
    pub(crate) fn observe(&self, release: Release) {
        let mut state = self.lock();
        match release {
            Release::Nothing => {}
            Release::Plan(plan) => state.release_plan(plan),
            Release::Scan(correlation) => state.release_scan(correlation),
            Release::Call(invocation) => {
                state.calls.remove(&invocation);
            }
            Release::Timer(timer) => {
                if state.timers.get(&timer) == Some(&true) {
                    state.timers.remove(&timer);
                }
            }
            Release::Session(handle) => state.release_session(handle),
        }
    }

    /// (plans, scans, calls, timers) currently counted.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (usize, usize, usize, usize) {
        let state = self.lock();
        (
            state.plan_total,
            state.scans.len(),
            state.calls.len(),
            state.timers.len(),
        )
    }
}

/// Whether `opcode` needs core's acknowledgement at all (§7.6): every
/// plugin → host opcode except `VOSU_OP_LOG_RECORD`. Host → plugin opcodes
/// a plugin sends have no effect in-process and are never written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    NoEffect,
    Unacknowledged,
    Acknowledged,
}

pub(crate) fn route(opcode: vosu_opcode) -> Route {
    use vosu_opcode::*;
    match opcode {
        VOSU_OP_LOG_RECORD => Route::Unacknowledged,
        VOSU_OP_PUBLISH_FIELD
        | VOSU_OP_READ_PLAN_SUBMIT
        | VOSU_OP_READ_PLAN_CANCEL
        | VOSU_OP_PUBLISH_BATCH
        | VOSU_OP_SESSION_IDENTIFY
        | VOSU_OP_SESSION_DECLINE
        | VOSU_OP_SCAN_REQUEST
        | VOSU_OP_RESOURCE_RESOLVED
        | VOSU_OP_STREAM_CHUNK
        | VOSU_OP_ROOT_DECLARE
        | VOSU_OP_ACTION_CALL
        | VOSU_OP_ACTION_RESULT
        | VOSU_OP_ACTION_CANCEL
        | VOSU_OP_ACTION_PROGRESS
        | VOSU_OP_TIMER_SET
        | VOSU_OP_TIMER_CANCEL
        | VOSU_OP_STATE_SUBSCRIBE
        | VOSU_OP_ARTIFACT_DECLARE => Route::Acknowledged,
        VOSU_OP_DEMAND_CHANGED
        | VOSU_OP_READ_PLAN_RESULT
        | VOSU_OP_SESSION_ATTACHED
        | VOSU_OP_SESSION_DETACHED
        | VOSU_OP_FOCUS_CHANGED
        | VOSU_OP_SCAN_RESULT
        | VOSU_OP_RESOURCE_RESOLVE
        | VOSU_OP_TREE_LIST
        | VOSU_OP_TREE_LOOKUP
        | VOSU_OP_STREAM_READ
        | VOSU_OP_STREAM_CLOSE
        | VOSU_OP_ACTION_INVOKE
        | VOSU_OP_SETTINGS_CHANGED
        | VOSU_OP_TIMER_FIRED
        | VOSU_OP_STATE_UPDATE
        | VOSU_OP_ARTIFACT_DECLARED
        | VOSU_OP_ARTIFACT_OPEN
        | VOSU_OP_ARTIFACT_RELEASE => Route::NoEffect,
    }
}

#[cfg(test)]
mod tests;
