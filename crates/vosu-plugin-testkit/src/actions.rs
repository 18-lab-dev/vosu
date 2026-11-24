//! Instance-owned action control: explicit progress/result barriers and bounded
//! invocation ownership. Dropping/shutting down one fixture affects no other.
use crate::resources::CAPACITY;
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use vosu_plugin_api::{
    ActionInvoke, ActionProgress, ActionResult, ActionStatus, CborValue, Message, vosu_status,
};

#[derive(Debug, Clone, PartialEq)]
pub enum ActionEvent {
    Invoked(ActionInvoke),
    Cancelled(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActionStep {
    Progress { fraction: f64, message: String },
    Success(CborValue),
    Failure { code: String, message: String },
}

#[derive(Debug, Clone)]
pub struct ActionScript {
    pub steps: VecDeque<ActionStep>,
    /// Cancellation remains observable even when deliberately ignored.
    pub ignores_cancel: bool,
}

pub struct ActionFixture {
    scripts: HashMap<String, ActionScript>,
    pending: HashMap<u64, ActionScript>,
    events: SyncSender<ActionEvent>,
    pub invoked: usize,
    pub cancelled: usize,
}

impl ActionFixture {
    pub fn new() -> (Self, Receiver<ActionEvent>) {
        let (events, receiver) = sync_channel(CAPACITY);
        (
            Self {
                scripts: HashMap::new(),
                pending: HashMap::new(),
                events,
                invoked: 0,
                cancelled: 0,
            },
            receiver,
        )
    }

    pub fn script(&mut self, action: String, script: ActionScript) -> Result<(), vosu_status> {
        if script.steps.len() > CAPACITY
            || (!self.scripts.contains_key(&action) && self.scripts.len() == CAPACITY)
        {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT);
        }
        self.scripts.insert(action, script);
        Ok(())
    }

    pub fn invoke(&mut self, invoke: ActionInvoke) -> Result<(), vosu_status> {
        if self.pending.len() == CAPACITY || self.pending.contains_key(&invoke.invocation) {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT);
        }
        let script = self
            .scripts
            .get(&invoke.action)
            .ok_or(vosu_status::VOSU_ERR_INVALID_ARGUMENT)?
            .clone();
        self.events
            .try_send(ActionEvent::Invoked(invoke.clone()))
            .map_err(|_| vosu_status::VOSU_ERR_INVALID_ARGUMENT)?;
        self.pending.insert(invoke.invocation, script);
        self.invoked += 1;
        Ok(())
    }

    /// Release exactly one step. Empty scripts model genuinely unresponsive
    /// actions without spinning, timers, blocking dispatch or leaked workers.
    pub fn release(&mut self, invocation: u64) -> Option<Message> {
        let step = self.pending.get_mut(&invocation)?.steps.pop_front()?;
        let result = match step {
            ActionStep::Progress { fraction, message } => {
                return Some(Message::ActionProgress(ActionProgress {
                    invocation,
                    fraction: Some(fraction),
                    message: Some(message),
                }));
            }
            ActionStep::Success(value) => ActionResult {
                invocation,
                status: ActionStatus::Ok,
                value: Some(value),
                code: None,
                message: None,
            },
            ActionStep::Failure { code, message } => ActionResult {
                invocation,
                status: ActionStatus::Error,
                value: None,
                code: Some(code),
                message: Some(message),
            },
        };
        self.pending.remove(&invocation);
        Some(Message::ActionResult(result))
    }

    pub fn cancel(&mut self, invocation: u64) -> Result<Option<Message>, vosu_status> {
        let script = self
            .pending
            .get(&invocation)
            .ok_or(vosu_status::VOSU_ERR_INVALID_ARGUMENT)?;
        self.events
            .try_send(ActionEvent::Cancelled(invocation))
            .map_err(|_| vosu_status::VOSU_ERR_INVALID_ARGUMENT)?;
        self.cancelled += 1;
        if script.ignores_cancel {
            return Ok(None);
        }
        self.pending.remove(&invocation);
        Ok(Some(Message::ActionResult(ActionResult {
            invocation,
            status: ActionStatus::Error,
            value: None,
            code: Some("cancelled".into()),
            message: Some("fixture cancelled".into()),
        })))
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
    pub fn shutdown(&mut self) {
        self.pending.clear();
    }
}
