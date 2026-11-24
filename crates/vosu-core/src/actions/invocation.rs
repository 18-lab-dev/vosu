//! Common invocation API; HTTP jobs and ABI relays have independent storage.
use std::sync::{Arc, Weak};
use std::time::Duration;

use serde_json::Value;
use vosu_plugin_api::ActionProgress;

use super::{ActionDescriptor, JobOwner, JobRegistry, JobProgress, TerminalOutcome};
use crate::plugin::{host::PluginHost, host_services::PluginInstanceId};

pub type InvocationId = u64;
pub type SessionId = String;
pub type InvocationResult = TerminalOutcome;

/// Tool identities are authenticated and capability-checked by composition on
/// each request, not derived from a tool id or retained bearer token.
#[derive(Clone, Debug)]
pub enum Principal {
    Tool(JobOwner),
    Broker,
    Plugin(PluginInstanceId),
}

#[derive(Clone, Copy, Debug)]
pub struct InvocationPolicy {
    pub deadline: Duration,
}
impl Default for InvocationPolicy {
    fn default() -> Self {
        Self { deadline: Duration::from_secs(30) }
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
}
impl InvocationExecutor {
    pub fn invoke(
        &self,
        principal: Principal,
        action: Arc<ActionDescriptor>,
        session: Option<SessionId>,
        arguments: Value,
        sink: Arc<dyn InvocationSink>,
    ) -> Result<InvocationId, InvocationError> {
        self.host.upgrade().ok_or(InvocationError::Unavailable)?
            .invoke_action(principal, action, session, arguments, sink, self.policy)
    }

    pub fn cancel(&self, invocation: InvocationId) {
        if let Some(host) = self.host.upgrade() {
            host.cancel_action(invocation);
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
    pub fn new(registry: JobRegistry, id: String) -> Self { Self { registry, id } }
}
impl InvocationSink for JobSink {
    fn progress(&self, progress: ActionProgress) {
        let _ = self.registry.progress(&self.id, JobProgress {
            fraction: progress.fraction,
            message: progress.message,
        });
    }
    fn finish(&self, result: InvocationResult) {
        let _ = self.registry.finish(&self.id, result);
    }
}
