//! One-shot invocation deadlines; no retention sweep or idle background worker.

use std::sync::Arc;
use std::time::Duration;

use super::jobs::{JobError, JobFailure, JobRegistry, TerminalOutcome};

#[derive(Debug, Clone, Copy)]
pub struct DeadlinePolicy {
    pub timeout: Duration,
}

impl Default for DeadlinePolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

/// Owned by the invocation slot. Dropping it cancels the timer, not the job.
/// No join is necessary: the task holds only a weak registry and a watch receiver.
pub struct DeadlineGuard(tokio::task::JoinHandle<()>);

impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl DeadlinePolicy {
    /// Call in a Tokio runtime after reserving the invocation, before dispatch.
    /// Expiry is measured from job creation, not from a later HTTP poll/arm.
    pub fn arm(self, registry: &JobRegistry, id: &str) -> Result<DeadlineGuard, JobError> {
        let state = registry.inner.lock();
        let entry = state.entries.get(id).ok_or(JobError::UnknownJob)?;
        let deadline = tokio::time::Instant::from_std(entry.created_at) + self.timeout;
        let mut changes = entry.changes.subscribe();
        let weak = Arc::downgrade(&registry.inner);
        let id = id.to_owned();
        drop(state);
        Ok(DeadlineGuard(tokio::spawn(async move {
            let timer = tokio::time::sleep_until(deadline);
            tokio::pin!(timer);
            loop {
                if changes.borrow_and_update().state.is_terminal() {
                    return;
                }
                tokio::select! {
                    () = &mut timer => {
                        if let Some(inner) = weak.upgrade() {
                            let registry = JobRegistry { inner };
                            let _ = registry.finish(&id, TerminalOutcome::Failed(JobFailure {
                                code: "timed_out".into(), message: "Action deadline elapsed".into(),
                            }));
                        }
                        return;
                    }
                    changed = changes.changed() => if changed.is_err() { return; },
                }
            }
        })))
    }
}
