//! Immutable job snapshots and first-terminal-wins transitions.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;

use super::artifacts::{ArtifactCell, ArtifactView, DueRelease};

mod artifact_jobs;

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
    Plugin(crate::identity::PluginInstanceId),
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
    /// Only on a `succeeded` job whose invocation declared artifacts
    /// (AF.12); the key is absent on every other job.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Vec<ArtifactView>>,
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
    invocation: Option<(
        std::sync::Weak<dyn super::InvocationHost>,
        super::InvocationId,
    )>,
    pub waiters: usize,
    pub created_at: Instant,
    pub changes: watch::Sender<Arc<JobSnapshot>>,
    /// Declared artifacts, in declaration order (AF.12).
    artifacts: Vec<Arc<ArtifactCell>>,
    /// The owning launch session was invalidated: no new artifact.
    revoked: bool,
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

    /// Releases every artifact; notices due now are queued on `released`.
    fn release_artifacts(&self, released: &mut Vec<DueRelease>) {
        released.extend(self.artifacts.iter().filter_map(|cell| cell.release(true)));
    }
}

pub(super) struct RegistryState {
    pub entries: HashMap<String, Entry>,
    pub waiters: usize,
    retention: Duration,
    /// `ARTIFACT_RELEASE` notices due once the registry lock is released.
    released: Vec<DueRelease>,
}

impl Default for RegistryState {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            waiters: 0,
            retention: TERMINAL_RETENTION,
            released: Vec::new(),
        }
    }
}

impl RegistryState {
    pub(super) fn cleanup(&mut self, now: Instant) {
        let retention = self.retention;
        let released = &mut self.released;
        self.entries.retain(|_, entry| {
            let keep = entry
                .terminal_at
                .is_none_or(|at| now.saturating_duration_since(at) < retention);
            if !keep {
                entry.release_artifacts(released);
            }
            keep
        });
    }

    fn remove(&mut self, id: &str) {
        if let Some(entry) = self.entries.remove(id) {
            entry.release_artifacts(&mut self.released);
        }
    }
}

#[derive(Default)]
pub(super) struct RegistryInner {
    pub state: Mutex<RegistryState>,
    /// Active artifact downloads across every job.
    downloads: Arc<AtomicUsize>,
}

impl RegistryInner {
    pub fn lock(&self) -> RegistryGuard<'_> {
        RegistryGuard(Some(
            self.state
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
        ))
    }
}

/// The registry lock. Dropping it releases the lock first, then sends the
/// artifact releases its holder queued, so no plugin delivery ever runs
/// under the registry lock.
pub(super) struct RegistryGuard<'a>(Option<MutexGuard<'a, RegistryState>>);

impl Deref for RegistryGuard<'_> {
    type Target = RegistryState;
    fn deref(&self) -> &RegistryState {
        self.0.as_ref().expect("held until drop")
    }
}

impl DerefMut for RegistryGuard<'_> {
    fn deref_mut(&mut self) -> &mut RegistryState {
        self.0.as_mut().expect("held until drop")
    }
}

