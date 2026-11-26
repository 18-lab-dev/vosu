//! Bounded ordinary jobs, capacity-reserved terminal work, and lifecycle
//! terminals admitted past the ordinary bound in FIFO order. No polling.
use super::{
    dispatch::{DispatchTask, Job},
    error::HostError,
};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, Weak};

pub(crate) struct DispatchQueue {
    state: Mutex<State>,
    ready: Condvar,
    capacity: usize,
}
#[derive(Default)]
struct State {
    /// Ordinary jobs and lifecycle terminals, in arrival order. The flag
    /// marks a lifecycle terminal, which never counts against `capacity`.
    ordinary: VecDeque<(Job, bool)>,
    /// Lifecycle terminals currently in `ordinary`.
    lifecycle: usize,
    reserved: VecDeque<Job>,
    closed: bool,
}
impl DispatchQueue {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
            capacity: capacity.max(1),
        })
    }
    pub(crate) fn submit(&self, job: Job) -> Result<(), HostError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed || state.ordinary.len() - state.lifecycle >= self.capacity {
            return Err(HostError::DispatchQueueFull);
        }
        state.ordinary.push_back((job, false));
        self.ready.notify_one();
        Ok(())
    }
    /// Only `LifecycleLane` may submit here: at most one job per session
    /// it tracks, so this lane is bounded by the sessions an instance knows,
    /// not by `capacity`. The job keeps its FIFO position among ordinary
    /// jobs, so a session's earlier deliveries still run before it.
    pub(crate) fn submit_lifecycle(&self, job: Job) -> Result<(), HostError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.closed {
            return Err(HostError::DispatchQueueFull);
        }
        state.ordinary.push_back((job, true));
        state.lifecycle += 1;
        self.ready.notify_one();
        Ok(())
    }
    // Only ReservedOutbox may submit here: one runner per bounded instance outbox.
    pub(crate) fn submit_reserved(&self, job: Job) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state.closed {
            state.reserved.push_back(job);
            self.ready.notify_one();
        }
    }
    pub(crate) fn recv(&self) -> Option<Job> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if let Some(job) = state.reserved.pop_front() {
                return Some(job);
            }
            if let Some((job, lifecycle)) = state.ordinary.pop_front() {
                state.lifecycle -= usize::from(lifecycle);
                return Some(job);
            }
            if state.closed {
                return None;
            }
            state = self.ready.wait(state).unwrap_or_else(|p| p.into_inner());
        }
    }
    pub(crate) fn close(&self) {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).closed = true;
        self.ready.notify_all();
    }
}

/// One instance's admission of lifecycle terminals: a job that delivers a
/// session's `SESSION_DETACHED` (`docs/v2/plugin-abi.md` §7.6 "Lifecycle
/// terminals are reserved"). Each session has at most one such job queued
/// or running, so the lane holds at most one entry per session the
/// instance was told about. That bound is the instance's session count,
/// never the ordinary queue's capacity, and a full ordinary queue never
/// refuses it.
///
/// One pointer wide, because every `PluginInstance` holds one.
pub(crate) struct LifecycleLane(Arc<Lane>);

struct Lane {
    queue: Arc<DispatchQueue>,
    pending: Mutex<HashSet<u64>>,
}

impl LifecycleLane {
    pub(crate) fn new(queue: Arc<DispatchQueue>) -> Self {
        Self(Arc::new(Lane {
            queue,
            pending: Mutex::default(),
        }))
    }

    /// Queues `task`, which delivers `session`'s lifecycle terminal, after
    /// every job already queued. `build` makes the job around the wrapped
    /// task. Refused only when `session` already has a terminal pending
    /// here (a duplicate never takes a second entry) or the queue closed
    /// because the instance is being torn down.
    pub(crate) fn submit(
        &self,
        session: u64,
        task: DispatchTask,
        build: impl FnOnce(DispatchTask) -> Job,
    ) -> Result<(), HostError> {
        if !self
            .0
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(session)
        {
            return Err(HostError::DispatchQueueFull);
        }
        // Held until the job has run, or is dropped unrun.
        let claim = Claim {
            lane: Arc::downgrade(&self.0),
            session,
        };
        self.0.queue.submit_lifecycle(build(Box::new(move |target| {
            let _claim = claim;
            task(target);
        })))
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.0
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len()
    }
}

struct Claim {
    lane: Weak<Lane>,
    session: u64,
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Some(lane) = self.lane.upgrade() {
            lane.pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&self.session);
        }
    }
}
