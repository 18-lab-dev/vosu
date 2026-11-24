//! Instance-owned resource scripts. No global registry, worker, or ordering sleeps.
//! A held reply is an explicit barrier: dispatch records the request, then the
//! test calls `release` and sends the returned envelopes through its own host.
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use vosu_plugin_api::{Message, vosu_status};

pub const CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub message: Message,
    pub correlation: u64,
}

/// A single reply, optionally withheld or deliberately miscorrelated.
#[derive(Debug, Clone)]
pub struct Reply {
    pub message: Message,
    pub correlation: Option<u64>,
    pub held: bool,
}

impl Reply {
    pub fn immediate(message: Message) -> Self {
        Self {
            message,
            correlation: None,
            held: false,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResourceCounters {
    pub resolve: usize,
    pub list: usize,
    pub lookup: usize,
    pub read: usize,
    pub close: usize,
}

/// Each script step checks the *entire* expected request, including opaque key,
/// session, stream, offset and length. Empty steps deliberately do not answer.
#[derive(Debug)]
pub struct ResourceStep {
    pub request: Message,
    pub replies: Vec<Reply>,
}

pub struct ResourceFixture {
    steps: VecDeque<ResourceStep>,
    held: VecDeque<Envelope>,
    events: SyncSender<Envelope>,
    counters: ResourceCounters,
}

impl ResourceFixture {
    pub fn new() -> (Self, Receiver<Envelope>) {
        let (events, receiver) = sync_channel(CAPACITY);
        (
            Self {
                steps: VecDeque::new(),
                held: VecDeque::new(),
                events,
                counters: ResourceCounters::default(),
            },
            receiver,
        )
    }

    pub fn push(&mut self, step: ResourceStep) -> Result<(), vosu_status> {
        if self.steps.len() == CAPACITY || step.replies.len() > CAPACITY {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT);
        }
        self.steps.push_back(step);
        Ok(())
    }

    pub fn counters(&self) -> ResourceCounters {
        self.counters
    }
    pub fn pending(&self) -> usize {
        self.held.len()
    }
    pub fn remaining(&self) -> usize {
        self.steps.len()
    }

    pub fn dispatch(
        &mut self,
        request: Message,
        correlation: u64,
    ) -> Result<Vec<Envelope>, vosu_status> {
        let Some(step) = self.steps.front() else {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT);
        };
        if step.request != request
            || self.held.len() + step.replies.iter().filter(|r| r.held).count() > CAPACITY
        {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT);
        }
        // A full real event queue refuses before consuming a script step.
        self.events
            .try_send(Envelope {
                message: request.clone(),
                correlation,
            })
            .map_err(|e| match e {
                TrySendError::Full(_) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
                TrySendError::Disconnected(_) => vosu_status::VOSU_ERR_CANCELLED,
            })?;
        match request {
            Message::ResourceResolve(_) => self.counters.resolve += 1,
            Message::TreeList(_) => self.counters.list += 1,
            Message::TreeLookup(_) => self.counters.lookup += 1,
            Message::StreamRead(_) => self.counters.read += 1,
            Message::StreamClose(_) => self.counters.close += 1,
            _ => return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT),
        }
        let step = self.steps.pop_front().expect("checked front");
        let mut ready = Vec::with_capacity(step.replies.len());
        for reply in step.replies {
            let envelope = Envelope {
                message: reply.message,
                correlation: reply.correlation.unwrap_or(correlation),
            };
            if reply.held {
                self.held.push_back(envelope);
            } else {
                ready.push(envelope);
            }
        }
        Ok(ready)
    }

    /// FIFO release is deterministic and never waits. Save the returned envelope
    /// to send it after detach/timeout, or through a different plugin instance.
    pub fn release(&mut self) -> Option<Envelope> {
        self.held.pop_front()
    }

    pub fn shutdown(&mut self) {
        self.held.clear();
        self.steps.clear();
    }
}
