//! Signature scanning and module enumeration (`docs/v2/plugin-abi.md`
//! §6.4): executes one decoded `VOSU_OP_SCAN_REQUEST` over
//! `vosu_sys::process::scanner`'s primitive, the 64-outstanding-scan cap,
//! the error form A1 added for a scan that cannot complete, and a bounded
//! worker pool so a scan — unbounded cost in the worst case, the whole
//! process address space — never runs on a dispatch thread (§4: "real
//! work triggered by [a notification opcode] must be handed off").
//!
//! Standalone, like [`super::read_plan`]: wiring this into
//! `PluginHostHandler::handle` (resolving `ScanRequest.session` to a live
//! `ProcessMemory`, delivering `VOSU_OP_SCAN_RESULT` back through the
//! dispatch queue once the worker finishes) is later wiring, not this
//! phase's scope. [`OutstandingScans::cancel_session`] *is* in scope: it
//! is the primitive that wiring calls when a session detaches mid-scan.
//!
//! **Module enumeration.** [`WindowsModuleTable`] and [`LinuxModuleTable`]
//! both implement [`ModuleRegions`] and [`super::read_plan::ModuleResolver`]
//! from one platform-native enumeration
//! (`vosu_sys::process::enumerate_modules`, backed by the loader table on
//! Windows and `/proc/{pid}/maps` on Linux). Either table is a point-in-time
//! snapshot: a caller must build a fresh one for each read plan or scan
//! that resolves a module root/scope ("resolved by the host at read time",
//! `docs/v2/plugin-abi.md` §6.3) rather than caching one across requests.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;

use vosu_plugin_api::{ErrorInfo, ScanRequest, ScanResult, vosu_status};
use vosu_sys::process::scanner::{Pattern, PatternParseError};
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessMemory, ReadRequest};
#[cfg(any(windows, target_os = "linux"))]
use vosu_sys::process::{ModuleEntry, enumerate_modules};

/// Per-instance bound on `VOSU_OP_SCAN_REQUEST`s outstanding at once
/// (submitted, awaiting their `VOSU_OP_SCAN_RESULT`; §5, §6.4): the same
/// `VOSU_ERR_INVALID_ARGUMENT` code §6.3 reuses for an outstanding-read-
/// plan-cap refusal.
pub const MAX_OUTSTANDING_SCANS: usize = 64;

/// Resolves a `VOSU_OP_SCAN_REQUEST`'s `module` name to the address range
/// scanning is scoped to (§6.4: "`module` scopes the scan to one module's
/// readable regions"). `None` when the named module is not currently
/// loaded.
pub trait ModuleRegions {
    fn resolve(&self, module: &str) -> Option<ModuleRegion>;
}

/// One loaded module's address range, as `ModuleRegions::resolve` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleRegion {
    pub base: u64,
    pub size: u64,
}

/// Wraps `memory` so its `readable_regions` are clipped to `region`,
/// scoping every scanner primitive call to exactly one module's window
/// without changing `vosu_sys::process::scanner`'s own API.
struct ModuleScopedMemory<'a> {
    inner: &'a dyn ProcessMemory,
    region: ModuleRegion,
}

impl ProcessMemory for ModuleScopedMemory<'_> {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.inner.read_exact(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.inner.read_many(reads)
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        let start = usize::try_from(self.region.base).unwrap_or(usize::MAX);
        let end = usize::try_from(self.region.base.saturating_add(self.region.size))
            .unwrap_or(usize::MAX);
        Ok(self
            .inner
            .readable_regions()?
            .into_iter()
            .filter_map(|region| {
                let region_end = region.address.checked_add(region.size)?;
                let clipped_start = region.address.max(start);
                let clipped_end = region_end.min(end);
                (clipped_start < clipped_end).then(|| MemoryRegion {
                    address: clipped_start,
                    size: clipped_end - clipped_start,
                })
            })
            .collect())
    }

    fn read_scanner_region_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.inner.read_scanner_region_exact(address, dst)
    }

    fn process_id(&self) -> Option<u32> {
        self.inner.process_id()
    }

    fn is_alive(&self) -> bool {
        self.inner.is_alive()
    }
}

