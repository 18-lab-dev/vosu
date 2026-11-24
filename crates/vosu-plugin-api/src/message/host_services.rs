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
    /// handles, not this trait. `correlation_id` is this plugin's own
    /// choice (`0` when the opcode has no reply, or when echoing a
    /// request this plugin received back as its reply — see
    /// `on_message`'s own `correlation_id` parameter): the host carries it
    /// unchanged into the matching reply's `vosu_message.correlation_id`
    /// (B1).
    fn send(&self, message: &Message, correlation_id: u64) -> vosu_status;

    /// `VOSU_OP_PUBLISH_FIELD`.
    fn publish(
        &self,
        handle: vosu_session_id,
        ns: &str,
        pointer: &str,
        value: PublishValue,
    ) -> vosu_status {
        self.send(
            &Message::PublishField(crate::message::flat::PublishField {
                handle,
                ns: ns.to_owned(),
                pointer: pointer.to_owned(),
                value,
            }),
            0,
        )
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
        self.send(
            &Message::SessionIdentify(SessionIdentify {
                handle,
                game: game.to_owned(),
                variant: variant.to_owned(),
                name: name.to_owned(),
                version: version.to_owned(),
                focus_priority,
            }),
            0,
        )
    }

    /// `VOSU_OP_ROOT_DECLARE` (§6.5). `path` must be valid UTF-8 (the wire
    /// field is a CBOR string): a non-UTF-8 path is rejected with
    /// `VOSU_ERR_INVALID_ARGUMENT` rather than lossily substituted,
    /// so the host never resolves a different path than the one on disk
    /// (minor 6).
    fn declare_root(
        &self,
        handle: Option<vosu_session_id>,
        name: &str,
        path: &Path,
    ) -> vosu_status {
        let Some(path) = path.to_str() else {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        };
        self.send(
            &Message::RootDeclare(RootDeclare {
                handle,
                name: name.to_owned(),
                path: path.to_owned(),
            }),
            0,
        )
    }

    /// `VOSU_OP_READ_PLAN_SUBMIT` (§6.3). `plan` is caller-assigned by the
    /// caller of this method, never generated here; a result is correlated
    /// by `plan`, not `correlation_id` (`docs/v2/plugin-abi.md` §5), so
    /// this submission always carries `correlation_id` `0`.
    fn submit_read_plan(
        &self,
        plan: vosu_plan_id,
        handle: vosu_session_id,
        root: PlanBase,
        stages: Vec<Stage>,
    ) -> vosu_status {
        self.send(
            &Message::ReadPlanSubmit(ReadPlanSubmit {
                plan,
                handle,
                root,
                stages,
                sync: false,
            }),
            0,
        )
    }

    /// `VOSU_OP_SCAN_REQUEST` (§6.4): `correlation_id` is this plugin's own
    /// choice, echoed back in the matching `VOSU_OP_SCAN_RESULT` so
    /// several outstanding scans can be told apart.
    fn scan(&self, correlation_id: u64, request: ScanRequest) -> vosu_status {
        self.send(&Message::ScanRequest(request), correlation_id)
    }

    /// `VOSU_OP_LOG_RECORD` (§6.10).
    fn log(&self, level: LogLevel, message: &str, fields: CborValue) -> vosu_status {
        self.send(
            &Message::LogRecord(LogRecord {
                level,
                message: message.to_owned(),
                fields,
            }),
            0,
        )
    }

    /// `VOSU_OP_TIMER_SET` (§6.11); `interval: None` requests a one-shot.
    /// A non-zero `interval` below one millisecond is rejected with
    /// `VOSU_ERR_INVALID_ARGUMENT` before anything is sent, rather than
    /// truncated to `0` (minor 6).
    fn set_timer(&self, timer: vosu_timer_id, interval: Option<Duration>) -> vosu_status {
        let interval_ms = match interval.map(checked_millis).transpose() {
            Ok(interval_ms) => interval_ms,
            Err(()) => return vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        };
        self.send(&Message::TimerSet(TimerSet { timer, interval_ms }), 0)
    }

    /// `VOSU_OP_READ_PLAN_SUBMIT` with `VOSU_READ_PLAN_SYNC` set (§6.3, D5),
    /// behind the unpublished `unstable-sync-reads` feature: never call
    /// this directly. [`crate::PlanMemory`] is the one sanctioned caller
    /// — it routes the matching `VOSU_OP_READ_PLAN_RESULT` to a per-plan
    /// slot before the plugin's own `on_message` ever sees it (M5); a
    /// synchronous result sent any other way would instead reach
    /// `on_message` like any other message, which §6.3 says never
    /// happens, and a published build must not be able to construct that
    /// contradiction at all (D11).
    #[cfg(feature = "unstable-sync-reads")]
    fn submit_read_plan_sync(
        &self,
        plan: vosu_plan_id,
        handle: vosu_session_id,
        root: PlanBase,
        stages: Vec<Stage>,
    ) -> vosu_status {
        self.send(
            &Message::ReadPlanSubmit(ReadPlanSubmit {
                plan,
                handle,
                root,
                stages,
                sync: true,
            }),
            0,
        )
    }

    /// `VOSU_OP_TIMER_CANCEL` (§6.11).
    fn cancel_timer(&self, timer: vosu_timer_id) -> vosu_status {
        self.send(&Message::TimerCancel(TimerCancel { timer }), 0)
    }

    /// `VOSU_OP_READ_PLAN_SUBMIT` with `VOSU_READ_PLAN_SYNC` set, already
    /// flat-encoded (`crate::sdk::plan_memory`'s own zero-allocation path,
    /// D17): sends `payload` verbatim, skipping the `Message` enum/
    /// `encode_payload` round trip this trait's other methods go through.
    /// This default implementation instead decodes `payload` back into a
    /// typed [`Message`] and calls [`Self::send`] — correct, but
    /// allocating, for a `HostServices` implementation with no faster
    /// path of its own; [`crate::VtableHostServices`] overrides this to
    /// pass `payload` straight to the vtable instead.
    #[cfg(feature = "unstable-sync-reads")]
    fn send_flat(
        &self,
        opcode: crate::vosu_opcode,
        flags: u32,
        correlation_id: u64,
        payload: &[u8],
    ) -> vosu_status {
        match Message::decode(opcode, flags, payload) {
            Ok(message) => self.send(&message, correlation_id),
            Err(_) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        }
    }

    /// `VOSU_OP_STATE_SUBSCRIBE` (§6.12): replaces this plugin's previous
    /// selection for `session` wholesale.
    fn subscribe_state(
        &self,
        session: Option<vosu_session_id>,
        selectors: Vec<Selector>,
    ) -> vosu_status {
        self.send(
            &Message::StateSubscribe(StateSubscribe { session, selectors }),
            0,
        )
    }

    /// `VOSU_OP_ACTION_CALL` (§6.12).
    fn call_action(
        &self,
        invocation: vosu_invocation_id,
        action: &str,
        session: Option<vosu_session_id>,
        args: CborValue,
    ) -> vosu_status {
        self.send(
            &Message::ActionCall(ActionCall {
                invocation,
                action: action.to_owned(),
                session,
                args,
            }),
            0,
        )
    }
}

/// Converts `interval` to whole milliseconds for `VOSU_OP_TIMER_SET`
/// (minor 6): `Ok(None)` for a one-shot, `Ok(Some(ms))` for a periodic
/// timer saturating at `u32::MAX` instead of silently wrapping, or `Err`
/// for a non-zero interval below one millisecond — §6.11's `interval_ms`
/// is `u32|null` with no fractional-millisecond representation, so
/// truncating such a value to `0` would silently turn it into "fire every
/// tick" instead of reporting that this interval cannot be expressed.
fn checked_millis(interval: Duration) -> Result<u32, ()> {
    let millis = interval.as_millis();
    if millis == 0 && !interval.is_zero() {
        return Err(());
    }
    Ok(u32::try_from(millis).unwrap_or(u32::MAX))
}
