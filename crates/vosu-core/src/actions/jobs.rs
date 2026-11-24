//! Immutable job snapshots and first-terminal-wins transitions.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;

pub const MAX_JOBS: usize = 1024;
pub const TERMINAL_RETENTION: Duration = Duration::from_secs(300);
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

/// One exact launch-session identity. Clone into its authentication record;
/// creating another identity, even for the same tool, never inherits its jobs.
#[derive(Debug, Clone, Default)]
pub struct JobOwner(Arc<()>, Option<Arc<str>>);

impl JobOwner {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh launch identity with its authenticated owning tool metadata.
    pub fn for_tool(tool_id: Arc<str>) -> Self {
        Self(Arc::new(()), Some(tool_id))
    }

    pub fn tool_id(&self) -> Option<&str> {
        self.1.as_deref()
    }

    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Debug, Clone)]
pub enum Principal {
    Broker,
    Tool(JobOwner),
    Plugin(crate::plugin::PluginInstanceId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        self != Self::Running
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobProgress {
    pub fraction: Option<f64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JobFailure {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobSnapshot {
    pub id: String,
    pub action: String,
    pub session: Option<String>,
    pub state: JobState,
    pub revision: u64,
    pub progress: Option<JobProgress>,
    pub result: Option<Value>,
    pub error: Option<JobFailure>,
}

#[derive(Debug)]
pub enum TerminalOutcome {
    Succeeded(Value),
    Failed(JobFailure),
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JobError {
    #[error("unknown_job")]
    UnknownJob,
    #[error("too_many_jobs")]
    TooManyJobs,
    #[error("job id space exhausted")]
    IdExhausted,
}

/// Dispatch cancellation only when `dispatch` is true, after the registry lock
/// has been released. Cancellation does not change the public revision/state.
#[derive(Debug)]
pub struct CancellationRequest {
    pub job: Arc<JobSnapshot>,
    pub dispatch: bool,
}

pub(super) struct Entry {
    pub snapshot: Arc<JobSnapshot>,
    owner: Principal,
    terminal_at: Option<Instant>,
    cancel_requested: bool,
    pub waiters: usize,
    pub created_at: Instant,
    pub changes: watch::Sender<Arc<JobSnapshot>>,
}

impl Entry {
    pub(super) fn visible_to(&self, principal: &Principal) -> bool {
        match (principal, &self.owner) {
            (Principal::Broker, _) => true,
            (Principal::Tool(a), Principal::Tool(b)) => a.same(b),
            (Principal::Plugin(a), Principal::Plugin(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(Default)]
pub(super) struct RegistryState {
    pub entries: HashMap<String, Entry>,
    pub waiters: usize,
}

impl RegistryState {
    pub(super) fn cleanup(&mut self, now: Instant) {
        self.entries.retain(|_, entry| {
            entry
                .terminal_at
                .is_none_or(|at| now.saturating_duration_since(at) < TERMINAL_RETENTION)
        });
    }
}

#[derive(Default)]
pub(super) struct RegistryInner {
    pub state: Mutex<RegistryState>,
}

impl RegistryInner {
    pub fn lock(&self) -> MutexGuard<'_, RegistryState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

#[derive(Clone, Default)]
pub struct JobRegistry {
    pub(super) inner: Arc<RegistryInner>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the captured initial running snapshot, not a later registry read.
    /// Dispatch may finish synchronously; wait=0 must still return this snapshot.
    pub fn create(
        &self,
        owner: Principal,
        action: String,
        session: Option<String>,
    ) -> Result<Arc<JobSnapshot>, JobError> {
        self.create_at(owner, action, session, Instant::now())
    }

    fn create_at(
        &self,
        owner: Principal,
        action: String,
        session: Option<String>,
        now: Instant,
    ) -> Result<Arc<JobSnapshot>, JobError> {
        let mut state = self.inner.lock();
        state.cleanup(now);
        if state.entries.len() == MAX_JOBS {
            let oldest = state
                .entries
                .iter()
                .filter_map(|(id, entry)| entry.terminal_at.map(|at| (id, at)))
                .min_by_key(|(_, at)| *at)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                state.entries.remove(&id);
            } else {
                return Err(JobError::TooManyJobs);
            }
        }
        let number = NEXT_JOB
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| JobError::IdExhausted)?;
        let snapshot = Arc::new(JobSnapshot {
            id: format!("job-{number}"),
            action,
            session,
            state: JobState::Running,
            revision: 0,
            progress: None,
            result: None,
            error: None,
        });
        let (changes, _) = watch::channel(Arc::clone(&snapshot));
        state.entries.insert(
            snapshot.id.clone(),
            Entry {
                snapshot: Arc::clone(&snapshot),
                owner,
                terminal_at: None,
                cancel_requested: false,
                created_at: now,
                waiters: 0,
                changes,
            },
        );
        Ok(snapshot)
    }

    pub fn get(&self, id: &str, principal: &Principal) -> Result<Arc<JobSnapshot>, JobError> {
        self.get_at(id, principal, Instant::now())
    }

    fn get_at(
        &self,
        id: &str,
        principal: &Principal,
        now: Instant,
    ) -> Result<Arc<JobSnapshot>, JobError> {
        let mut state = self.inner.lock();
        state.cleanup(now);
        state
            .entries
            .get(id)
            .filter(|entry| entry.visible_to(principal))
            .map(|entry| Arc::clone(&entry.snapshot))
            .ok_or(JobError::UnknownJob)
    }

    /// Internal invocation sink. External callers must never select an arbitrary
    /// job id to update; the host retains the id alongside its invocation slot.
    pub fn progress(&self, id: &str, progress: JobProgress) -> Result<bool, JobError> {
        self.update(id, None, Some(progress), Instant::now())
    }

    pub fn finish(&self, id: &str, outcome: TerminalOutcome) -> Result<bool, JobError> {
        self.finish_at(id, outcome, Instant::now())
    }

    fn finish_at(
        &self,
        id: &str,
        outcome: TerminalOutcome,
        now: Instant,
    ) -> Result<bool, JobError> {
        self.update(id, Some(outcome), None, now)
    }

    fn update(
        &self,
        id: &str,
        outcome: Option<TerminalOutcome>,
        progress: Option<JobProgress>,
        now: Instant,
    ) -> Result<bool, JobError> {
        let mut state = self.inner.lock();
        state.cleanup(now);
        let entry = state.entries.get_mut(id).ok_or(JobError::UnknownJob)?;
        if entry.snapshot.state.is_terminal() {
            return Ok(false);
        }
        let snapshot = Arc::make_mut(&mut entry.snapshot);
        // A u64 cannot wrap within a bounded invocation lifetime. Fail closed
        // rather than publish a revision that moves backwards if exhausted.
        let Some(revision) = snapshot.revision.checked_add(1) else {
            return Ok(false);
        };
        snapshot.revision = revision;
        if let Some(outcome) = outcome {
            entry.terminal_at = Some(now);
            match outcome {
                TerminalOutcome::Succeeded(value) => {
                    snapshot.state = JobState::Succeeded;
                    snapshot.result = Some(value);
                }
                TerminalOutcome::Failed(error) if error.code == "cancelled" => {
                    snapshot.state = JobState::Cancelled;
                }
                TerminalOutcome::Failed(error) => {
                    snapshot.state = JobState::Failed;
                    snapshot.error = Some(error);
                }
                TerminalOutcome::Cancelled => snapshot.state = JobState::Cancelled,
            }
        } else {
            snapshot.progress = progress;
        }
        entry.changes.send_replace(Arc::clone(&entry.snapshot));
        Ok(true)
    }

    pub fn request_cancel(
        &self,
        id: &str,
        principal: &Principal,
    ) -> Result<CancellationRequest, JobError> {
        let mut state = self.inner.lock();
        state.cleanup(Instant::now());
        let entry = state
            .entries
            .get_mut(id)
            .filter(|entry| entry.visible_to(principal))
            .ok_or(JobError::UnknownJob)?;
        let dispatch = !entry.cancel_requested && !entry.snapshot.state.is_terminal();
        entry.cancel_requested |= dispatch;
        Ok(CancellationRequest {
            job: Arc::clone(&entry.snapshot),
            dispatch,
        })
    }

    /// Detach asks for cancellation without changing the captured session id.
    /// Returned ids are dispatched by the host only after this call returns.
    pub fn cancel_session(&self, session: &str) -> Vec<String> {
        let mut state = self.inner.lock();
        state.cleanup(Instant::now());
        state
            .entries
            .iter_mut()
            .filter_map(|(id, entry)| {
                if entry.snapshot.session.as_deref() == Some(session)
                    && !entry.snapshot.state.is_terminal()
                    && !entry.cancel_requested
                {
                    entry.cancel_requested = true;
                    Some(id.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
