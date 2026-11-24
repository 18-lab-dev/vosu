//! Common invocation API; HTTP jobs and ABI relays have independent storage.
use std::sync::{Arc, Weak};
use std::time::Duration;

use super::{ActionDescriptor, JobProgress, JobRegistry, Principal, TerminalOutcome};
use crate::plugin::PluginHost;
use serde_json::Value;
use vosu_plugin_api::ActionProgress;

pub type InvocationId = u64;
pub type SessionId = String;
pub type InvocationResult = TerminalOutcome;

#[derive(Clone, Copy, Debug)]
pub struct InvocationPolicy {
    pub deadline: Duration,
}
impl Default for InvocationPolicy {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum InvocationError {
    #[error("unknown_action")]
    UnknownAction,
    #[error("permission_denied")]
    PermissionDenied,
    #[error("invalid_session")]
    InvalidSession,
    #[error("invalid_arguments")]
    InvalidArguments,
    #[error("args_too_large")]
    ArgumentsTooLarge,
    #[error("too_many_invocations")]
    Capacity,
    #[error("plugin_crashed")]
    Unavailable,
}

pub trait InvocationSink: Send + Sync {
    fn progress(&self, progress: ActionProgress);
    fn finish(&self, result: InvocationResult);
}

#[derive(Clone)]
pub struct InvocationExecutor {
    pub(crate) host: Weak<PluginHost>,
    pub(crate) policy: InvocationPolicy,
    pub(crate) tool_authority: Option<Arc<dyn super::ToolAuthority>>,
}
impl InvocationExecutor {
    pub fn with_tool_authority(mut self, authority: Arc<dyn super::ToolAuthority>) -> Self {
        self.tool_authority = Some(authority);
        self
    }

    pub fn invoke(
        &self,
        principal: Principal,
        action: Arc<ActionDescriptor>,
        session: Option<SessionId>,
        arguments: Value,
        sink: Arc<dyn InvocationSink>,
    ) -> Result<InvocationId, InvocationError> {
        self.prepare(principal, action, session, arguments)?
            .invoke(sink)
    }

    /// Reserves and validates the exact encoded invocation before job creation.
    /// Dropping this preparation releases the bounded reservation.
    pub fn prepare(
        &self,
        principal: Principal,
        action: Arc<ActionDescriptor>,
        session: Option<SessionId>,
        arguments: Value,
    ) -> Result<PreparedInvocation, InvocationError> {
        if let Principal::Tool(owner) = &principal
            && !self
                .tool_authority
                .as_ref()
                .is_some_and(|authority| authority.authorizes(owner, &action))
        {
            return Err(InvocationError::PermissionDenied);
        }
        self.host
            .upgrade()
            .ok_or(InvocationError::Unavailable)?
            .prepare_action(principal, action, session, arguments, self.policy)
    }

    pub fn cancel(&self, invocation: InvocationId) {
        if let Some(host) = self.host.upgrade() {
            host.cancel_action(invocation);
        }
    }
}

pub struct PreparedInvocation {
    pub(crate) host: Weak<PluginHost>,
    pub(crate) id: InvocationId,
    pub(crate) message: Option<vosu_plugin_api::Message>,
    pub(crate) provider: crate::plugin::PluginInstanceId,
    pub(crate) policy: InvocationPolicy,
}
impl PreparedInvocation {
    pub fn invoke(
        mut self,
        sink: Arc<dyn InvocationSink>,
    ) -> Result<InvocationId, InvocationError> {
        let host = self.host.upgrade().ok_or(InvocationError::Unavailable)?;
        host.start_action(
            self.id,
            self.provider,
            self.message.take().expect("prepared once"),
            sink,
            self.policy,
        )?;
        Ok(self.id)
    }
}
impl Drop for PreparedInvocation {
    fn drop(&mut self) {
        if self.message.is_some()
            && let Some(host) = self.host.upgrade()
        {
            host.discard_prepared_action(self.id);
        }
    }
}

/// Only the invocation creator chooses the job id; public ownership checks
/// remain in JobRegistry. Terminal transitions remain first-wins in that domain.
pub struct JobSink {
    registry: JobRegistry,
    id: String,
}
impl JobSink {
    pub fn new(registry: JobRegistry, id: String) -> Self {
        Self { registry, id }
    }
}
impl InvocationSink for JobSink {
    fn progress(&self, progress: ActionProgress) {
        let _ = self.registry.progress(
            &self.id,
            JobProgress {
                fraction: progress.fraction,
                message: progress.message,
            },
        );
    }
    fn finish(&self, result: InvocationResult) {
        let _ = self.registry.finish(&self.id, result);
    }
}
