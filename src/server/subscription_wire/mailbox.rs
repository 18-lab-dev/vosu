use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bytes::Bytes;
use tokio::sync::Notify;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outbound {
    /// Initial or update frame; the transport must call `write_completed` after sending it.
    Text(Bytes),
    /// Application error frame. It is always followed by `Close`.
    Error(Bytes),
    Pong(Bytes),
    Close {
        code: u16,
    },
}

/// A client's outbound queues behind one lock and one wakeup, drained terminal first, then
/// control, then data. Every queue is bounded: terminal frames are queued once per client
/// (`cleaned`), and an occupied control or data slot refuses the frame. Delivery reserves the
/// data slot (`PENDING`) until `write_completed`, so data never needs a second slot.
pub struct Mailbox {
    state: Mutex<MailboxState>,
    ready: Notify,
}

#[derive(Default)]
struct MailboxState {
    /// Queued in order; the second entry is occupied only while the first is.
    terminal: [Option<Outbound>; 2],
    control: Option<Outbound>,
    data: Option<Outbound>,
    /// The receiver is gone; frames are refused like sends to a closed channel.
    receiver_closed: bool,
    /// The client is gone; the receiver ends once the queued frames are drained.
    senders_closed: bool,
    /// The receiver found nothing queued and may be waiting. Only the first frame or close
    /// after that notifies: the woken receiver checks every queue before it waits again.
    receiver_waiting: bool,
}

impl MailboxState {
    fn pop(&mut self) -> Option<Outbound> {
        if let Some(frame) = self.terminal[0].take() {
            self.terminal[0] = self.terminal[1].take();
            return Some(frame);
        }
        self.control.take().or_else(|| self.data.take())
    }
}

impl Default for Mailbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Mailbox {
    /// Marks the sole owning client gone; drain queued frames before ending the stream.
    pub fn close_senders(&self) {
        let wake = {
            let mut state = lock(&self.state);
            state.senders_closed = true;
            std::mem::take(&mut state.receiver_waiting)
        };
        if wake {
            self.ready.notify_one();
        }
    }

    pub fn new() -> Self {
        Self {
            state: Mutex::new(MailboxState::default()),
            ready: Notify::new(),
        }
    }

    /// Queues `frame` into the slot `select` picks unless it is occupied or the receiver is
    /// gone.
    fn try_send(
        &self,
        select: impl FnOnce(&mut MailboxState) -> Option<&mut Option<Outbound>>,
        frame: Outbound,
    ) -> Result<(), Outbound> {
        let mut state = lock(&self.state);
        if state.receiver_closed {
            return Err(frame);
        }
        let Some(slot @ None) = select(&mut state) else {
            return Err(frame);
        };
        *slot = Some(frame);
        let wake = std::mem::take(&mut state.receiver_waiting);
        drop(state);
        if wake {
            // Stores a permit when the receiver has not started waiting yet, so the wakeup
            // cannot be lost.
            self.ready.notify_one();
        }
        Ok(())
    }

    pub fn try_send_data(&self, frame: Outbound) -> Result<(), Outbound> {
        self.try_send(|state| Some(&mut state.data), frame)
    }

    pub fn try_send_control(&self, frame: Outbound) -> bool {
        self.try_send(|state| Some(&mut state.control), frame)
            .is_ok()
    }

    pub fn try_send_terminal(&self, frame: Outbound) {
        let _ = self.try_send(
            |state| state.terminal.iter_mut().find(|slot| slot.is_none()),
            frame,
        );
    }
}

pub struct Receiver {
    mailbox: Arc<Mailbox>,
}

impl Receiver {
    pub fn new(mailbox: Arc<Mailbox>) -> Self {
        Self { mailbox }
    }

    pub async fn recv(&mut self) -> Option<Outbound> {
        loop {
            // A queued frame needs no wait and charges no coop budget. Skipping the budget
            // cannot starve the runtime: at most four frames can be queued.
            {
                let mut state = lock(&self.mailbox.state);
                if let Some(frame) = state.pop() {
                    return Some(frame);
                }
                if state.senders_closed {
                    return None;
                }
                state.receiver_waiting = true;
            }
            // A send between the unlock and this wait sees `receiver_waiting` and leaves a
            // permit, so it is not lost.
            self.mailbox.ready.notified().await;
        }
    }

    pub fn try_recv(&mut self) -> Option<Outbound> {
        lock(&self.mailbox.state).pop()
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let mut state = lock(&self.mailbox.state);
        state.receiver_closed = true;
        state.terminal = [None, None];
        state.control = None;
        state.data = None;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
