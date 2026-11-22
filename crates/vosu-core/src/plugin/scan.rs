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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
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
        self.lock_entries().insert(id, ScanEntry { handle, gate });
        *count += 1;
        Ok(ScanSlot {
            outstanding: Arc::clone(self),
            id,
        })
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
    /// `read_plan::OutstandingPlans::cancel_session`'s A1 rule).
    pub fn cancel_session(&self, handle: u64) {
        let matches: Vec<(u64, Option<Gate>)> = {
            let entries = self.lock_entries();
            entries
                .iter()
                .filter(|(_, entry)| entry.handle == handle)
                .map(|(id, entry)| (*id, entry.gate.clone()))
                .collect()
        };
        for (id, gate) in matches {
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
            self.free(id);
        }
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

/// Refused when [`ScanWorkerPool`]'s bounded queue is already full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueFullError;

/// A small, bounded pool of worker threads scans run on, so a slow or
/// whole-address-space scan never blocks a plugin instance's own dispatch
/// thread (`docs/v2/plugin-abi.md` §4: real work triggered by a
/// notification-shaped opcode "must be handed off to another thread").
/// Scans are not on the measured hot path (§6.4: "a plugin that scans
/// every tick instead of caching the result … is a plugin bug"), so a
/// small fixed worker count bounds total scanning concurrency rather than
/// maximizing it; its queue is bounded by `queue_capacity`, matching
/// `dispatch::WorkerPool`'s own "no unbounded channels" rule.
pub struct ScanWorkerPool {
    sender: Option<SyncSender<Job>>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl ScanWorkerPool {
    #[must_use]
    pub fn new(worker_count: usize, queue_capacity: usize) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Job>(queue_capacity);
        let receiver = Arc::new(Mutex::new(receiver));
        let workers = (0..worker_count.max(1))
            .map(|_| {
                let receiver = Arc::clone(&receiver);
                thread::spawn(move || {
                    loop {
                        let job = {
                            let receiver = receiver.lock().unwrap_or_else(PoisonError::into_inner);
                            receiver.recv()
                        };
                        match job {
                            Ok(job) => job(),
                            Err(_disconnected) => break,
                        }
                    }
                })
            })
            .collect();
        Self {
            sender: Some(sender),
            workers,
        }
    }

    /// Queues `job` without blocking the caller. `Err` means the bounded
    /// queue is already full: the caller should refuse the submission
    /// (`VOSU_ERR_INVALID_ARGUMENT`) rather than block a dispatch thread
    /// waiting for room.
    pub fn try_spawn(&self, job: impl FnOnce() + Send + 'static) -> Result<(), QueueFullError> {
        let sender = self
            .sender
            .as_ref()
            .expect("sender is only taken by this pool's own Drop");
        match sender.try_send(Box::new(job)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(QueueFullError),
            Err(TrySendError::Disconnected(_)) => {
                unreachable!("this pool outlives every worker it spawned")
            }
        }
    }
}

impl Drop for ScanWorkerPool {
    fn drop(&mut self) {
        // Drop the sender first so every worker's blocking `recv` returns
        // `Err` and its loop exits, then join them instead of detaching.
        self.sender.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// Every way [`submit`] can refuse a scan before it ever reaches a worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanSubmitError {
    CapExceeded,
    QueueFull,
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

pub fn submit(
    outstanding: &Arc<OutstandingScans>,
    pool: &ScanWorkerPool,
    request: ScanRequest,
    memory: Arc<dyn ProcessMemory>,
    modules: Arc<dyn ModuleRegions + Send + Sync>,
    handle: u64,
    deliver: impl FnOnce(ScanResult) + Send + 'static,
) -> Result<(), ScanSubmitError> {
    Pattern::parse(&request.pattern, request.non_zero_mask)
        .map_err(|_| ScanSubmitError::InvalidPattern)?;

    let gate: Gate = Arc::new(Mutex::new(Some(Box::new(deliver) as DeliverFn)));
    let slot = outstanding
        .begin_internal(handle, Some(Arc::clone(&gate)))
        .map_err(|_| ScanSubmitError::CapExceeded)?;
    // Test-only seam (F6): lets a test run `cancel_session` in exactly
    // the window a real race would need — after the gate above is
    // registered, before `try_spawn` below is attempted — making that
    // race deterministic instead of timing-dependent.
    #[cfg(test)]
    run_race_hook_for_tests();
    let spawn_gate = Arc::clone(&gate);
    let spawn_result = pool.try_spawn(move || {
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
    match spawn_result {
        Ok(()) => Ok(()),
        Err(QueueFullError) => {
            // F6: `try_spawn` failing does not prove this scan delivered
            // nothing — a concurrent `cancel_session` may have already
            // taken this gate and delivered the session-detached form in
            // the window between `begin_internal` registering it, above,
            // and this call failing. Take the gate ourselves: empty
            // means that race happened, so this submission must not
            // *also* report a refusal (the plugin already has its
            // result); still holding the original `deliver` means
            // nothing was delivered, so drop it (this scan is refused,
            // and must never run) and report `QueueFull` as before.
            let already_delivered = gate
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .is_none();
            if already_delivered {
                Ok(())
            } else {
                Err(ScanSubmitError::QueueFull)
            }
        }
    }
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
