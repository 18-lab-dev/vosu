//! Lock order: instances -> action state. Session lookups precede action state.
//! No plugin dispatch or sink callback runs while action state is locked.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::Value;
use vosu_plugin_api::{ActionCancel, ActionInvoke, ActionProgress, ActionResult, ActionSessionMode,
    ActionStatus, Caller, CallerKind, CborValue, Message, vosu_status};

use super::PluginHost;
use crate::actions::{ActionDescriptor, ActionRegistry, InvocationError, InvocationExecutor,
    InvocationId, InvocationPolicy, InvocationSink, JobFailure, Principal, TerminalOutcome};
use crate::plugin::{host_services::PluginInstanceId, registry::LoadedPlugin, session::SessionHandle};

pub(super) const MAX_INVOCATIONS: usize = 2048;
pub(super) const MAX_RELAYS: usize = 64;

pub(super) struct PendingInvocation {
    pub action: Arc<ActionDescriptor>,
    pub session: Option<SessionHandle>,
    pub sink: Option<Arc<dyn InvocationSink>>,
    pub deadline: Option<tokio::task::JoinHandle<()>>,
    pub cancelled: bool,
    pub caller: Option<PluginInstanceId>,
}
impl Drop for PendingInvocation {
    fn drop(&mut self) {
        if let Some(task) = self.deadline.take() { task.abort(); }
    }
}

#[derive(Default)]
pub(super) struct ActionState {
    pub descriptors: HashMap<String, Arc<ActionDescriptor>>,
    pub grants: HashMap<PluginInstanceId, Vec<vosu_plugin_api::Capability>>,
    pub pending: HashMap<InvocationId, PendingInvocation>,
    pub relays: HashMap<(PluginInstanceId, u64), Option<InvocationId>>,
    pub next_id: u64,
}

impl ActionRegistry for PluginHost {
    fn get(&self, action: &str) -> Option<Arc<ActionDescriptor>> {
        self.action_state.lock().unwrap_or_else(|p| p.into_inner())
            .descriptors.get(action).cloned()
    }
}

impl PluginHost {
    pub fn invocation_executor(&self) -> InvocationExecutor {
        self.invocation_executor_with_policy(InvocationPolicy::default())
    }

    /// One policy for both jobs and relays; tests may inject a short deadline.
    pub fn invocation_executor_with_policy(&self, policy: InvocationPolicy) -> InvocationExecutor {
        InvocationExecutor { host: self.self_ref.clone(), policy }
    }

    pub(super) fn register_actions(&self, instance: PluginInstanceId, plugin: &LoadedPlugin) {
        let manifest = plugin.manifest();
        let mut state = self.action_state.lock().unwrap_or_else(|p| p.into_inner());
        state.grants.insert(instance, manifest.requires.iter().chain(&manifest.optional).cloned().collect());
        for declaration in &manifest.actions {
            let id = format!("{}.{}", manifest.id, declaration.name);
            state.descriptors.insert(id.clone(), Arc::new(ActionDescriptor {
                id, version: semver::Version::parse(&declaration.version).expect("validated manifest"),
                provider: instance, provider_id: manifest.id.clone(), title: declaration.title.clone(),
                session: declaration.session, cli: declaration.cli,
                args_schema: Arc::clone(&plugin.compiled_action_schemas()[&declaration.args_schema]),
                result_schema: Arc::clone(&plugin.compiled_action_schemas()[&declaration.result_schema]),
            }));
        }
    }

    pub(crate) fn invoke_action(
        &self, principal: Principal, action: Arc<ActionDescriptor>, session: Option<String>,
        arguments: Value, sink: Arc<dyn InvocationSink>, policy: InvocationPolicy,
    ) -> Result<InvocationId, InvocationError> {
        if self.shutting_down.load(Ordering::Acquire) { return Err(InvocationError::Unavailable); }
        let caller = match &principal {
            Principal::Broker => {
                if !action.cli { return Err(InvocationError::PermissionDenied); }
                Caller { kind: CallerKind::Cli, id: "vosux".into() }
            }
            Principal::Tool(_) => Caller { kind: CallerKind::Tool, id: "tool-session".into() },
            Principal::Plugin(instance) => Caller {
                kind: CallerKind::Plugin,
                id: self.plugin_id_of(*instance).ok_or(InvocationError::PermissionDenied)?,
            },
        };
        let handle = {
            let sessions = self.sessions.lock().unwrap_or_else(|p| p.into_inner());
            match (action.session, session.as_deref()) {
                (ActionSessionMode::Required, None) | (ActionSessionMode::None, Some(_)) =>
                    return Err(InvocationError::InvalidSession),
                (_, None) => None,
                (_, Some(id)) => {
                    let handle = sessions.identified_sessions()
                        .find(|(h, owner, _)| owner.as_str() == action.provider_id && sessions.public_id_of(*h) == Some(id))
                        .map(|(h, _, _)| h).ok_or(InvocationError::InvalidSession)?;
                    Some(handle)
                }
            }
        };
        action.args_schema.validate_arguments(&arguments).map_err(|_| InvocationError::InvalidArguments)?;
        let args = CborValue::serialized(&arguments).map_err(|_| InvocationError::InvalidArguments)?;
        let id = {
            let mut state = self.action_state.lock().unwrap_or_else(|p| p.into_inner());
            let registered = state.descriptors.get(&action.id).ok_or(InvocationError::UnknownAction)?;
            if !Arc::ptr_eq(registered, &action) { return Err(InvocationError::UnknownAction); }
            if let Principal::Plugin(instance) = principal {
                let capability = format!("action:{}", action.id);
                if !state.grants.get(&instance).is_some_and(|grants| grants.iter().any(|g|
                    g.id == capability && semver::VersionReq::parse(&g.version).is_ok_and(|v| v.matches(&action.version)))) {
                    return Err(InvocationError::PermissionDenied);
                }
            }
            if state.pending.len() >= MAX_INVOCATIONS { return Err(InvocationError::Capacity); }
            state.next_id = state.next_id.checked_add(1).ok_or(InvocationError::Capacity)?;
            let id = state.next_id;
            let message = Message::ActionInvoke(ActionInvoke {
                invocation: id, action: action.id.clone(), session: handle.map(|h| h.0), caller, args,
            });
            // The callee id's CBOR width is part of the frame budget.
            message.encode_payload().map_err(|_| InvocationError::ArgumentsTooLarge)?;
            state.pending.insert(id, PendingInvocation {
                action: Arc::clone(&action), session: handle, sink, deadline: None, cancelled: false,
            });
            (id, message)
        };
        let weak = self.self_ref.clone();
        let timer = self.state_runtime.spawn(async move {
            tokio::time::sleep(policy.deadline).await;
            if let Some(host) = weak.upgrade() {
                host.finish_action(invocation, failure("timed_out", "Action deadline elapsed"));
            }
        });
        {
            let mut state = self.action_state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(pending) = state.pending.get_mut(&invocation) { pending.deadline = Some(timer); }
            else { timer.abort(); }
        }
        if self.dispatch_to_instance(provider, &message, 0).is_err() {
            self.finish_action(invocation, failure("plugin_crashed", "Action provider unavailable"));
        }
        Ok(())
    }