fn error_result(code: &str, message: impl Into<String>) -> ScanResult {
    ScanResult {
        addresses: Vec::new(),
        error: Some(ErrorInfo {
            code: code.to_owned(),
            message: message.into(),
        }),
    }
}

fn memory_error_code(error: &MemoryError) -> &'static str {
    match error {
        MemoryError::Backend { .. } => "backend",
        MemoryError::PermissionDenied { .. } => "permission_denied",
        MemoryError::Read { .. } => "read_failed",
        MemoryError::AddressOverflow { .. } => "address_overflow",
        MemoryError::Corrupt { .. } | MemoryError::InvalidUtf16 { .. } => "backend",
    }
}

/// Executes one `VOSU_OP_SCAN_REQUEST` against `memory`, scoping it to
/// `request.module`'s region when given. Never rejects the call itself:
/// every outcome — a malformed `pattern`, an unresolved `module`, a dead
/// target, or an in-bounds scan that simply finds nothing — is a
/// [`ScanResult`], carrying the error form when it cannot complete (§6.4).
/// The 64-outstanding cap and running this off a dispatch thread are the
/// caller's concern ([`OutstandingScans`], [`ScanWorkerPool`]).
pub fn execute(
    request: &ScanRequest,
    memory: &dyn ProcessMemory,
    modules: &dyn ModuleRegions,
) -> ScanResult {
    if !memory.is_alive() {
        return error_result("target_gone", "the target session's process has exited");
    }

    let pattern = match Pattern::parse(&request.pattern, request.non_zero_mask) {
        Ok(pattern) => pattern,
        Err(PatternParseError::InvalidToken { index, token }) => {
            return error_result(
                "invalid_pattern",
                format!("invalid signature token {token:?} at index {index}"),
            );
        }
    };

    let scoped;
    let target: &dyn ProcessMemory = match &request.module {
        Some(module) => match modules.resolve(module) {
            Some(region) => {
                scoped = ModuleScopedMemory {
                    inner: memory,
                    region,
                };
                &scoped
            }
            None => {
                return error_result(
                    "module_not_found",
                    format!("module {module:?} is not loaded in the target process"),
                );
            }
        },
        None => memory,
    };

    let outcome = if request.all_matches {
        target.scan_all(&pattern)
    } else {
        target
            .scan_first(&pattern)
            .map(|found| found.into_iter().collect())
    };

    // A scan is unbounded cost in the worst case; re-check liveness rather
    // than trusting the check at the top of this function, so a session
    // that detached partway through is reported as the error form (§6.4)
    // instead of whatever partial/empty result the scanner's own "skip
    // unreadable regions" behavior produced.
    if !memory.is_alive() {
        return error_result(
            "target_gone",
            "the target session's process exited during the scan",
        );
    }

    match outcome {
        Ok(addresses) => ScanResult {
            addresses: addresses
                .into_iter()
                .map(|address| address as u64)
                .collect(),
            error: None,
        },
        Err(error) => error_result(memory_error_code(&error), error.to_string()),
    }
}

/// Every way admitting a new outstanding scan can be refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutstandingScansError {
    /// This instance already has [`MAX_OUTSTANDING_SCANS`] scans awaiting
    /// their result.
    CapExceeded,
}

impl From<OutstandingScansError> for vosu_status {
    fn from(_: OutstandingScansError) -> Self {
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    }
}

/// Called exactly once per scan: either the worker's own finished
/// [`ScanResult`], or (if [`OutstandingScans::cancel_session`] wins the
/// race) the session-detached error form.
type DeliverFn = Box<dyn FnOnce(ScanResult) + Send>;
/// Shared between the worker and [`OutstandingScans::cancel_session`]:
/// whichever side `take()`s it first is the one that actually calls it,
/// guaranteeing exactly-one delivery no matter which finishes first.
type Gate = Arc<Mutex<Option<DeliverFn>>>;

struct ScanEntry {
    handle: u64,
    gate: Option<Gate>,
    /// F1: this scan's still-queued [`ScanWorkerPool`] job id, set once
    /// [`submit`] has actually queued it — `None` once a worker has
    /// already dequeued the job (or it never queued at all, e.g. a
    /// malformed request answered inline before reaching the pool).
    /// [`OutstandingScans::cancel_session`] hands this back so the
    /// caller can remove the job from its key's own sub-queue
    /// immediately, instead of leaving a cancelled job to sit there
    /// until a worker happens to reach it (F1).
    queue_ticket: Option<u64>,
}

