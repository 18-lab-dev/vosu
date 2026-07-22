use std::collections::BTreeSet;
use std::env;
use std::io;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tokio::sync::Notify;

use super::Pid;

mod cn_proc;
mod ebpf;

const EVENT_POLL_TIMEOUT: Duration = Duration::from_millis(100);
pub(super) const MAX_PENDING_EVENTS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessEventKind {
    Inspect,
    Exit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProcessEvent {
    pub tgid: Pid,
    pub kind: ProcessEventKind,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProcessEventPoll {
    pub observed: u64,
    pub needs_reconcile: bool,
}

pub(super) trait ProcessEventLoop: Send {
    /// Appends process lifecycle deltas and reports source loss.
    fn poll(
        &mut self,
        timeout: Duration,
        events: &mut Vec<ProcessEvent>,
    ) -> io::Result<ProcessEventPoll>;
}

pub(super) trait ProcessTrackingControl: Send + Sync {
    fn replace_tracked(&self, tracked: &BTreeSet<Pid>) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub(super) struct ProcessEventBatch {
    pub events: Vec<ProcessEvent>,
    pub needs_reconcile: bool,
}

#[derive(Debug, Default)]
struct EventQueue {
    pending: Vec<ProcessEvent>,
    tracked: BTreeSet<Pid>,
    exiting: BTreeSet<Pid>,
    reconcile_untracked_exit: bool,
    needs_reconcile: bool,
}

impl EventQueue {
    fn push(&mut self, event: ProcessEvent) -> bool {
        match event.kind {
            ProcessEventKind::Inspect if self.exiting.contains(&event.tgid) => {
                return self.request_reconcile();
            }
            ProcessEventKind::Inspect if self.tracked.contains(&event.tgid) => return false,
            ProcessEventKind::Inspect => {}
            ProcessEventKind::Exit if !self.tracked.contains(&event.tgid) => {
                if self.reconcile_untracked_exit {
                    return self.request_reconcile();
                }
                let Ok(index) = self
                    .pending
                    .binary_search_by_key(&event.tgid, |pending| pending.tgid)
                else {
                    return false;
                };
                self.pending.remove(index);
                return true;
            }
            ProcessEventKind::Exit => {
                if self.exiting.len() == MAX_PENDING_EVENTS && !self.exiting.contains(&event.tgid) {
                    self.request_reconcile();
                } else {
                    self.exiting.insert(event.tgid);
                }
            }
        }
        let insertion_index =
            match self
                .pending
                .binary_search_by_key(&event.tgid, |pending| pending.tgid)
            {
                Ok(index) => {
                    self.pending[index].kind = event.kind;
                    return true;
                }
                Err(index) => index,
            };
        if self.pending.len() == MAX_PENDING_EVENTS {
            self.needs_reconcile = true;
            return false;
        }
        self.pending.insert(insertion_index, event);
        true
    }

    fn request_reconcile(&mut self) -> bool {
        let newly_latched = !self.needs_reconcile;
        self.needs_reconcile = true;
        newly_latched
    }

    fn drain(&mut self) -> ProcessEventBatch {
        ProcessEventBatch {
            events: self.pending.drain(..).collect(),
            needs_reconcile: std::mem::take(&mut self.needs_reconcile),
        }
    }

    fn replace_tracked(&mut self, tracked: impl IntoIterator<Item = Pid>) {
        self.tracked.clear();
        self.tracked.extend(tracked);
        self.exiting.retain(|tgid| self.tracked.contains(tgid));
    }
}

fn record_reconciliation_request(counter: &AtomicU64, newly_latched: bool) {
    if newly_latched {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

fn candidate_comm(comm: &[u8]) -> bool {
    comm.windows(b"osu!".len()).any(|name| name == b"osu!")
        || comm
            .windows(b"osulazer.exe".len())
            .any(|name| name == b"osulazer.exe")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessEventMode {
    CnProc,
    Ebpf,
    Auto,
}

impl ProcessEventMode {
    fn parse(value: &str) -> io::Result<Option<Self>> {
        match value {
            "" | "off" | "poll" => Ok(None),
            "cn-proc" => Ok(Some(Self::CnProc)),
            "ebpf" => Ok(Some(Self::Ebpf)),
            "auto" => Ok(Some(Self::Auto)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "VOSU_PROCESS_EVENTS must be off, poll, cn-proc, ebpf, or auto; got {value:?}"
                ),
            )),
        }
    }
}

pub struct LinuxProcessEvents {
    mode: ProcessEventMode,
    notify: Arc<Notify>,
    healthy: Arc<AtomicBool>,
    event_count: Arc<AtomicU64>,
    reconciliation_request_count: Arc<AtomicU64>,
    queue: Arc<Mutex<EventQueue>>,
    tracking_control: Option<Arc<dyn ProcessTrackingControl>>,
    cancel: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for LinuxProcessEvents {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinuxProcessEvents")
            .field("mode", &self.mode)
            .field("healthy", &self.healthy())
            .field("event_count", &self.event_count())
            .finish_non_exhaustive()
    }
}

impl LinuxProcessEvents {
    pub fn from_environment() -> Option<Self> {
        let value = env::var("VOSU_PROCESS_EVENTS").unwrap_or_default();
        let mode = match ProcessEventMode::parse(&value) {
            Ok(mode) => mode?,
            Err(error) => {
                tracing::warn!(%error, "invalid Linux process-event mode; using polling");
                return None;
            }
        };
        match Self::open(mode) {
            Ok(events) => {
                tracing::info!(mode = ?events.mode, "Linux process-event wakeups enabled");
                Some(events)
            }
            Err(error) => {
                tracing::warn!(requested = ?mode, %error, "Linux process-event setup failed; using polling");
                None
            }
        }
    }

    pub fn open(mode: ProcessEventMode) -> io::Result<Self> {
        let (mode, backend, tracking_control): (
            ProcessEventMode,
            Box<dyn ProcessEventLoop>,
            Option<Arc<dyn ProcessTrackingControl>>,
        ) = match mode {
            ProcessEventMode::CnProc => (mode, cn_proc::open()?, None),
            ProcessEventMode::Ebpf => {
                let (backend, tracking_control) = ebpf::open()?;
                (mode, backend, Some(tracking_control))
            }
            ProcessEventMode::Auto => match ebpf::open() {
                Ok((backend, tracking_control)) => {
                    (ProcessEventMode::Ebpf, backend, Some(tracking_control))
                }
                Err(ebpf_error) => match cn_proc::open() {
                    Ok(backend) => (ProcessEventMode::CnProc, backend, None),
                    Err(cn_error) => {
                        return Err(io::Error::other(format!(
                            "eBPF unavailable ({ebpf_error}); proc connector unavailable ({cn_error})"
                        )));
                    }
                },
            },
        };

        let notify = Arc::new(Notify::new());
        let healthy = Arc::new(AtomicBool::new(true));
        let event_count = Arc::new(AtomicU64::new(0));
        let reconciliation_request_count = Arc::new(AtomicU64::new(0));
        let queue = Arc::new(Mutex::new(EventQueue {
            reconcile_untracked_exit: mode == ProcessEventMode::Ebpf,
            ..EventQueue::default()
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let loop_notify = Arc::clone(&notify);
        let loop_healthy = Arc::clone(&healthy);
        let loop_event_count = Arc::clone(&event_count);
        let loop_reconciliation_request_count = Arc::clone(&reconciliation_request_count);
        let loop_queue = Arc::clone(&queue);
        let loop_cancel = Arc::clone(&cancel);
        let task = thread::Builder::new()
            .name(format!("vosu-process-events-{mode:?}"))
            .spawn(move || {
                let mut backend = backend;
                let mut events = Vec::with_capacity(MAX_PENDING_EVENTS);
                while !loop_cancel.load(Ordering::Acquire) {
                    events.clear();
                    match backend.poll(EVENT_POLL_TIMEOUT, &mut events) {
                        Ok(poll) => {
                            if poll.observed != 0 {
                                loop_event_count.fetch_add(poll.observed, Ordering::Relaxed);
                            }
                            if events.is_empty() && !poll.needs_reconcile {
                                continue;
                            }
                            let mut queue =
                                loop_queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            let reconcile_was_pending = queue.needs_reconcile;
                            if poll.needs_reconcile {
                                queue.request_reconcile();
                            }
                            let mut accepted = false;
                            for event in events.drain(..) {
                                accepted |= queue.push(event);
                            }
                            let reconcile = !reconcile_was_pending && queue.needs_reconcile;
                            record_reconciliation_request(
                                &loop_reconciliation_request_count,
                                reconcile,
                            );
                            drop(queue);
                            if accepted || reconcile {
                                loop_notify.notify_one();
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "Linux process-event source failed; restoring polling cadence");
                            loop_healthy.store(false, Ordering::Release);
                            let mut queue =
                                loop_queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            let reconcile = queue.request_reconcile();
                            drop(queue);
                            record_reconciliation_request(
                                &loop_reconciliation_request_count,
                                reconcile,
                            );
                            if reconcile {
                                loop_notify.notify_one();
                            }
                            break;
                        }
                    }
                }
            })?;

        Ok(Self {
            mode,
            notify,
            healthy,
            event_count,
            reconciliation_request_count,
            queue,
            tracking_control,
            cancel,
            task: Some(task),
        })
    }

    #[must_use]
    pub fn mode(&self) -> ProcessEventMode {
        self.mode
    }

    #[must_use]
    pub fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn event_count(&self) -> u64 {
        self.event_count.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn reconciliation_request_count(&self) -> u64 {
        self.reconciliation_request_count.load(Ordering::Relaxed)
    }

    pub async fn notified(&self) {
        self.notify.notified().await;
    }

    pub(crate) fn notifier(&self) -> &Notify {
        self.notify.as_ref()
    }

    pub(super) fn drain(&self) -> ProcessEventBatch {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
    }

    pub(super) fn replace_tracked(&self, tracked: impl IntoIterator<Item = Pid>) {
        let tracked = tracked.into_iter().collect::<BTreeSet<_>>();
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(control) = self.tracking_control.as_ref()
            && let Err(error) = control.replace_tracked(&tracked)
        {
            tracing::warn!(%error, "Linux process-event tracking synchronization failed; restoring polling cadence");
            self.healthy.store(false, Ordering::Release);
            let reconcile = queue.request_reconcile();
            drop(queue);
            record_reconciliation_request(&self.reconciliation_request_count, reconcile);
            if reconcile {
                self.notify.notify_one();
            }
            return;
        }

        queue.replace_tracked(tracked);
    }
}

impl Drop for LinuxProcessEvents {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(task) = self.task.take()
            && task.join().is_err()
        {
            tracing::warn!("Linux process-event task panicked during shutdown");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailOnceTrackingControl(AtomicU64);

    impl ProcessTrackingControl for FailOnceTrackingControl {
        fn replace_tracked(&self, _tracked: &BTreeSet<Pid>) -> io::Result<()> {
            if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                Err(io::Error::other("synthetic map failure"))
            } else {
                Ok(())
            }
        }
    }

    fn events_with_tracking_control(
        tracking_control: Arc<dyn ProcessTrackingControl>,
    ) -> LinuxProcessEvents {
        LinuxProcessEvents {
            mode: ProcessEventMode::Ebpf,
            notify: Arc::new(Notify::new()),
            healthy: Arc::new(AtomicBool::new(true)),
            event_count: Arc::new(AtomicU64::new(0)),
            reconciliation_request_count: Arc::new(AtomicU64::new(0)),
            queue: Arc::new(Mutex::new(EventQueue::default())),
            tracking_control: Some(tracking_control),
            cancel: Arc::new(AtomicBool::new(false)),
            task: None,
        }
    }

    #[test]
    fn tracking_control_failure_preserves_queue_and_successful_retry_commits() {
        let control = Arc::new(FailOnceTrackingControl(AtomicU64::new(0)));
        let events = events_with_tracking_control(control.clone());
        events
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace_tracked([3]);

        events.replace_tracked([7]);
        assert!(!events.healthy());
        assert_eq!(events.reconciliation_request_count(), 1);
        assert_eq!(
            events
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tracked,
            BTreeSet::from([3]),
        );

        events.replace_tracked([8]);
        assert_eq!(control.0.load(Ordering::Relaxed), 2);
        assert_eq!(
            events
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .tracked,
            BTreeSet::from([8]),
        );
        assert_eq!(events.reconciliation_request_count(), 1);
        assert!(events.drain().needs_reconcile);
    }

    #[test]
    fn parses_explicit_modes_and_keeps_polling_default() {
        assert_eq!(ProcessEventMode::parse("").unwrap(), None);
        assert_eq!(ProcessEventMode::parse("poll").unwrap(), None);
        assert_eq!(
            ProcessEventMode::parse("cn-proc").unwrap(),
            Some(ProcessEventMode::CnProc)
        );
        assert_eq!(
            ProcessEventMode::parse("ebpf").unwrap(),
            Some(ProcessEventMode::Ebpf)
        );
        assert!(ProcessEventMode::parse("unknown").is_err());
    }

    #[test]
    fn queue_ignores_untracked_exit_and_coalesces_same_tgid() {
        let mut queue = EventQueue::default();
        assert!(!queue.push(ProcessEvent {
            tgid: 8,
            kind: ProcessEventKind::Exit,
        }));
        assert!(queue.push(ProcessEvent {
            tgid: 8,
            kind: ProcessEventKind::Inspect,
        }));
        assert!(queue.push(ProcessEvent {
            tgid: 8,
            kind: ProcessEventKind::Exit,
        }));
        assert!(queue.drain().events.is_empty());
        assert!(queue.push(ProcessEvent {
            tgid: 7,
            kind: ProcessEventKind::Inspect,
        }));
        queue.replace_tracked([7]);
        assert!(queue.push(ProcessEvent {
            tgid: 7,
            kind: ProcessEventKind::Exit,
        }));

        let batch = queue.drain();
        assert_eq!(
            batch.events,
            [ProcessEvent {
                tgid: 7,
                kind: ProcessEventKind::Exit,
            }]
        );
        assert!(!batch.needs_reconcile);
    }

    #[test]
    fn ebpf_queue_reconciles_untracked_exit() {
        let mut queue = EventQueue {
            reconcile_untracked_exit: true,
            ..EventQueue::default()
        };

        assert!(queue.push(ProcessEvent {
            tgid: 8,
            kind: ProcessEventKind::Exit,
        }));
        let batch = queue.drain();
        assert!(batch.events.is_empty());
        assert!(batch.needs_reconcile);
    }

    #[test]
    fn queue_suppresses_tracked_inspects_across_drains() {
        let mut queue = EventQueue::default();
        queue.replace_tracked([7]);

        for _ in 0..2 {
            assert!(!queue.push(ProcessEvent {
                tgid: 7,
                kind: ProcessEventKind::Inspect,
            }));
            assert!(queue.drain().events.is_empty());
        }
    }

    #[test]
    fn queue_preserves_exit_and_reconciles_inspect_conflicts() {
        let mut queue = EventQueue::default();
        queue.replace_tracked([7]);
        assert!(queue.push(ProcessEvent {
            tgid: 7,
            kind: ProcessEventKind::Exit,
        }));
        assert!(queue.push(ProcessEvent {
            tgid: 7,
            kind: ProcessEventKind::Inspect,
        }));

        let batch = queue.drain();
        assert_eq!(
            batch.events,
            [ProcessEvent {
                tgid: 7,
                kind: ProcessEventKind::Exit,
            }]
        );
        assert!(batch.needs_reconcile);

        assert!(queue.push(ProcessEvent {
            tgid: 7,
            kind: ProcessEventKind::Inspect,
        }));
        let batch = queue.drain();
        assert!(batch.events.is_empty());
        assert!(batch.needs_reconcile);
    }

    #[test]
    fn queue_keeps_untracked_inspects_for_generic_rename_discovery() {
        let mut queue = EventQueue::default();

        for _ in 0..2 {
            assert!(queue.push(ProcessEvent {
                tgid: 7,
                kind: ProcessEventKind::Inspect,
            }));
            assert_eq!(
                queue.drain().events,
                [ProcessEvent {
                    tgid: 7,
                    kind: ProcessEventKind::Inspect,
                }]
            );
        }
    }

    #[test]
    fn replacing_tracked_set_discards_removed_exit_state() {
        let mut queue = EventQueue::default();
        queue.replace_tracked([7]);
        assert!(queue.push(ProcessEvent {
            tgid: 7,
            kind: ProcessEventKind::Exit,
        }));
        queue.drain();

        queue.replace_tracked([7, 8]);
        assert!(queue.exiting.contains(&7));
        queue.replace_tracked([8]);
        assert!(!queue.exiting.contains(&7));
        assert!(queue.push(ProcessEvent {
            tgid: 7,
            kind: ProcessEventKind::Inspect,
        }));
        assert_eq!(
            queue.drain().events,
            [ProcessEvent {
                tgid: 7,
                kind: ProcessEventKind::Inspect,
            }]
        );
    }

    #[test]
    fn queue_overflow_latches_one_reconciliation() {
        let mut queue = EventQueue::default();
        for tgid in 1..=MAX_PENDING_EVENTS as Pid {
            assert!(queue.push(ProcessEvent {
                tgid,
                kind: ProcessEventKind::Inspect,
            }));
        }
        assert!(!queue.push(ProcessEvent {
            tgid: MAX_PENDING_EVENTS as Pid + 1,
            kind: ProcessEventKind::Inspect,
        }));
        assert!(queue.needs_reconcile);
        assert!(queue.drain().needs_reconcile);
        assert!(!queue.drain().needs_reconcile);
    }
    #[test]
    fn reconciliation_counter_only_records_new_latches() {
        let counter = AtomicU64::new(0);
        let mut queue = EventQueue::default();

        record_reconciliation_request(&counter, queue.request_reconcile());
        record_reconciliation_request(&counter, queue.request_reconcile());
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        assert!(queue.drain().needs_reconcile);
        record_reconciliation_request(&counter, queue.request_reconcile());
        assert_eq!(counter.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn queue_bounds_exit_tracking_and_reconciles_overflow() {
        let mut queue = EventQueue::default();
        queue.replace_tracked(1..=MAX_PENDING_EVENTS as Pid + 1);

        for tgid in 1..=MAX_PENDING_EVENTS as Pid {
            assert!(queue.push(ProcessEvent {
                tgid,
                kind: ProcessEventKind::Exit,
            }));
        }
        assert!(!queue.push(ProcessEvent {
            tgid: MAX_PENDING_EVENTS as Pid + 1,
            kind: ProcessEventKind::Exit,
        }));

        assert_eq!(queue.exiting.len(), MAX_PENDING_EVENTS);
        assert!(queue.needs_reconcile);
    }
}
