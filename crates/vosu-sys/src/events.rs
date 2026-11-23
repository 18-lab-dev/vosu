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

/// Process id. A plain `u32` alias: the daemon's own `Pid` alias is the
/// identical type, so values cross the crate boundary without conversion.
pub type Pid = u32;

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
pub struct ProcessEventBatch {
    pub inspect: Vec<Pid>,
    pub exit: Vec<Pid>,
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
        let insertion_index = match self
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
        let mut inspect = Vec::with_capacity(self.pending.len());
        let mut exit = Vec::new();
        for event in self.pending.drain(..) {
            match event.kind {
                ProcessEventKind::Inspect => inspect.push(event.tgid),
                ProcessEventKind::Exit => exit.push(event.tgid),
            }
        }
        ProcessEventBatch {
            inspect,
            exit,
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

/// Maximum length, in bytes, of one `comm_hints` substring -- the Linux
/// kernel/CN_PROC `comm` field width minus its NUL terminator
/// (`docs/v2/plugin-abi.md` Sec 6.1; mirrored by `COMM_HINT_MAX_LEN` in
/// `bpf/process_events.bpf.c`).
pub const MAX_COMM_HINT_LEN: usize = 15;

/// Maximum number of `comm_hints` entries accepted at once -- the bounded
/// in-kernel/CN_PROC pre-filter table size (`docs/v2/plugin-abi.md` Sec 6.1;
/// mirrored by `MAX_COMM_HINTS` in `bpf/process_events.bpf.c`).
pub const MAX_COMM_HINTS: usize = 256;

/// A validated, bounded set of `comm` substrings (`docs/v2/plugin-abi.md`
/// Sec 6.1 `comm_hints`) used only to fill the in-kernel eBPF map and the
/// CN_PROC userspace pre-filter; full process matching still runs in
/// userspace against the candidate's full path/argv regardless of what this
/// narrows (core discovery wiring lands in a later unit).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommHints {
    hints: Vec<Vec<u8>>,
}

/// A `comm_hints` entry or set that violates `docs/v2/plugin-abi.md` Sec
/// 6.1's bounds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommHintsError {
    #[error("comm hint at index {index} is empty")]
    Empty { index: usize },
    #[error("comm hint at index {index} is {len} bytes, over the {MAX_COMM_HINT_LEN}-byte limit")]
    TooLong { index: usize, len: usize },
    #[error("{count} comm hints exceed the bounded pre-filter capacity of {MAX_COMM_HINTS}")]
    TooMany { count: usize },
}

impl CommHints {
    /// Validates every hint (non-empty, at most [`MAX_COMM_HINT_LEN`] bytes,
    /// at most [`MAX_COMM_HINTS`] entries total), then deduplicates and
    /// sorts them for deterministic iteration and diffing.
    pub fn new<I, S>(hints: I) -> Result<Self, CommHintsError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        let mut collected: Vec<Vec<u8>> = Vec::new();
        for (index, hint) in hints.into_iter().enumerate() {
            let hint = hint.as_ref();
            if hint.is_empty() {
                return Err(CommHintsError::Empty { index });
            }
            if hint.len() > MAX_COMM_HINT_LEN {
                return Err(CommHintsError::TooLong {
                    index,
                    len: hint.len(),
                });
            }
            collected.push(hint.to_vec());
        }
        if collected.len() > MAX_COMM_HINTS {
            return Err(CommHintsError::TooMany {
                count: collected.len(),
            });
        }
        collected.sort_unstable();
        collected.dedup();
        Ok(Self { hints: collected })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hints.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.hints.len()
    }

    /// Iterates the validated hints in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        self.hints.iter().map(Vec::as_slice)
    }

    fn contains(&self, hint: &[u8]) -> bool {
        self.hints
            .binary_search_by(|existing| existing.as_slice().cmp(hint))
            .is_ok()
    }

    /// Reports whether `comm` contains any hint as a substring -- the exact
    /// matching rule `docs/v2/plugin-abi.md` Sec 6.1 and the former
    /// hard-coded `osu!`/`osulazer.exe` filter both use.
    #[must_use]
    pub fn matches(&self, comm: &[u8]) -> bool {
        self.hints.iter().any(|hint| {
            comm.windows(hint.len())
                .any(|window| window == hint.as_slice())
        })
    }
}

/// Live backend-specific sink for [`CommHints`] updates, mirroring
/// [`ProcessTrackingControl`]'s split between construction-time seeding
/// (`LinuxProcessEvents::open`) and later live updates
/// (`LinuxProcessEvents::set_comm_hints`).
pub(super) trait ProcessCommFilter: Send + Sync {
    fn set_comm_hints(&self, hints: &CommHints) -> io::Result<()>;
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