/// Tracks one plugin instance's outstanding `VOSU_OP_SCAN_REQUEST`s
/// against [`MAX_OUTSTANDING_SCANS`] (§5, §6.4), and which session handle
/// each belongs to, so a session detach can free every scan it still owes
/// a result to.
#[derive(Debug, Default)]
pub struct OutstandingScans {
    count: Mutex<usize>,
    entries: Mutex<std::collections::HashMap<u64, ScanEntry>>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for ScanEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScanEntry")
            .field("handle", &self.handle)
            .field("gate", &self.gate.is_some())
            .finish()
    }
}

impl OutstandingScans {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock_count(&self) -> std::sync::MutexGuard<'_, usize> {
        self.count.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_entries(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, ScanEntry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Frees `id`'s slot if it is still held: idempotent, so both
    /// [`ScanSlot::drop`] and [`Self::cancel_session`] racing to free the
    /// same scan never double-decrement the count (§6.4: "a scan's slot
    /// is freed by its `SCAN_RESULT` ... or, if its session detaches
    /// first, by core sending that error form itself").
    fn free(&self, id: u64) {
        let removed = self.lock_entries().remove(&id).is_some();
        if removed {
            let mut count = self.lock_count();
            *count = count.saturating_sub(1);
        }
    }

    fn begin_internal(
        self: &Arc<Self>,
        handle: u64,
        gate: Option<Gate>,
    ) -> Result<ScanSlot, OutstandingScansError> {
        let mut count = self.lock_count();
        if *count >= MAX_OUTSTANDING_SCANS {
            return Err(OutstandingScansError::CapExceeded);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.lock_entries().insert(
            id,
            ScanEntry {
                handle,
                gate,
                queue_ticket: None,
            },
        );
        *count += 1;
        Ok(ScanSlot {
            outstanding: Arc::clone(self),
            id,
        })
    }

    /// F1: records `id`'s pool job ticket once [`submit`] has actually
    /// queued it. N3: a session detach (`cancel_session`, the only way a
    /// scan is ever cancelled) can still land between
    /// [`Self::begin_internal`] and this call, in which case `id`'s slot
    /// is already gone (freed and, through its own gate, already
    /// delivered the session-detached result) — reported `false` so
    /// [`submit`] can remove the now-pointless job from the pool itself
    /// with the `ticket` it already has, rather than leaving it to sit
    /// in the pool's own per-key sub-queue until a worker reaches it and
    /// finds nothing left to do (mirroring `read_plan::OutstandingPlans`'
    /// own `set_queue_ticket`, which has the same race for the same
    /// reason).
    fn set_queue_ticket(&self, id: u64, ticket: u64) -> bool {
        if let Some(entry) = self.lock_entries().get_mut(&id) {
            entry.queue_ticket = Some(ticket);
            true
        } else {
            false
        }
    }

    /// Admits one more scan for `handle`'s session, refusing beyond
    /// [`MAX_OUTSTANDING_SCANS`]. The returned [`ScanSlot`] frees it when
    /// dropped.
    pub fn begin(self: &Arc<Self>, handle: u64) -> Result<ScanSlot, OutstandingScansError> {
        self.begin_internal(handle, None)
    }

    /// `SESSION_DETACHED` for `handle`: frees every scan still outstanding
    /// for that session immediately, and — racing against each one's own
    /// worker — delivers the session-detached error form for it exactly
    /// once, whichever side gets there first (§6.4, mirroring
    /// `read_plan::OutstandingPlans::cancel_session`'s A1 rule). Returns
    /// every freed scan's own pool job ticket, if it still had one (F1):
    /// the caller removes each from the shared pool's own per-key
    /// sub-queue immediately, rather than leaving a cancelled job to pile
    /// up there until a worker happens to reach it.
    #[must_use]
    pub fn cancel_session(&self, handle: u64) -> Vec<u64> {
        let matches: Vec<(u64, Option<Gate>, Option<u64>)> = {
            let entries = self.lock_entries();
            entries
                .iter()
                .filter(|(_, entry)| entry.handle == handle)
                .map(|(id, entry)| (*id, entry.gate.clone(), entry.queue_ticket))
                .collect()
        };
        let mut tickets = Vec::new();
        for (id, gate, queue_ticket) in matches {
            if let Some(gate) = gate {
                // Released before `deliver` runs (F6): holding the
                // gate's own mutex across that call would let a slow
                // `deliver` (plugin-wiring code, not under this module's
                // control) block `Self::free` below, or a concurrent
                // `cancel_session`/worker-side take, for as long as it
                // takes.
                let deliver = gate.lock().unwrap_or_else(PoisonError::into_inner).take();
                if let Some(deliver) = deliver {
                    deliver(error_result(
                        "session_detached",
                        "the target session detached while this scan was in flight",
                    ));
                }
            }
            if let Some(ticket) = queue_ticket {
                tickets.push(ticket);
            }
            self.free(id);
        }
        tickets
    }

    /// Current outstanding count, for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        *self.lock_count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Held for as long as one scan is outstanding; dropping it (on any path:
/// normal completion, a malformed request answered inline, or a session
/// detaching mid-scan) frees its slot.
#[derive(Debug)]
#[must_use]
pub struct ScanSlot {
    outstanding: Arc<OutstandingScans>,
    id: u64,
}

impl Drop for ScanSlot {
    fn drop(&mut self) {
        self.outstanding.free(self.id);
    }
}

type Job = Box<dyn FnOnce() + Send + 'static>;

/// A small, bounded pool of worker threads read plans and scans both run
/// on (`host/read_plans.rs`'s own `background_pool`), so neither ever
/// blocks a plugin instance's own dispatch thread (`docs/v2/plugin-abi.md`
/// §4: real work triggered by a notification-shaped opcode "must be
/// handed off to another thread").
///
/// P2-7 (`docs/v2/plugin-abi.md` §5): jobs queue per *key* — in practice a
/// `PluginInstanceId`'s own `u64`, one sub-queue per live instance —
/// rather than into one queue shared by every instance. A caller must
/// already have admitted `job` against its own bounded cap
/// (`OutstandingPlans`/`OutstandingScans`, 64 read plans + 64 scans) before
/// calling [`Self::submit`], so one key's own sub-queue never holds more
/// than 128 jobs *genuinely outstanding* at a time — [`Self::submit`]
/// therefore never refuses, and one plugin flooding its own cap can never
/// delay another plugin's already-admitted work the way a single shared,
/// fixed-capacity queue could. [`Self::submit`] returns a ticket
/// identifying the queued job; [`Self::cancel`] removes it again if a
/// caller's own cancellation (`VOSU_OP_READ_PLAN_CANCEL`, a session
/// detach) reaches it before a worker does (F1) — without this, a plugin
/// that loops submit-then-cancel against its own already-admitted cap
/// could grow its own sub-queue without bound while this instance's own
/// workers stay busy on its other, still-running jobs, since admission
/// alone only bounds how many jobs are genuinely *outstanding*, not how
/// many cancelled-but-still-queued entries accumulate behind them. Each
/// sub-queue is a plain unbounded-looking `VecDeque`, but its true bound
/// is that external per-instance admission *combined with* prompt
/// cancellation — exactly the same way `dispatch::WorkerPool`'s own
/// per-instance queue is bounded by a fixed capacity rather than a wholly
/// separate admission check — here the admission check already lives in
/// `OutstandingPlans`/`OutstandingScans`, so duplicating it as a second,
/// pool-level capacity would only let the two bounds disagree.
///
/// A small fixed worker count (`host/read_plans.rs`'s own
/// `BACKGROUND_WORKER_COUNT`) bounds total concurrency; workers drain
/// every live key's sub-queue in round-robin order, so no one key's
/// backlog starves another's.
pub struct ScanWorkerPool {
    inner: Arc<PoolInner>,
    workers: Vec<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct PoolState {
    queues: HashMap<u64, VecDeque<(u64, Job)>>,
    /// Keys with at least one queued job, in round-robin service order.
    /// A key appears at most once: pushed back onto the end when its
    /// sub-queue is still non-empty after a worker takes one job from it.
    ready: VecDeque<u64>,
    closed: bool,
}

struct PoolInner {
    state: Mutex<PoolState>,
    not_empty: Condvar,
    /// F1: assigns each queued job its own [`Self::submit`]-returned
    /// ticket, unique for the pool's whole lifetime (never reused, so a
    /// stale ticket from an already-run or already-cancelled job can
    /// never collide with, and wrongly cancel, a later one).
    next_job_id: AtomicU64,
}

fn worker_loop(inner: Arc<PoolInner>) {
    loop {
        let job = {
            let mut state = inner.state.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if let Some(key) = state.ready.pop_front() {
                    let queue = state
                        .queues
                        .get_mut(&key)
                        .expect("a ready key always has a non-empty queue");
                    let (_id, job) = queue.pop_front().expect("ready implies a queued job");
                    if queue.is_empty() {
                        state.queues.remove(&key);
                    } else {
                        state.ready.push_back(key);
                    }
                    break Some(job);
                }
                if state.closed {
                    break None;
                }
                state = inner
                    .not_empty
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        match job {
            Some(job) => job(),
            None => break,
        }
    }
}

impl ScanWorkerPool {
    #[must_use]
    pub fn new(worker_count: usize) -> Self {
        let inner = Arc::new(PoolInner {
            state: Mutex::new(PoolState::default()),
            not_empty: Condvar::new(),
            next_job_id: AtomicU64::new(0),
        });
        let workers = (0..worker_count.max(1))
            .map(|_| {
                let inner = Arc::clone(&inner);
                thread::spawn(move || worker_loop(inner))
            })
            .collect();
        Self { inner, workers }
    }

    /// Queues `job` on `key`'s own sub-queue (this struct's own doc
    /// comment): the caller must already have admitted it against its own
    /// per-instance cap, so this never refuses. Returns a ticket
    /// [`Self::cancel`] can later use to remove `job` again, if a worker
    /// has not already dequeued it (F1).
    pub fn submit(&self, key: u64, job: impl FnOnce() + Send + 'static) -> u64 {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let id = self.inner.next_job_id.fetch_add(1, Ordering::Relaxed);
        let queue = state.queues.entry(key).or_default();
        queue.push_back((id, Box::new(job) as Job));
        if queue.len() == 1 {
            state.ready.push_back(key);
        }
        drop(state);
        self.inner.not_empty.notify_one();
        id
    }

    /// F1: removes `ticket`'s job from `key`'s own sub-queue if a worker
    /// has not already dequeued it, returning whether it was still there
    /// to remove. A caller whose own cancellation races a worker that
    /// already started the job gets `false` back — nothing left to
    /// remove, and the job's own `is_cancelled`/gate-take check (the
    /// caller's own admission bookkeeping) still skips its real work.
    pub fn cancel(&self, key: u64, ticket: u64) -> bool {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(queue) = state.queues.get_mut(&key) else {
            return false;
        };
        let before = queue.len();
        queue.retain(|(id, _)| *id != ticket);
        let removed = queue.len() != before;
        if removed && queue.is_empty() {
            // Mirrors `worker_loop`'s own bookkeeping: an empty sub-queue
            // is never left in `queues`, and its key is never left in
            // `ready` either, or a later worker's `expect` above would
            // panic finding a `ready` key with no queue behind it.
            state.queues.remove(&key);
            state.ready.retain(|&ready_key| ready_key != key);
        }
        removed
    }
}

#[cfg(test)]
impl ScanWorkerPool {
    /// Test-only: `key`'s own sub-queue length, for F1's own regression
    /// test — proving a loop of submit-then-cancel never grows it.
    pub(crate) fn queue_len_for_tests(&self, key: u64) -> usize {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queues
            .get(&key)
            .map_or(0, VecDeque::len)
    }
}

impl Drop for ScanWorkerPool {
    fn drop(&mut self) {
        // Close first so every worker's `wait` wakes, observes `closed`,
        // and exits its loop, then join them instead of detaching.
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.inner.not_empty.notify_all();
        // F4: a narrower self-join window than P1-2's own closed — a job
        // holds only a `Weak<PluginHost>` (P1-2), but the runtime can
        // still drop the last strong `Arc` from inside that job's own
        // `host.upgrade()` window, making *this* `drop` run on a worker
        // thread that is this pool's own. Joining that thread's handle
        // from inside itself would deadlock (a plain `thread::Builder`
        // join, not even `JoinHandle::join`'s own panic-propagation,
        // ever supports that); skip exactly that one handle; every other
        // worker is still joined normally, and the running job (this
        // call itself) returns, letting that one thread exit on its own.
        let current = thread::current().id();
        for worker in self.workers.drain(..) {
            if worker.thread().id() == current {
                continue;
            }
            let _ = worker.join();
        }
    }
}

/// Every way [`submit`] can refuse a scan before it ever reaches a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanSubmitError {
    CapExceeded,
    /// `request.pattern` does not parse (§5: "Malformed payload →
    /// `INVALID_ARGUMENT`"): refused synchronously here, before taking an
    /// [`OutstandingScans`] slot or a worker-pool queue entry, rather than
    /// answering the error form from a worker the way [`execute`] does
    /// for a direct (non-pool) caller.
    InvalidPattern,
}

impl From<ScanSubmitError> for vosu_status {
    fn from(_: ScanSubmitError) -> Self {
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    }
}

/// Where [`submit`] routes one scan: `session` is the handle
/// [`OutstandingScans::cancel_session`] keys it by; `queue_key` is the
/// owning instance's own `u64` the shared [`ScanWorkerPool`] sub-queues it
/// by (P2-7). Grouped into one struct so `submit` stays under clippy's
/// argument-count limit rather than taking both as bare `u64`s.
pub struct ScanRouting {
    pub session: u64,
    pub queue_key: u64,
}

pub fn submit(
    outstanding: &Arc<OutstandingScans>,
    pool: &ScanWorkerPool,
    request: ScanRequest,
    memory: Arc<dyn ProcessMemory>,
    modules: Arc<dyn ModuleRegions + Send + Sync>,
    routing: ScanRouting,
    deliver: impl FnOnce(ScanResult) + Send + 'static,
) -> Result<(), ScanSubmitError> {
    Pattern::parse(&request.pattern, request.non_zero_mask)
        .map_err(|_| ScanSubmitError::InvalidPattern)?;

    let gate: Gate = Arc::new(Mutex::new(Some(Box::new(deliver) as DeliverFn)));
    let slot = outstanding
        .begin_internal(routing.session, Some(Arc::clone(&gate)))
        .map_err(|_| ScanSubmitError::CapExceeded)?;
    // Test-only seam (F6): lets a test run `cancel_session` in exactly
    // the window a real race would need — after the gate above is
    // registered, before `pool.submit` below runs it — making that race
    // deterministic instead of timing-dependent.
    #[cfg(test)]
    run_race_hook_for_tests();
    let slot_id = slot.id;
    let spawn_gate = Arc::clone(&gate);
    let ticket = pool.submit(routing.queue_key, move || {
        // P2-7: skip the real (possibly whole-address-space) scan
        // entirely if `cancel_session` already won the race and
        // delivered the session-detached form — this job has nothing
        // left to do. `queue_key`'s own sub-queue (`ScanWorkerPool`'s
        // own doc comment) also means this job always runs, since
        // `queue_key` already has room for it under its own admitted cap.
        let already_cancelled = spawn_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none();
        if already_cancelled {
            drop(slot);
            return;
        }
        let result = execute(&request, memory.as_ref(), modules.as_ref());
        // §6.4/P3: deliver before freeing the slot, so the slot stays
        // held for the whole time its `SCAN_RESULT` could still be in
        // flight toward the plugin rather than being reusable the moment
        // the scan itself finishes. Released before calling `deliver`
        // (F6), for the same reason as `cancel_session`'s own take.
        let deliver = spawn_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(deliver) = deliver {
            deliver(result);
        }
        drop(slot);
    });
    // F1: lets a later `cancel_session` remove this job from the pool's
    // own sub-queue immediately if it still can — a no-op if the job
    // already ran (or was already cancelled) by the time this runs. N3:
    // if `cancel_session` already won the race against this very call
    // (`set_queue_ticket` found `slot_id`'s own slot already gone), the
    // ticket was never recorded there for a later `cancel_session` to
    // find — remove the now-pointless job from the pool directly with
    // the `ticket` already in hand instead, rather than leaving it to
    // sit in the pool's own per-key sub-queue until a worker reaches it
    // and finds nothing left to do.
    if !outstanding.set_queue_ticket(slot_id, ticket) {
        pool.cancel(routing.queue_key, ticket);
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static RACE_HOOK_FOR_TESTS: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only hook for F6's race: installs a one-shot closure that
/// [`submit`] runs at the exact point its own window for the race opens.
#[cfg(test)]
fn set_race_hook_for_tests(hook: impl FnOnce() + 'static) {
    RACE_HOOK_FOR_TESTS.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
fn run_race_hook_for_tests() {
    let hook = RACE_HOOK_FOR_TESTS.with(|cell| cell.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

/// A snapshot of one process's loaded modules, built once from
/// `vosu_sys::process::enumerate_modules`. Implements both
/// [`ModuleRegions`] (scan scoping) and
/// [`super::read_plan::ModuleResolver`] (read-plan `VOSU_BASE_MODULE`
/// roots) from the same data, since both only need a named module's base
/// address — `ModuleRegions` additionally needs its size to scope a scan.
#[cfg(windows)]
pub struct WindowsModuleTable {
    modules: Vec<ModuleEntry>,
}

#[cfg(windows)]
impl WindowsModuleTable {
    pub fn snapshot(pid: u32) -> Result<Self, MemoryError> {
        Ok(Self {
            modules: enumerate_modules(pid)?,
        })
    }

    /// `docs/v2/plugin-abi.md` §6.1: Windows module-name comparison is
    /// case-insensitive, matching that platform's own filesystem semantics.
    fn find(&self, name: &str) -> Option<&ModuleEntry> {
        self.modules
            .iter()
            .find(|module| module.name.eq_ignore_ascii_case(name))
    }
}

#[cfg(windows)]
impl super::read_plan::ModuleResolver for WindowsModuleTable {
    fn resolve(&self, module: &str) -> Option<u64> {
        self.find(module).map(|entry| entry.base_address as u64)
    }
}

#[cfg(windows)]
impl ModuleRegions for WindowsModuleTable {
    fn resolve(&self, module: &str) -> Option<ModuleRegion> {
        self.find(module).map(|entry| ModuleRegion {
            base: entry.base_address as u64,
            size: u64::from(entry.size),
        })
    }
}

/// A snapshot of one Linux process's loaded modules, built once from
/// `vosu_sys::process::enumerate_modules`'s `/proc/{pid}/maps`-derived
/// table. Implements both [`ModuleRegions`] and
/// [`super::read_plan::ModuleResolver`], mirroring [`WindowsModuleTable`].
#[cfg(target_os = "linux")]
pub struct LinuxModuleTable {
    modules: Vec<ModuleEntry>,
}

#[cfg(target_os = "linux")]
impl LinuxModuleTable {
    pub fn snapshot(pid: u32) -> Result<Self, MemoryError> {
        Ok(Self {
            modules: enumerate_modules(pid)?,
        })
    }

    /// Linux module-name comparison is exact: the filesystem (and so a
    /// mapped file's own name) is case-sensitive there, unlike Windows.
    fn find(&self, name: &str) -> Option<&ModuleEntry> {
        self.modules.iter().find(|module| module.name == name)
    }
}

#[cfg(target_os = "linux")]
impl super::read_plan::ModuleResolver for LinuxModuleTable {
    fn resolve(&self, module: &str) -> Option<u64> {
        self.find(module).map(|entry| entry.base_address as u64)
    }
}

#[cfg(target_os = "linux")]
impl ModuleRegions for LinuxModuleTable {
    fn resolve(&self, module: &str) -> Option<ModuleRegion> {
        self.find(module).map(|entry| ModuleRegion {
            base: entry.base_address as u64,
            size: u64::from(entry.size),
        })
    }
}

#[cfg(test)]
mod tests;