impl Drop for RegistryGuard<'_> {
    fn drop(&mut self) {
        let Some(mut state) = self.0.take() else {
            return;
        };
        let released = std::mem::take(&mut state.released);
        drop(state);
        for due in released {
            due.send();
        }
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
                state.remove(&id);
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
            artifacts: None,
        });
        let (changes, _) = watch::channel(Arc::clone(&snapshot));
        state.entries.insert(
            snapshot.id.clone(),
            Entry {
                snapshot: Arc::clone(&snapshot),
                owner,
                terminal_at: None,
                cancel_requested: false,
                invocation: None,
                created_at: now,
                waiters: 0,
                changes,
                artifacts: Vec::new(),
                revoked: false,
            },
        );
        Ok(snapshot)
    }

    pub fn get(&self, principal: &Principal, id: &str) -> Result<Arc<JobSnapshot>, JobError> {
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
        let retention = state.retention;
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
        let mut released = Vec::new();
        let mut expiry = None;
        if let Some(outcome) = outcome {
            entry.terminal_at = Some(now);
            entry.invocation = None;
            match outcome {
                TerminalOutcome::Succeeded(value) => {
                    snapshot.state = JobState::Succeeded;
                    snapshot.result = Some(value);
                    if !entry.artifacts.is_empty() {
                        snapshot.artifacts =
                            Some(entry.artifacts.iter().map(|cell| cell.view()).collect());
                        expiry = entry.artifacts[0].host();
                    }
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
            // Only a succeeded job lists its artifacts; any other end
            // releases them at once.
            if entry.snapshot.state != JobState::Succeeded {
                entry.release_artifacts(&mut released);
            }
        } else {
            snapshot.progress = progress;
        }
        entry.changes.send_replace(Arc::clone(&entry.snapshot));
        state.released.extend(released);
        drop(state);
        // A one-shot retention deadline, so an idle registry still releases
        // what backs the artifacts when the job expires; never a sweep.
        if let Some(host) = expiry {
            let registry = Arc::downgrade(&self.inner);
            host.expire_after(
                retention,
                Box::new(move || {
                    if let Some(inner) = registry.upgrade() {
                        inner.lock().cleanup(Instant::now());
                    }
                }),
            );
        }
        Ok(true)
    }

    pub(crate) fn request_cancel(
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

    /// Associates a prepared invocation without retaining the host or the tool
    /// authority. A cancellation racing preparation is forwarded after unlock.
    pub fn bind_invocation(
        &self,
        id: &str,
        executor: &super::InvocationExecutor,
        invocation: super::InvocationId,
    ) -> Result<(), JobError> {
        let cancel = {
            let mut state = self.inner.lock();
            let entry = state.entries.get_mut(id).ok_or(JobError::UnknownJob)?;
            if entry.snapshot.state.is_terminal() {
                return Ok(());
            }
            entry.invocation = Some((executor.host.clone(), invocation));
            entry.cancel_requested
        };
        if cancel {
            executor.cancel(invocation);
        }
        Ok(())
    }

    pub fn cancel(&self, principal: &Principal, id: &str) -> Result<Arc<JobSnapshot>, JobError> {
        let request = self.request_cancel(id, principal)?;
        if request.dispatch {
            let invocation = self
                .inner
                .lock()
                .entries
                .get(id)
                .and_then(|entry| entry.invocation.clone());
            if let Some((host, invocation)) = invocation
                && let Some(host) = host.upgrade()
            {
                host.cancel_action(invocation);
            }
        }
        Ok(request.job)
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

    /// Advisory cancellation of every running job one launch session created,
    /// when core invalidates that session (API v1 "Tool launch sessions"
    /// item 6). Bound invocations are cancelled after the registry lock is
    /// released; an unbound one is cancelled when it binds. Every artifact
    /// of that session's jobs, terminal ones included, is released at once
    /// (AF.12): an open download ends and no later one starts. Returns the
    /// ids of the running jobs asked to cancel.
    pub fn cancel_owner(&self, owner: &JobOwner) -> Vec<String> {
        let principal = Principal::Tool(owner.clone());
        let mut dispatch = Vec::new();
        let ids = {
            let mut state = self.inner.lock();
            state.cleanup(Instant::now());
            let mut released = Vec::new();
            let ids = state
                .entries
                .iter_mut()
                .filter_map(|(id, entry)| {
                    if !matches!(entry.owner, Principal::Tool(_)) || !entry.visible_to(&principal) {
                        return None;
                    }
                    entry.revoked = true;
                    entry.release_artifacts(&mut released);
                    if !entry.snapshot.state.is_terminal() && !entry.cancel_requested {
                        entry.cancel_requested = true;
                        dispatch.extend(entry.invocation.clone());
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();
            state.released.extend(released);
            ids
        };
        for (host, invocation) in dispatch {
            if let Some(host) = host.upgrade() {
                host.cancel_action(invocation);
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests;
