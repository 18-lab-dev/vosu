//! Bounded public jobs, independent of ABI relay invocation lifetimes.
//!
//! The registry mutex is the only domain lock. Never dispatch plugin code or
//! invoke caller callbacks while holding it. Callers authenticate live launch
//! sessions before presenting their principal; ownership does not authorize an
//! expired token. HTTP response policy (including no-store) belongs to the server.

pub mod deadline;
pub mod invocation;
pub mod jobs;
pub mod registry;
pub mod waiters;

pub use invocation::{
    InvocationError, InvocationExecutor, InvocationId, InvocationPolicy, InvocationResult,
    InvocationSink, JobSink, PreparedInvocation, SessionId,
};
pub use registry::{ActionDescriptor, ActionRegistry, ToolAuthority};

pub use deadline::DeadlineGuard;
pub use jobs::{
    CancellationRequest, JobError, JobFailure, JobOwner, JobProgress, JobRegistry, JobSnapshot,
    JobState, Principal, TerminalOutcome,
};
pub use waiters::{JobWait, WaiterGuard};
