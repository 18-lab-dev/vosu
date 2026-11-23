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
    #[error(
        "comm hint at index {index} contains a NUL byte, which would match every \
         process (the kernel/CN_PROC `comm` field is always NUL-padded)"
    )]
    ContainsNul { index: usize },
    #[error("{count} comm hints exceed the bounded pre-filter capacity of {MAX_COMM_HINTS}")]
    TooMany { count: usize },
}

impl CommHints {
    /// Validates every hint (non-empty, at most [`MAX_COMM_HINT_LEN`] bytes,
    /// no NUL byte, at most [`MAX_COMM_HINTS`] entries total), then
    /// deduplicates and sorts them for deterministic iteration and diffing.
    ///
    /// A NUL byte is rejected rather than matched literally: every `comm`
    /// source (`bpf_get_current_comm`, the kernel string-read helpers, and
    /// CN_PROC's fixed-width field) is NUL-padded, so a hint containing one
    /// -- e.g. a bare `"\0"` -- would match every process, and a trailing
    /// `"x\0"` would act as a silent suffix anchor, contradicting the
    /// substring rule in `docs/v2/plugin-abi.md` Sec 6.1.
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
            if hint.contains(&0) {
                return Err(CommHintsError::ContainsNul { index });
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

/// Backend hook for one comm-hint insert or remove into an underlying map
/// that has no atomic bulk-replace, used by [`diff_comm_hints`].
pub(super) trait CommHintMapOps {
    fn insert(&self, hint: &[u8]) -> io::Result<()>;
    fn remove(&self, hint: &[u8]) -> io::Result<()>;
}

/// Diffs `shadow` against `hints` through `ops`: removes every shadow hint
/// absent from `hints`, then adds every hint absent from `shadow` --
/// mirroring the eBPF `tracked`-map diff pattern, since a
/// `BPF_MAP_TYPE_HASH` has no atomic bulk-replace either.
///
/// `shadow` is updated immediately after each individual operation
/// succeeds, so a partial failure -- this returns the first error and stops
/// -- leaves `shadow` equal to exactly the hints `ops` actually holds at
/// that point, never missing an applied add or keeping a removed one. A
/// later retry resumes the diff from that consistent point instead of
/// leaking an entry that was applied but forgotten (which would otherwise
/// waste map capacity and could eventually fail a full replace with
/// E2BIG).
pub(super) fn diff_comm_hints(
    shadow: &mut BTreeSet<Vec<u8>>,
    hints: &CommHints,
    ops: &impl CommHintMapOps,
) -> io::Result<()> {
    let target: BTreeSet<Vec<u8>> = hints.iter().map(<[u8]>::to_vec).collect();

    let removed: Vec<Vec<u8>> = shadow.difference(&target).cloned().collect();
    for hint in removed {
        ops.remove(&hint)?;
        shadow.remove(&hint);
    }

    let added: Vec<Vec<u8>> = target.difference(shadow).cloned().collect();
    for hint in added {
        ops.insert(&hint)?;
        shadow.insert(hint);
    }

    Ok(())
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
#[path = "events_tests.rs"]
mod tests;