    pub(crate) fn cancel_action(&self, invocation: InvocationId) {
        let provider = {
            let mut state = self.action_state.lock().unwrap_or_else(|p| p.into_inner());
            let Some(pending) = state.pending.get_mut(&invocation) else { return; };
            if pending.cancelled { return; }
            pending.cancelled = true;
            pending.action.provider
        };
        // Advisory only: neither the invocation nor a relay slot is removed.
        let _ = self.dispatch_to_instance(provider, &Message::ActionCancel(ActionCancel { invocation }), 0);
    }

    pub(super) fn finish_action(&self, id: InvocationId, result: TerminalOutcome) {
        let pending = self.action_state.lock().unwrap_or_else(|p| p.into_inner()).pending.remove(&id);
        if let Some(pending) = pending { pending.sink.finish(result); }
    }

    pub(super) fn handle_action_progress(&self, instance: PluginInstanceId, progress: ActionProgress) -> vosu_status {
        if progress.fraction.is_some_and(|f| !f.is_finite() || !(0.0..=1.0).contains(&f)) {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let sink = {
            let state = self.action_state.lock().unwrap_or_else(|p| p.into_inner());
            let Some(pending) = state.pending.get(&progress.invocation) else { return vosu_status::VOSU_ERR_INVALID_ARGUMENT; };
            if pending.action.provider != instance { return vosu_status::VOSU_ERR_PERMISSION_DENIED; }
            Arc::clone(&pending.sink)
        };
        sink.progress(progress);
        vosu_status::VOSU_OK
    }

    pub(super) fn handle_action_result(&self, instance: PluginInstanceId, result: ActionResult) -> vosu_status {
        let pending = {
            let mut state = self.action_state.lock().unwrap_or_else(|p| p.into_inner());
            let Some(pending) = state.pending.get(&result.invocation) else { return vosu_status::VOSU_ERR_INVALID_ARGUMENT; };
            if pending.action.provider != instance { return vosu_status::VOSU_ERR_PERMISSION_DENIED; }
            state.pending.remove(&result.invocation).expect("checked entry")
        };
        let outcome = if result.status == ActionStatus::Ok {
            let value = result.value.unwrap_or(CborValue::Null).deserialized::<Value>();
            match value {
                Ok(value) if pending.action.result_schema.validate_result(&value).is_ok() => TerminalOutcome::Succeeded(value),
                _ => failure("invalid_result", "Action returned a contract-invalid result"),
            }
        } else {
            failure(result.code.as_deref().unwrap_or("action_failed"), result.message.as_deref().unwrap_or("Action failed"))
        };
        pending.sink.finish(outcome);
        vosu_status::VOSU_OK
    }

    /// Production instance-loss seam, shared by shutdown and future process host.
    pub fn action_instance_lost(&self, instance: PluginInstanceId) {
        let pending = {
            let mut state = self.action_state.lock().unwrap_or_else(|p| p.into_inner());
            state.descriptors.retain(|_, d| d.provider != instance);
            state.grants.remove(&instance);
            state.relays.retain(|(caller, _), _| *caller != instance);
            let ids: Vec<_> = state.pending.iter().filter_map(|(id, p)|
                (p.action.provider == instance).then_some(*id)).collect();
            ids.into_iter().filter_map(|id| state.pending.remove(&id)).collect::<Vec<_>>()
        };
        for entry in pending { entry.sink.finish(failure("plugin_crashed", "Action provider unavailable")); }
    }

    pub(super) fn action_session_detached(&self, handle: SessionHandle) {
        let ids: Vec<_> = self.action_state.lock().unwrap_or_else(|p| p.into_inner())
            .pending.iter().filter_map(|(id, p)| (p.session == Some(handle)).then_some(*id)).collect();
        for id in ids { self.cancel_action(id); }
    }
}

pub(super) fn failure(code: &str, message: &str) -> TerminalOutcome {
    TerminalOutcome::Failed(JobFailure { code: code.into(), message: message.into() })
}

#[cfg(test)]
mod tests;
