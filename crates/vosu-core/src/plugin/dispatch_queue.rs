//! Bounded ordinary jobs plus capacity-reserved terminal work. No polling.
use super::{dispatch::Job, error::HostError};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

pub(crate) struct DispatchQueue {
    state: Mutex<State>,
    ready: Condvar,
    capacity: usize,
}
#[derive(Default)]
struct State {
    ordinary: VecDeque<Job>,
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
        if state.closed || state.ordinary.len() == self.capacity {
            return Err(HostError::DispatchQueueFull);
        }
        state.ordinary.push_back(job);
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
            if let Some(job) = state
                .reserved
                .pop_front()
                .or_else(|| state.ordinary.pop_front())
            {
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
