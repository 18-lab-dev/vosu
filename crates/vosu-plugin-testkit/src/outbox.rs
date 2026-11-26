//! The plugin end of a test's bounded event channel. Host dispatch never
//! waits for a test to drain it, yet a full channel must not lose the
//! latest state an observer converges on (ADR 0002, `docs/v2/plugin-abi.md`
//! §6.12): an event that does not fit waits in a backlog instead. Latest-only
//! events (state updates, focus, demand) merge into the same session's
//! queued one, as the host's own delivery slot does, so the backlog stays
//! bounded by sessions and lifecycle events rather than by message rate.
//! Every send flushes the backlog first, in order, and one testkit thread
//! flushes every live backlog on a short tick, so what waited reaches the
//! test once it drains, even after the last message.

use std::collections::VecDeque;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

/// Events waiting beyond the channel's own capacity; only past it is a new
/// event dropped.
const BACKLOG: usize = 256;
const TICK: Duration = Duration::from_millis(5);

/// How a new event relates to one already waiting, scanning from the newest.
pub(crate) enum Merge<E> {
    /// Folded into the waiting event, which now carries the latest state.
    Merged,
    /// Another session's event of the same latest-only kind: look further back.
    Skip(E),
    /// Order matters across these two: queue the new event after it.
    Stop(E),
}

pub(crate) trait Coalesce: Sized {
    /// Merges `next` into `self` when both carry one session's latest-only
    /// state, keeping the newest value per key.
    fn merge(&mut self, next: Self) -> Merge<Self>;
}

pub(crate) struct Outbox<E: Coalesce + Send + 'static>(Arc<Inner<E>>);

struct Inner<E> {
    events: SyncSender<E>,
    backlog: Mutex<VecDeque<E>>,
}

impl<E: Coalesce + Send + 'static> Clone for Outbox<E> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

trait Flush: Send + Sync {
    fn flush(&self);
}

impl<E: Coalesce + Send> Flush for Inner<E> {
    fn flush(&self) {
        flush(
            &self.events,
            &mut self.backlog.lock().unwrap_or_else(|p| p.into_inner()),
        );
    }
}

/// Every outbox created so far, flushed by one thread for the process's life.
static LIVE: LazyLock<Mutex<Vec<Weak<dyn Flush>>>> = LazyLock::new(|| {
    std::thread::spawn(|| {
        loop {
            std::thread::park_timeout(TICK);
            let live = {
                let mut live = LIVE.lock().unwrap_or_else(|p| p.into_inner());
                live.retain(|outbox| outbox.strong_count() > 0);
                live.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
            };
            for outbox in live {
                outbox.flush();
            }
        }
    });
    Mutex::new(Vec::new())
});

impl<E: Coalesce + Send + 'static> Outbox<E> {
    pub(crate) fn new(events: SyncSender<E>) -> Self {
        let inner = Arc::new(Inner {
            events,
            backlog: Mutex::new(VecDeque::new()),
        });
        let flush: Arc<dyn Flush> = inner.clone();
        LIVE.lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Arc::downgrade(&flush));
        Self(inner)
    }

    pub(crate) fn send(&self, event: E) {
        let mut backlog = self.0.backlog.lock().unwrap_or_else(|p| p.into_inner());
        flush(&self.0.events, &mut backlog);
        let mut event = if backlog.is_empty() {
            match self.0.events.try_send(event) {
                Ok(()) | Err(TrySendError::Disconnected(_)) => return,
                Err(TrySendError::Full(event)) => event,
            }
        } else {
            event
        };
        let event = 'scan: {
            for queued in backlog.iter_mut().rev() {
                event = match queued.merge(event) {
                    Merge::Merged => return,
                    Merge::Skip(event) => event,
                    Merge::Stop(event) => break 'scan event,
                };
            }
            event
        };
        if backlog.len() < BACKLOG {
            backlog.push_back(event);
        }
    }
}

fn flush<E>(events: &SyncSender<E>, backlog: &mut VecDeque<E>) {
    while let Some(event) = backlog.pop_front() {
        match events.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(event)) => {
                backlog.push_front(event);
                return;
            }
            Err(TrySendError::Disconnected(_)) => {
                backlog.clear();
                return;
            }
        }
    }
}
