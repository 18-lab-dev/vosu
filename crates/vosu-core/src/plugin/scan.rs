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
//! dispatch queue once the worker finishes, and answering a mid-scan
//! session detach with the error form below) is later wiring, not this
//! phase's scope.
//!
//! **Module enumeration gap.** `vosu_sys::process` only exposes module
//! enumeration on Windows (`enumerate_modules`/`ModuleEntry`); Linux has
//! no equivalent today (`crates/vosu-sys/src/process/linux.rs` has no
//! `enumerate_modules`, only the unnamed `readable_regions` a scan already
//! uses). [`WindowsModuleTable`] implements both [`ModuleRegions`] and
//! [`super::read_plan::ModuleResolver`] from that platform's own
//! enumeration; a Linux module list (presumably built from
//! `/proc/{pid}/maps`'s path column) is not part of this phase and is
//! flagged here rather than invented without a spec to match against.

use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use vosu_plugin_api::{ErrorInfo, ScanRequest, ScanResult, vosu_status};
use vosu_sys::process::scanner::{Pattern, PatternParseError};
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessMemory, ReadRequest};
#[cfg(windows)]
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

/// Tracks one plugin instance's outstanding `VOSU_OP_SCAN_REQUEST`s
/// against [`MAX_OUTSTANDING_SCANS`] (§5, §6.4).
#[derive(Debug, Default)]
pub struct OutstandingScans {
    count: Mutex<usize>,
}

impl OutstandingScans {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, usize> {
        self.count.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Admits one more scan, refusing beyond [`MAX_OUTSTANDING_SCANS`].
    /// The returned [`ScanSlot`] frees it when dropped — "a scan's slot is
    /// freed by its `SCAN_RESULT` (success or the error form above) or, if
    /// its session detaches first, by core sending that error form itself
    /// rather than leaving the slot held" (§6.4): both paths are simply
    /// "drop the slot once a `ScanResult` of either kind has been sent".
    pub fn begin(self: &Arc<Self>) -> Result<ScanSlot, OutstandingScansError> {
        let mut count = self.lock();
        if *count >= MAX_OUTSTANDING_SCANS {
            return Err(OutstandingScansError::CapExceeded);
        }
        *count += 1;
        Ok(ScanSlot(Arc::clone(self)))
    }

    /// Current outstanding count, for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        *self.lock()
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
pub struct ScanSlot(Arc<OutstandingScans>);

impl Drop for ScanSlot {
    fn drop(&mut self) {
        let mut count = self.0.lock();
        *count = count.saturating_sub(1);
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
}

impl From<ScanSubmitError> for vosu_status {
    fn from(_: ScanSubmitError) -> Self {
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    }
}

/// Submits one scan to run on `pool`, honoring [`OutstandingScans`]'s cap:
/// refuses with `VOSU_ERR_INVALID_ARGUMENT` if the cap is already reached
/// or the worker queue is already full; otherwise `deliver` is called with
/// the finished [`ScanResult`] on a worker thread, never on the caller's.
pub fn submit(
    outstanding: &Arc<OutstandingScans>,
    pool: &ScanWorkerPool,
    request: ScanRequest,
    memory: Arc<dyn ProcessMemory>,
    modules: Arc<dyn ModuleRegions + Send + Sync>,
    deliver: impl FnOnce(ScanResult) + Send + 'static,
) -> Result<(), ScanSubmitError> {
    let slot = outstanding
        .begin()
        .map_err(|_| ScanSubmitError::CapExceeded)?;
    pool.try_spawn(move || {
        let result = execute(&request, memory.as_ref(), modules.as_ref());
        drop(slot);
        deliver(result);
    })
    .map_err(|_| ScanSubmitError::QueueFull)
}

/// A snapshot of one process's loaded modules, built once from
/// `vosu_sys::process::enumerate_modules`. Implements both
/// [`ModuleRegions`] (scan scoping) and
/// [`super::read_plan::ModuleResolver`] (read-plan `VOSU_BASE_MODULE`
/// roots) from the same data, since both only need a named module's base
/// address — `ModuleRegions` additionally needs its size to scope a scan.
///
/// Windows-only: see this module's doc comment for the Linux gap.
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

#[cfg(test)]
mod tests;