    fn configured(value: Option<&str>) -> io::Result<Option<Self>> {
        match value {
            Some(value) => Self::parse(value),
            None => Ok(Some(Self::Auto)),
        }
    }
    fn backend_name(self) -> &'static str {
        match self {
            Self::CnProc => "CN_PROC",
            Self::Ebpf => "eBPF",
            Self::Auto => unreachable!("Auto process-event mode must resolve before storage"),
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
    comm_filter: Arc<dyn ProcessCommFilter>,
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
    pub fn from_environment(comm_hints: &CommHints) -> Option<Self> {
        let configured = match env::var("VOSU_PROCESS_EVENTS") {
            Ok(value) => ProcessEventMode::configured(Some(&value)),
            Err(env::VarError::NotPresent) => ProcessEventMode::configured(None),
            Err(env::VarError::NotUnicode(_)) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "VOSU_PROCESS_EVENTS must be valid UTF-8",
            )),
        };
        let mode = match configured {
            Ok(mode) => mode?,
            Err(error) => {
                tracing::warn!(%error, "invalid Linux process-event mode; using polling");
                return None;
            }
        };
        match Self::open(mode, comm_hints) {
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

    pub fn open(mode: ProcessEventMode, comm_hints: &CommHints) -> io::Result<Self> {
        type OpenedBackend = (
            ProcessEventMode,
            Box<dyn ProcessEventLoop>,
            Option<Arc<dyn ProcessTrackingControl>>,
            Arc<dyn ProcessCommFilter>,
        );
        let (mode, backend, tracking_control, comm_filter): OpenedBackend = match mode {
            ProcessEventMode::CnProc => {
                let (backend, comm_filter) = cn_proc::open(comm_hints)?;
                (mode, backend, None, comm_filter)
            }
            ProcessEventMode::Ebpf => {
                let (backend, tracking_control, comm_filter) = ebpf::open(comm_hints)?;
                (mode, backend, Some(tracking_control), comm_filter)
            }
            ProcessEventMode::Auto => match ebpf::open(comm_hints) {
                Ok((backend, tracking_control, comm_filter)) => (
                    ProcessEventMode::Ebpf,
                    backend,
                    Some(tracking_control),
                    comm_filter,
                ),
                Err(ebpf_error) => match cn_proc::open(comm_hints) {
                    Ok((backend, comm_filter)) => {
                        (ProcessEventMode::CnProc, backend, None, comm_filter)
                    }
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
                            let events_were_pending = !queue.pending.is_empty();
                            for event in events.drain(..) {
                                queue.push(event);
                            }
                            let accepted = !events_were_pending && !queue.pending.is_empty();
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
            comm_filter,
            cancel,
            task: Some(task),
        })
    }

    #[must_use]
    pub fn mode(&self) -> ProcessEventMode {
        self.mode
    }

    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        self.mode.backend_name()
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

    pub fn notifier(&self) -> &Notify {
        self.notify.as_ref()
    }

    pub fn drain(&self) -> ProcessEventBatch {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
    }

    pub fn replace_tracked(&self, tracked: impl IntoIterator<Item = Pid>) {
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

    pub fn set_comm_hints(&self, hints: &CommHints) -> io::Result<()> {
        if let Err(error) = self.comm_filter.set_comm_hints(hints) {
            tracing::warn!(%error, "Linux process-event comm-hint update failed; restoring polling cadence");
            self.healthy.store(false, Ordering::Release);
            let mut queue = self
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let reconcile = queue.request_reconcile();
            drop(queue);
            record_reconciliation_request(&self.reconciliation_request_count, reconcile);
            if reconcile {
                self.notify.notify_one();
            }
            return Err(error);
        }
        Ok(())
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

    struct NoopCommFilter;

    impl ProcessCommFilter for NoopCommFilter {
        fn set_comm_hints(&self, _hints: &CommHints) -> io::Result<()> {
            Ok(())
        }
    }

    struct FailOnceCommFilter(AtomicU64);

    impl ProcessCommFilter for FailOnceCommFilter {
        fn set_comm_hints(&self, _hints: &CommHints) -> io::Result<()> {
            if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                Err(io::Error::other("synthetic comm-filter failure"))
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
            comm_filter: Arc::new(NoopCommFilter),
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
    fn absent_mode_uses_auto_while_explicit_polling_stays_disabled() {
        assert_eq!(
            ProcessEventMode::configured(None).unwrap(),
            Some(ProcessEventMode::Auto)
        );
        for value in ["", "off", "poll"] {
            assert_eq!(ProcessEventMode::configured(Some(value)).unwrap(), None);
        }
        assert_eq!(
            ProcessEventMode::configured(Some("cn-proc")).unwrap(),
            Some(ProcessEventMode::CnProc)
        );
        assert_eq!(
            ProcessEventMode::configured(Some("ebpf")).unwrap(),
            Some(ProcessEventMode::Ebpf)
        );
        assert!(ProcessEventMode::configured(Some("unknown")).is_err());
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
        let batch = queue.drain();
        assert!(batch.inspect.is_empty());
        assert!(batch.exit.is_empty());
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
        assert_eq!(batch.exit, [7]);
        assert!(batch.inspect.is_empty());
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
        assert!(batch.inspect.is_empty());
        assert!(batch.exit.is_empty());
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
            let batch = queue.drain();
            assert!(batch.inspect.is_empty());
            assert!(batch.exit.is_empty());
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
        assert_eq!(batch.exit, [7]);
        assert!(batch.inspect.is_empty());
        assert!(batch.needs_reconcile);

        assert!(queue.push(ProcessEvent {
            tgid: 7,
            kind: ProcessEventKind::Inspect,
        }));
        let batch = queue.drain();
        assert!(batch.inspect.is_empty());
        assert!(batch.exit.is_empty());
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
            let batch = queue.drain();
            assert_eq!(batch.inspect, [7]);
            assert!(batch.exit.is_empty());
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
        let batch = queue.drain();
        assert_eq!(batch.inspect, [7]);
        assert!(batch.exit.is_empty());
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

    fn events_with_comm_filter(comm_filter: Arc<dyn ProcessCommFilter>) -> LinuxProcessEvents {
        LinuxProcessEvents {
            mode: ProcessEventMode::Ebpf,
            notify: Arc::new(Notify::new()),
            healthy: Arc::new(AtomicBool::new(true)),
            event_count: Arc::new(AtomicU64::new(0)),
            reconciliation_request_count: Arc::new(AtomicU64::new(0)),
            queue: Arc::new(Mutex::new(EventQueue::default())),
            tracking_control: None,
            comm_filter,
            cancel: Arc::new(AtomicBool::new(false)),
            task: None,
        }
    }

    #[test]
    fn comm_filter_failure_preserves_health_and_successful_retry_clears_it() {
        let filter = Arc::new(FailOnceCommFilter(AtomicU64::new(0)));
        let events = events_with_comm_filter(filter.clone());
        let hints = CommHints::new(["osu!"]).unwrap();

        assert!(events.set_comm_hints(&hints).is_err());
        assert!(!events.healthy());
        assert_eq!(events.reconciliation_request_count(), 1);

        assert!(events.set_comm_hints(&hints).is_ok());
        assert_eq!(filter.0.load(Ordering::Relaxed), 2);
        assert_eq!(events.reconciliation_request_count(), 1);
        assert!(events.drain().needs_reconcile);
    }

    #[test]
    fn comm_hints_reject_empty_overlong_and_oversized_sets() {
        assert_eq!(
            CommHints::new([""]).unwrap_err(),
            CommHintsError::Empty { index: 0 }
        );
        assert_eq!(
            CommHints::new(["osu!", ""]).unwrap_err(),
            CommHintsError::Empty { index: 1 }
        );

        let max_len_hint = vec![b'a'; MAX_COMM_HINT_LEN];
        assert_eq!(CommHints::new([max_len_hint]).unwrap().len(), 1);

        let over_len_hint = vec![b'a'; MAX_COMM_HINT_LEN + 1];
        assert_eq!(
            CommHints::new([over_len_hint]).unwrap_err(),
            CommHintsError::TooLong {
                index: 0,
                len: MAX_COMM_HINT_LEN + 1,
            }
        );

        let too_many: Vec<String> = (0..=MAX_COMM_HINTS)
            .map(|index| format!("h{index}"))
            .collect();
        assert_eq!(
            CommHints::new(too_many).unwrap_err(),
            CommHintsError::TooMany {
                count: MAX_COMM_HINTS + 1,
            }
        );

        let exactly_bounded: Vec<String> = (0..MAX_COMM_HINTS)
            .map(|index| format!("h{index}"))
            .collect();
        assert_eq!(
            CommHints::new(exactly_bounded).unwrap().len(),
            MAX_COMM_HINTS
        );
    }

    #[test]
    fn comm_hints_dedupe_and_match_substrings_like_the_former_hardcoded_filter() {
        let hints = CommHints::new(["osu!", "osulazer.exe", "osu!"]).unwrap();
        assert_eq!(hints.len(), 2);

        assert!(hints.matches(b"bench-osu!-away"));
        assert!(hints.matches(b"osulazer.exe"));
        assert!(!hints.matches(b"unrelated"));
        assert!(!hints.matches(b""));
        assert!(!CommHints::default().matches(b"osu!"));
    }
}
