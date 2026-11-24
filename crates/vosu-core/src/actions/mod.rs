//! Bounded public jobs, independent of ABI relay invocation lifetimes.
//!
//! The registry mutex is the only domain lock. Never dispatch plugin code or
//! invoke caller callbacks while holding it. Callers authenticate live launch
//! sessions before presenting their principal; ownership does not authorize an
//! expired token. HTTP response policy (including no-store) belongs to the server.

pub mod deadline;
pub mod jobs;
pub mod waiters;
pub mod invocation;
pub mod registry;

pub use registry::{ActionDescriptor, ActionRegistry};
pub use invocation::{
    InvocationError, InvocationExecutor, InvocationId, InvocationPolicy, InvocationResult,
    InvocationSink, JobSink, Principal, SessionId,
};

pub use deadline::{DeadlineGuard, DeadlinePolicy};
pub use jobs::{
    CancellationRequest, JobError, JobFailure, JobOwner, JobPrincipal, JobProgress, JobRegistry,
    JobSnapshot, JobState, TerminalOutcome,
};
pub use waiters::{JobWait, WaiterGuard};
