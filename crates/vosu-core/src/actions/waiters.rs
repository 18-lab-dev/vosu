//! Atomic waiter reservation and RAII release, including abandoned HTTP requests.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use super::jobs::{JobError, JobPrincipal, JobRegistry, JobSnapshot, RegistryInner};

pub const MAX_WAITERS_PER_JOB: usize = 8;
pub const MAX_WAITERS: usize = 256;
pub const MAX_WAIT: Duration = Duration::from_secs(30);

pub enum JobWait {
    Immediate(Arc<JobSnapshot>),
    Waiting(WaiterGuard),
}

impl JobWait {
    /// Always returns the latest observed job, including an unchanged timeout.
    pub async fn wait(self) -> Arc<JobSnapshot> {
        match self {
            Self::Immediate(snapshot) => snapshot,
            Self::Waiting(guard) => guard.wait().await,
        }
    }
}

pub struct WaiterGuard {
    registry: Weak<RegistryInner>,
    id: String,
    receiver: watch::Receiver<Arc<JobSnapshot>>,
    after: u64,
    until: tokio::time::Instant,
}

impl WaiterGuard {
    pub async fn wait(mut self) -> Arc<JobSnapshot> {
        loop {
            let snapshot = Arc::clone(&self.receiver.borrow_and_update());
            if snapshot.state.is_terminal() || snapshot.revision > self.after {
                return snapshot;
            }
            match tokio::time::timeout_at(self.until, self.receiver.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) | Err(_) => return Arc::clone(&self.receiver.borrow()),
            }
        }
    }
}

impl Drop for WaiterGuard {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut state = registry.lock();
            state.waiters -= 1;
            if let Some(entry) = state.entries.get_mut(&self.id) {
                entry.waiters -= 1;
            }
        }
    }
}

impl JobRegistry {
    /// Snapshot check and reservation share one lock, so progress cannot be lost
    /// between checking `after` and subscribing. Overflow is a normal read.
    pub fn poll(
        &self,
        id: &str,
        principal: &Principal,
        after: u64,
        wait: Duration,
    ) -> Result<JobWait, JobError> {
        let until = tokio::time::Instant::now() + wait.min(MAX_WAIT);
        let mut state = self.inner.lock();
        state.cleanup(Instant::now());
        let global_full = state.waiters == MAX_WAITERS;
        let entry = state
            .entries
            .get_mut(id)
            .filter(|entry| entry.visible_to(principal))
            .ok_or(JobError::UnknownJob)?;
        if wait.is_zero()
            || entry.snapshot.state.is_terminal()
            || entry.snapshot.revision > after
            || global_full
            || entry.waiters == MAX_WAITERS_PER_JOB
        {
            return Ok(JobWait::Immediate(Arc::clone(&entry.snapshot)));
        }
        entry.waiters += 1;
        let guard = WaiterGuard {
            registry: Arc::downgrade(&self.inner),
            id: id.to_owned(),
            receiver: entry.changes.subscribe(),
            after,
            until,
        };
        state.waiters += 1;
        Ok(JobWait::Waiting(guard))
    }
}
