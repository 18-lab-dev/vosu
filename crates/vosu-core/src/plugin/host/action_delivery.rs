//! Relay reservation survives cancellation and dispatch pressure. The generic
//! per-instance outbox owns terminal bytes until the actual callback completes.
use std::sync::{Arc, Mutex, Weak};

use vosu_plugin_api::{ActionCall, ActionCancel, ActionProgress, ActionResult, ActionStatus,
    CborValue, Message, vosu_status};

use super::{PluginHost, actions::{MAX_RELAYS, failure}};
use crate::actions::{ActionRegistry, InvocationPolicy, InvocationSink, Principal, TerminalOutcome};
use crate::plugin::{dispatch::OwnedMessage, host_services::PluginInstanceId, outbox::OutboxSlot,
    session::SessionHandle};

struct RelaySink {
    host: Weak<PluginHost>,
    caller: PluginInstanceId,
    original: u64,
    terminal: Mutex<Option<OutboxSlot>>,
}
impl InvocationSink for RelaySink {
    fn progress(&self, mut progress: ActionProgress) {
        let Some(host) = self.host.upgrade() else { return; };
        progress.invocation = self.original;
        // Encoding includes the remapped id. Oversized progress is dropped only.
        let _ = host.dispatch_to_instance(self.caller, &Message::ActionProgress(progress), 0);
    }
    fn finish(&self, outcome: TerminalOutcome) {
        let Some(slot) = self.terminal.lock().unwrap_or_else(|p| p.into_inner()).take() else { return; };
        let Some(host) = self.host.upgrade() else { return; };
        let mut result = ActionResult {
            invocation: self.original, status: ActionStatus::Error, value: None, code: None, message: None,
        };
        match outcome {
            TerminalOutcome::Succeeded(value) => {
                result.status = ActionStatus::Ok;
                result.value = CborValue::serialized(&value).ok();
            }
            TerminalOutcome::Failed(error) => { result.code = Some(error.code); result.message = Some(error.message); }
            TerminalOutcome::Cancelled => { result.code = Some("cancelled".into()); }
        }
        let owned = OwnedMessage::from_message(&Message::ActionResult(result), 0).unwrap_or_else(|_| {
            OwnedMessage::from_message(&Message::ActionResult(ActionResult {
                invocation: self.original, status: ActionStatus::Error, value: None,
                code: Some("result_too_large".into()), message: Some("Action result exceeds frame limit".into()),
            }), 0).expect("bounded terminal error")
        });
        let delivered = slot.send(owned);
        let weak = self.host.clone();
        let key = (self.caller, self.original);
        host.state_runtime.spawn(async move {
            // Receiver also resolves on instance destruction; both paths release
            // exactly this caller slot. No polling/retry queue is introduced.
            let _ = delivered.await;
            if let Some(host) = weak.upgrade() {
                host.action_state.lock().unwrap_or_else(|p| p.into_inner()).relays.remove(&key);
            }
        });
    }
}

impl PluginHost {
    pub(super) fn handle_action_call(&self, caller: PluginInstanceId, call: ActionCall) -> vosu_status {
        let slot = match self.reserve_outbox(caller) {
            Ok(slot) => slot,
            Err(_) => return vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        };
        let key = (caller, call.invocation);
        {
            let mut state = self.action_state.lock().unwrap_or_else(|p| p.into_inner());
            if state.relays.contains_key(&key) { return vosu_status::VOSU_ERR_INVALID_ARGUMENT; }
            if state.relays.keys().filter(|(id, _)| *id == caller).count() >= MAX_RELAYS {
                return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
            }
            state.relays.insert(key, None);
        }
        let sink = Arc::new(RelaySink { host: self.self_ref.clone(), caller,
            original: call.invocation, terminal: Mutex::new(Some(slot)) });
        let outcome = (|| {
            let action = self.get(&call.action).ok_or(crate::actions::InvocationError::UnknownAction)?;
            let session = match call.session {
                None => None,
                Some(handle) => {
                    let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
                    let handle = SessionHandle(handle);
                    if !sessions.is_identified(handle) { return Err(crate::actions::InvocationError::InvalidSession); }
                    Some(sessions.public_id_of(handle).ok_or(crate::actions::InvocationError::InvalidSession)?.to_owned())
                }
            };
            let args = call.args.deserialized().map_err(|_| crate::actions::InvocationError::InvalidArguments)?;
            self.invoke_action(Principal::Plugin(caller), action, session, args, sink.clone(), InvocationPolicy::default())
        })();
        match outcome {
            Ok(id) => {
                // A synchronous terminal can already have scheduled delivery;
                // never resurrect its relay slot if it has completed.
                if let Some(entry) = self.action_state.lock().unwrap_or_else(|p| p.into_inner()).relays.get_mut(&key) {
                    *entry = Some(id);
                }
            }
            Err(error) => sink.finish(failure(&error.to_string(), "Action invocation refused")),
        }
        vosu_status::VOSU_OK
    }

    pub(super) fn handle_action_cancel(&self, caller: PluginInstanceId, cancel: ActionCancel) -> vosu_status {
        let id = self.action_state.lock().unwrap_or_else(|p| p.into_inner())
            .relays.get(&(caller, cancel.invocation)).copied();
        match id {
            Some(Some(id)) => { self.cancel_action(id); vosu_status::VOSU_OK }
            Some(None) => vosu_status::VOSU_OK,
            None => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        }
    }
}
