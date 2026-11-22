//! `HostServices`: the safe surface a `Plugin`/`ConcurrentPlugin`
//! implementation calls into (`docs/v2/plugin-abi.md` §9's `HostServices`
//! sketch). `vosu_host_services` (`crate::vosu_host_services`) exposes
//! exactly one real operation, `send`; every other method here is a
//! default method built on it, so this trait covers the whole host vtable
//! with one required method. The SDK's real implementation (commit 3)
//! wraps `*const vosu_host_services`; a test substitutes any other
//! implementation.
//!
//! Unlike the sketch's `submit_read_plan`, which returns a freshly minted
//! `PlanId`, this trait takes the plan id as a parameter: the header
//! comment on `vosu_read_plan_submit_header::plan`
//! (`include/vosu_plugin.h:405`) says it is "caller-assigned", and a
//! trait's default method has no state of its own to assign one from — an
//! SDK-level id generator, layered on top of this trait, is free to pick
//! one and still call through this same method.

use core::time::Duration;
use std::path::Path;

use crate::cbor::CborValue;
use crate::message::Message;
use crate::message::flat::{PlanBase, PublishValue, ReadPlanSubmit, Stage};
use crate::{
    ActionCall, LogLevel, LogRecord, RootDeclare, ScanRequest, Selector, SessionIdentify,
    StateSubscribe, TimerCancel, TimerSet, vosu_invocation_id, vosu_plan_id, vosu_session_id,
    vosu_status, vosu_timer_id,
};

pub trait HostServices {
    /// Sends one plugin -> host message, mirroring
    /// `vosu_host_services::send` exactly: the return value only accepts
    /// or rejects the message itself; a reply, when the opcode has one,
    /// arrives later through the plugin's own `on_message`
    /// (`docs/v2/plugin-abi.md` §3) — except a `VOSU_READ_PLAN_SYNC`
    /// submission (D5), whose delivery the SDK's own implementation
    /// handles, not this trait.
    fn send(&self, message: &Message) -> vosu_status;

    /// `VOSU_OP_PUBLISH_FIELD`.
    fn publish(
        &self,
        handle: vosu_session_id,
        ns: &str,
        pointer: &str,
        value: PublishValue,
    ) -> vosu_status {
        self.send(&Message::PublishField(crate::message::flat::PublishField {
            handle,
            ns: ns.to_owned(),
            pointer: pointer.to_owned(),
            value,
        }))
    }

    /// `VOSU_OP_SESSION_IDENTIFY` (§6.2).
    fn identify_session(
        &self,
        handle: vosu_session_id,
        game: &str,
        variant: &str,
        name: &str,
        version: &str,
        focus_priority: bool,
    ) -> vosu_status {
        self.send(&Message::SessionIdentify(SessionIdentify {
            handle,
            game: game.to_owned(),
            variant: variant.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            focus_priority,
        }))
    }

    /// `VOSU_OP_ROOT_DECLARE` (§6.5).
    fn declare_root(
        &self,
        handle: Option<vosu_session_id>,
        name: &str,
        path: &Path,
    ) -> vosu_status {
        self.send(&Message::RootDeclare(RootDeclare {
            handle,
            name: name.to_owned(),
            path: path.to_string_lossy().into_owned(),
        }))
    }

    /// `VOSU_OP_READ_PLAN_SUBMIT` (§6.3). `plan` is caller-assigned by the
    /// caller of this method, never generated here.
    fn submit_read_plan(
        &self,
        plan: vosu_plan_id,
        handle: vosu_session_id,
        root: PlanBase,
        stages: Vec<Stage>,
        sync: bool,
    ) -> vosu_status {
        self.send(&Message::ReadPlanSubmit(ReadPlanSubmit {
            plan,
            handle,
            root,
            stages,
            sync,
        }))
    }

    /// `VOSU_OP_SCAN_REQUEST` (§6.4).
    fn scan(&self, request: ScanRequest) -> vosu_status {
        self.send(&Message::ScanRequest(request))
    }

    /// `VOSU_OP_LOG_RECORD` (§6.10).
    fn log(&self, level: LogLevel, message: &str, fields: CborValue) -> vosu_status {
        self.send(&Message::LogRecord(LogRecord {
            level,
            message: message.to_owned(),
            fields,
        }))
    }

    /// `VOSU_OP_TIMER_SET` (§6.11); `interval: None` requests a one-shot.
    fn set_timer(&self, timer: vosu_timer_id, interval: Option<Duration>) -> vosu_status {
        self.send(&Message::TimerSet(TimerSet {
            timer,
            interval_ms: interval.map(|d| d.as_millis() as u32),
        }))
    }

    /// `VOSU_OP_TIMER_CANCEL` (§6.11).
    fn cancel_timer(&self, timer: vosu_timer_id) -> vosu_status {
        self.send(&Message::TimerCancel(TimerCancel { timer }))
    }

    /// `VOSU_OP_STATE_SUBSCRIBE` (§6.12): replaces this plugin's previous
    /// selection for `session` wholesale.
    fn subscribe_state(
        &self,
        session: Option<vosu_session_id>,
        selectors: Vec<Selector>,
    ) -> vosu_status {
        self.send(&Message::StateSubscribe(StateSubscribe {
            session,
            selectors,
        }))
    }

    /// `VOSU_OP_ACTION_CALL` (§6.12).
    fn call_action(
        &self,
        invocation: vosu_invocation_id,
        action: &str,
        session: Option<vosu_session_id>,
        args: CborValue,
    ) -> vosu_status {
        self.send(&Message::ActionCall(ActionCall {
            invocation,
            action: action.to_owned(),
            session,
            args,
        }))
    }
}
