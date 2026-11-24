//! Routes `VOSU_OP_READ_PLAN_SUBMIT`/`VOSU_OP_READ_PLAN_CANCEL` through
//! [`PluginHost`] (`docs/v2/plugin-abi.md` §4's SYNC exception, §5, §6.3;
//! `docs/v2/implementation-plan.md` section 10, D5): resolves a session
//! handle to its live process, opening it once and caching the handle per
//! session ([`PluginHost::session_memory`]; dropped on detach,
//! [`PluginHost::forget_read_plan_session`]). A `PlanBase::Module` root's
//! module table is built lazily — only when a plan actually names one,
//! never for a `PlanBase::Absolute` plan (P1-1) — and cached alongside the
//! process handle, refreshed once on a resolve miss
//! ([`CachingModuleResolver`]): opening a process handle and parsing
//! `/proc/<pid>/maps` (or taking a Toolhelp module snapshot) are both too
//! expensive to repeat on every submission, which M1's own 1.x-style
//! per-tick reads make a hot path. A module snapshot that cannot be built
//! at all still resolves nothing, which `read_plan::resolve_root` already
//! reports as a per-read `VOSU_READ_NOT_READY` — never
//! `VOSU_ERR_TARGET_GONE`, and never for an `Absolute` plan, which never
//! touches the module table in the first place.
//!
//! An async (non-`VOSU_READ_PLAN_SYNC`) submission executes on
//! `PluginHost`'s own `background_pool` — one [`ScanWorkerPool`] shared
//! with `scans.rs` (that field's own doc comment: both opcodes' work is
//! off the hot path, so splitting worker threads between two pools buys
//! nothing but doubles the idle OS threads every `PluginHost` keeps
//! parked) — and delivers its result through the normal dispatch queue
//! ([`PluginHost::dispatch_to_instance`]); a `VOSU_READ_PLAN_SYNC`
//! submission instead delivers its result through a direct, non-queued
//! plugin call on the submitting thread itself, before this call (and so
//! `send`) returns — §4's one documented exception to per-instance
//! serialization. The job this spawns captures only a `Weak<PluginHost>`
//! (P1-2), upgraded once, only to deliver: a strong `Arc` here would let a
//! background-pool worker end up being the thread that drops the last
//! `PluginHost` reference, which self-joins (`ScanWorkerPool::drop` joins
//! every worker, including whichever one is running) and aborts or hangs
//! instead of shutting down. `submit_read_plan_async` also validates a
//! dependent stage's `ref_op` width synchronously, before admission (P1-3):
//! an async plan has no synchronous caller left to report
//! `VOSU_ERR_INVALID_ARGUMENT` to once a worker discovers this, so it must
//! never reach the worker in the first place. Each live instance submits
//! onto its own sub-queue in the shared pool (P2-7,
//! `super::super::scan::ScanWorkerPool`'s own doc comment), bounded only
//! by that instance's own admission caps, so a plan this instance was
//! already granted a slot for is never delayed by another instance's
//! backlog filling a queue the two would otherwise share; the job itself
//! also peeks [`super::super::read_plan::OutstandingPlans::is_cancelled`]
//! before doing any real work, so a plugin that cancels a plan it no
//! longer needs does not also cost the pool the time of running it.
//!
//! `VOSU_OP_READ_PLAN_CANCEL` and a session's detach both resolve to
//! [`super::super::read_plan::OutstandingPlans`]'s own `cancel`/
//! `cancel_session`; this module never re-derives that bookkeeping.
//!
//! [`PluginHost::session_memory`] and [`PluginHost::resolve_owned_session`]
//! are `pub(super)`, not private to this module: `host/scans.rs` (§6.4)
//! reuses both verbatim rather than opening a second process handle per
//! session or re-deriving the same ownership check — a scan against a
//! session this instance also has read plans outstanding against shares
//! the one cached handle. [`SessionMemoryError`] is likewise shared: both
//! modules map a failed open the same way (§5: only a dead process is
//! `VOSU_ERR_TARGET_GONE`; a live one this host simply failed to open is
//! `VOSU_ERR_PERMISSION_DENIED`/`VOSU_ERR_BACKEND`, P2-8).

use std::cell::RefCell;
#[cfg(any(test, feature = "test-harness"))]
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use vosu_plugin_api::{
    Message, ReadPlanCancel, ReadPlanSubmit, read_flat_header, validate_flags,
    validate_read_plan_submit, vosu_opcode, vosu_read_plan_submit_header, vosu_status,
};
use vosu_sys::process::{MemoryError, ProcessBitness, ProcessMemory};

use super::super::dispatch::dispatch_raw;
use super::super::host_services::PluginInstanceId;
use super::super::read_plan::{
    self, DispatchThreadScope, FinishOutcome, ModuleResolver, OutstandingPlans, SyncReadPlanError,
};
use super::super::scan::ScanWorkerPool;
use super::super::session::SessionHandle;
use super::{InstanceSlot, PluginHost};

/// Worker threads the shared background pool runs (production default):
/// comparatively cheap, deadline-bounded work (a read plan's handful of
/// syscalls, a scan's bounded memory walk), so a small fixed count
/// bounds total concurrency the same way `scan.rs`'s own
/// `ScanWorkerPool` doc comment reasons about scans alone —
/// `PluginHost::background_pool`'s own field doc comment covers why this
/// one pool now serves both opcodes.
const BACKGROUND_WORKER_COUNT: usize = 4;

/// [`ScanWorkerPool::new`]: each live instance gets its own sub-queue
/// (`ScanWorkerPool`'s own doc comment, P2-7), bounded only by the sum of
/// that instance's own [`super::super::read_plan::MAX_OUTSTANDING_ASYNC_PLANS`]
/// and [`super::super::scan::MAX_OUTSTANDING_SCANS`] (128) admission caps,
/// enforced by [`OutstandingPlans`]/[`super::super::scan::OutstandingScans`]
/// themselves, not by this pool — so one instance's admitted work can
/// never find its own sub-queue "full" or wait behind another instance's.
pub(super) fn new_background_pool() -> ScanWorkerPool {
    ScanWorkerPool::new(BACKGROUND_WORKER_COUNT)
}

/// Deadline given to one read plan's execution, sync or async (§6.3, §5's
/// `VOSU_ERR_TIMED_OUT`): the spec names no value, so this host picks one
/// generous enough that a legitimate dependent chain never trips it under
/// normal load, short enough that a stuck target does not hang a
/// `VOSU_READ_PLAN_SYNC` submitter — or, for an async plan, a worker
/// thread — indefinitely.
pub(super) const READ_PLAN_DEADLINE: Duration = Duration::from_secs(2);

thread_local! {
    /// Reused per-reader-thread scratch buffer for
    /// [`PluginHost::handle_read_plan_submit_raw`]'s own
    /// [`read_plan::execute_into`] call (D17): a `VOSU_READ_PLAN_SYNC`
    /// submission runs reentrant on the plugin's own submitting thread, so
    /// this buffer stays warm (steady-state capacity) for that one thread
    /// across repeated same-shaped plans, mirroring
    /// `vosu_plugin_api::sdk::plan_memory::SyncReadBuffers`'s own
    /// plugin-side buffer.
    static SYNC_RESULT_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// One session's cached, already-opened live-process handle plus its
/// bitness: opening a process handle is comparatively expensive, so it is
/// opened once per session and reused for every read plan the owning
/// plugin submits against it until the session detaches. The module table
/// backing a `VOSU_BASE_MODULE` root is cached here too, lazily
/// ([`CachingModuleResolver`]): built only once a plan actually names a
/// module, not unconditionally alongside the process handle itself.
pub(super) struct SessionMemory {
    /// `pub(super)`: `host::scans` reads this directly (module doc
    /// comment) rather than caching a second handle for the same session.
    pub(super) process: Arc<dyn ProcessMemory>,
    pub(super) bitness: ProcessBitness,
    pid: u32,
    /// Lazily built by [`CachingModuleResolver`]; `None` until the first
    /// `PlanBase::Module` root this session's plans ever resolve.
    modules: Mutex<Option<Arc<dyn ModuleResolver + Send + Sync>>>,
}

/// One session's own, lazily-opened slot (F5, N2): the global
/// `read_plan_memory` map holds one `Arc<SessionMemorySlot>` per handle,
/// never the open result directly, so [`PluginHost::session_memory`]
/// only ever needs that global lock long enough to get or insert the
/// slot itself — the actual `open_session_memory` call (a handful of
/// syscalls, `/proc/<pid>/maps` on Linux) runs against this slot's own
/// state, outside the global lock, so one session's slow first open
/// never blocks another plugin's already-cached (or independently
/// opening) session, including a `VOSU_READ_PLAN_SYNC` submission on a
/// completely different session.
///
/// N2: only a successful open is ever cached, in `result` — a failed
/// open (e.g. `ptrace_scope` denying it) is never stored, so the next
/// submission against this same handle retries the open instead of
/// replaying the same failure for the rest of the session's life (e.g.
/// the user lowers `ptrace_scope` at runtime; before this fix the next
/// plan would still fail until the game restarted). `open` single-
/// flights concurrent openers of this one slot onto one real open: a
/// plain `OnceLock<Result<..>>` could not do this without also caching
/// `Err`, so this slot takes its own `Mutex` around the open instead —
/// held only across `open_session_memory` itself, never across another
/// slot's, so two different sessions' opens still never serialize
/// against each other (only concurrent openers of the *same* session do,
/// same as before).
#[derive(Default)]
pub(super) struct SessionMemorySlot {
    result: std::sync::OnceLock<Arc<SessionMemory>>,
    open: Mutex<()>,
}

/// Every way [`open_session_memory`] can fail to open `pid`'s process
/// (P2-8, §5): only a `pid` that is actually dead maps to
/// `VOSU_ERR_TARGET_GONE`; a live process this host simply could not open
/// (denied access, or any other OS failure) must not look dead to the
/// plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionMemoryError {
    TargetGone,
    PermissionDenied,
    Backend,
}

impl From<SessionMemoryError> for vosu_status {
    fn from(error: SessionMemoryError) -> Self {
        match error {
            SessionMemoryError::TargetGone => vosu_status::VOSU_ERR_TARGET_GONE,
            SessionMemoryError::PermissionDenied => vosu_status::VOSU_ERR_PERMISSION_DENIED,
            SessionMemoryError::Backend => vosu_status::VOSU_ERR_BACKEND,
        }
    }
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    vosu_sys::process::is_process_alive(pid)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    vosu_sys::process::is_process_alive(pid)
}

/// Classifies an [`open_session_memory_real`] failure (P2-8): a
/// `MemoryError::PermissionDenied` is reported as such regardless of
/// liveness; otherwise `pid` is checked directly rather than trusting the
/// failed open's own error kind, since neither backend's "could not open"
/// error reliably distinguishes "already dead" from "denied" on its own
/// (`vosu-sys`'s own `open_process`/`WindowsProcessMemory::open` fold a
/// stale/wrong-owner pid into the same error shape as a live one this
/// host lacks rights to).
fn classify_open_error(pid: u32, error: &MemoryError) -> SessionMemoryError {
    if matches!(error, MemoryError::PermissionDenied { .. }) {
        return SessionMemoryError::PermissionDenied;
    }
    if !process_alive(pid) {
        return SessionMemoryError::TargetGone;
    }
    SessionMemoryError::Backend
}

#[cfg(target_os = "linux")]
fn open_session_memory_real(pid: u32) -> Result<SessionMemory, MemoryError> {
    let process = vosu_sys::process::open_process(pid)?;
    let bitness = vosu_sys::process::process_bitness(pid)?;
    Ok(SessionMemory {
        process: Arc::new(process),
        bitness,
        pid,
        modules: Mutex::new(None),
    })
}

#[cfg(windows)]
fn open_session_memory_real(pid: u32) -> Result<SessionMemory, MemoryError> {
    let process = vosu_sys::process::WindowsProcessMemory::open(pid)?;
    let bitness = process.bitness()?;
    Ok(SessionMemory {
        process: Arc::new(process),
        bitness,
        pid,
        modules: Mutex::new(None),
    })
}

#[cfg(target_os = "linux")]
fn build_module_table_real(pid: u32) -> Option<Arc<dyn ModuleResolver + Send + Sync>> {
    super::super::scan::LinuxModuleTable::snapshot(pid)
        .ok()
        .map(|table| Arc::new(table) as Arc<dyn ModuleResolver + Send + Sync>)
}

#[cfg(windows)]
fn build_module_table_real(pid: u32) -> Option<Arc<dyn ModuleResolver + Send + Sync>> {
    super::super::scan::WindowsModuleTable::snapshot(pid)
        .ok()
        .map(|table| Arc::new(table) as Arc<dyn ModuleResolver + Send + Sync>)
}

/// A [`ModuleResolver`] that never resolves anything: used for a test's
/// synthetic pid ([`set_session_memory_hook_for_tests`]), which has no
/// real `/proc`/loader table to enumerate and whose test plans never use
/// `VOSU_BASE_MODULE` anyway.
#[cfg(any(test, feature = "test-harness"))]
struct NoModules;

#[cfg(any(test, feature = "test-harness"))]
impl ModuleResolver for NoModules {
    fn resolve(&self, _module: &str) -> Option<u64> {
        None
    }
}

#[cfg(any(test, feature = "test-harness"))]
fn session_memory_hooks() -> &'static Mutex<HashMap<u32, Arc<SessionMemory>>> {
    static HOOKS: std::sync::LazyLock<Mutex<HashMap<u32, Arc<SessionMemory>>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &HOOKS
}

/// Test-only seam: substitutes [`open_session_memory`]'s real OS call for
/// `pid` with an already-built `process`/`bitness` pair (e.g. wrapping a
/// deliberately slow or blockable [`ProcessMemory`]), and
/// [`build_module_table`] with [`NoModules`] for the same `pid` — a test
/// using this never needs `pid` to name a real process at all. Keyed by
/// `pid`, not a single global override, so independent tests running in
/// the same process never collide as long as each picks its own synthetic
/// pid.
#[cfg(any(test, feature = "test-harness"))]
pub(crate) fn set_session_memory_hook_for_tests(
    pid: u32,
    process: Arc<dyn ProcessMemory>,
    bitness: ProcessBitness,
) {
    let mut hooks = session_memory_hooks()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    assert!(
        !hooks.contains_key(&pid),
        "synthetic pid {pid} is already hooked: pids must be unique per test (a process-wide \
         registry keyed by pid silently overwrites the earlier test's `ProcessMemory`, \
         redirecting its reads to the wrong test, when two tests share one); call \
         `clear_session_memory_hook_for_tests` first if this test genuinely reuses it"
    );
    hooks.insert(
        pid,
        Arc::new(SessionMemory {
            process,
            bitness,
            pid,
            modules: Mutex::new(None),
        }),
    );
}

/// Removes `pid`'s hook, the inverse of
/// [`set_session_memory_hook_for_tests`]:
/// `vosu_core::plugin::test_support::clear_session_memory` exposes this
/// so a test can retire a synthetic pid once it is done with it instead
/// of leaking it in the registry for the rest of the process's life.
#[cfg(any(test, feature = "test-harness"))]
pub(crate) fn clear_session_memory_hook_for_tests(pid: u32) {
    session_memory_hooks()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&pid);
}

/// N2: lets a test make [`open_session_memory`] fail `pid`'s next
/// `count` calls with `VOSU_ERR_PERMISSION_DENIED` before falling
/// through to [`session_memory_hooks`]'s own success hook — proving a
/// failed open is never cached (`SessionMemorySlot`'s own doc comment):
/// the submission that hits it fails, but the next one against the same
/// handle retries the open and succeeds.
#[cfg(test)]
fn open_failure_hooks() -> &'static Mutex<HashMap<u32, usize>> {
    static HOOKS: std::sync::LazyLock<Mutex<HashMap<u32, usize>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &HOOKS
}

#[cfg(test)]
pub(crate) fn set_open_failure_count_for_tests(pid: u32, count: usize) {
    open_failure_hooks()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(pid, count);
}

fn open_session_memory(pid: u32) -> Result<Arc<SessionMemory>, SessionMemoryError> {
    #[cfg(test)]
    {
        let mut failures = open_failure_hooks()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(remaining) = failures.get_mut(&pid)
            && *remaining > 0
        {
            *remaining -= 1;
            return Err(SessionMemoryError::PermissionDenied);
        }
    }
    #[cfg(any(test, feature = "test-harness"))]
    {
        let hooked = session_memory_hooks()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&pid)
            .cloned();
        if let Some(hooked) = hooked {
            return Ok(hooked);
        }
    }
    open_session_memory_real(pid)
        .map(Arc::new)
        .map_err(|error| classify_open_error(pid, &error))
}

fn build_module_table(pid: u32) -> Option<Arc<dyn ModuleResolver + Send + Sync>> {
    #[cfg(any(test, feature = "test-harness"))]
    {
        let hooked = session_memory_hooks()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(&pid);
        if hooked {
            return Some(Arc::new(NoModules));
        }
    }
    build_module_table_real(pid)
}

/// Whether [`set_session_memory_hook_for_tests`] registered a hook for
/// `pid`: `host::scans`'s own test-only module-table seam checks the same
/// hook registry (module doc comment) rather than keeping an independent
/// one keyed by the same pids.
#[cfg(any(test, feature = "test-harness"))]
pub(super) fn is_hooked_for_tests(pid: u32) -> bool {
    session_memory_hooks()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains_key(&pid)
}

/// Lazily builds, and caches per session, the module table a
/// `PlanBase::Module` root needs (P1-1): `read_plan::resolve_root` only
/// ever calls [`ModuleResolver::resolve`] for a `Module` root, never an
/// `Absolute` one, so this type's own work — opening a loader table,
/// parsing `/proc/<pid>/maps` — never runs at all for the common
/// `Absolute` case this phase exists to stop paying for on every call.
/// Refreshes the cached table once on a resolve miss before reporting a
/// module unresolved, since the named module may have loaded since the
/// last snapshot; a snapshot that cannot be built at all still resolves
/// nothing, which the caller already reports as a per-read
/// `VOSU_READ_NOT_READY`.
struct CachingModuleResolver {
    memory: Arc<SessionMemory>,
}

impl ModuleResolver for CachingModuleResolver {
    fn resolve(&self, module: &str) -> Option<u64> {
        let pid = self.memory.pid;
        resolve_cached(&self.memory.modules, module, || build_module_table(pid))
    }
}

/// The caching/refresh-on-miss policy [`CachingModuleResolver`] applies,
/// factored out so it is testable independently of a real `pid` or the OS
/// (P1-1): a hit against the already-cached `table` never calls `build`
/// at all; a miss (cache empty, or the named module not in the cached
/// table) calls `build` exactly once and replaces the cache with
/// whatever it returns, even `None`'s absence of a table, so a plan
/// against a target with no resolvable module does not rebuild out of
/// `cache`'s own storage — only ever from `build` being called again the
/// next time this function runs.
fn resolve_cached(
    cache: &Mutex<Option<Arc<dyn ModuleResolver + Send + Sync>>>,
    module: &str,
    build: impl FnOnce() -> Option<Arc<dyn ModuleResolver + Send + Sync>>,
) -> Option<u64> {
    let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(table) = cache.as_ref()
        && let Some(address) = table.resolve(module)
    {
        return Some(address);
    }
    let fresh = build()?;
    let address = fresh.resolve(module);
    *cache = Some(fresh);
    address
}

/// N3: a test-only seam standing in for a `READ_PLAN_CANCEL`/detach
/// landing on another thread in the narrow window between
/// [`PluginHost::submit_read_plan_async`]'s own `begin` and
/// `set_queue_ticket` calls — a `Mutex`, not a `thread_local`, since the
/// real submission may run on a different thread than the test that sets
/// this hook (unlike `scan.rs`'s own `RACE_HOOK_FOR_TESTS`, which a test
/// always races from a direct, same-thread `submit` call). Keyed by
/// `pid`, not a single global slot (unlike `scan.rs`'s own
/// thread-local), so an unrelated test's submission running concurrently
/// in the same test binary (`cargo test`'s default parallelism) can
/// never consume a hook meant for a different test's own pid, or run one
/// it never installed.
#[cfg(test)]
type AsyncSubmitRaceHook = Box<dyn FnOnce() + Send>;

#[cfg(test)]
fn async_submit_race_hooks_for_tests() -> &'static Mutex<HashMap<u32, AsyncSubmitRaceHook>> {
    static HOOKS: std::sync::LazyLock<Mutex<HashMap<u32, AsyncSubmitRaceHook>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &HOOKS
}

/// Installs a one-shot hook [`PluginHost::submit_read_plan_async`] runs,
/// for `pid`'s own submissions only, at the exact point its own window
/// for the N3 race opens.
#[cfg(test)]
pub(crate) fn set_async_submit_race_hook_for_tests(pid: u32, hook: impl FnOnce() + Send + 'static) {
    async_submit_race_hooks_for_tests()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(pid, Box::new(hook));
}

#[cfg(test)]
fn run_async_submit_race_hook_for_tests(pid: u32) {
    let hook = async_submit_race_hooks_for_tests()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&pid);
    if let Some(hook) = hook {
        hook();
    }
}

impl PluginHost {
    /// Returns `handle`'s cached process handle, opening (and caching) it
    /// on first use (point 1 of this phase). `pub(super)`: `host::scans`
    /// reuses this directly (module doc comment) instead of opening a
    /// second handle for a session this instance also has read plans
    /// outstanding against.
    ///
    /// F5: the global `read_plan_memory` lock is held only long enough
    /// to get or insert this handle's own [`SessionMemorySlot`] — never
    /// across `open_session_memory` itself, which on Linux parses
    /// `/proc/<pid>/maps`. Without this, every plugin's submissions,
    /// including a `VOSU_READ_PLAN_SYNC` read on an unrelated session,
    /// would wait behind one session's first (slowest) open.
    ///
    /// N2: only `Ok` is ever cached (`SessionMemorySlot`'s own doc
    /// comment) — a fast lock-free read against `slot.result` once it is
    /// set, otherwise `slot.open` single-flights concurrent openers of
    /// this *same* handle onto one real `open_session_memory` call,
    /// re-checking `slot.result` once that lock is held in case another
    /// opener already won while this one was waiting for it. A failed
    /// open is returned straight to its own caller, never written to
    /// `slot.result`, so the next submission against this handle (sync
    /// or async) retries the open instead of replaying the same failure
    /// for the rest of the session's life.
    pub(super) fn session_memory(
        &self,
        handle: SessionHandle,
        pid: u32,
    ) -> Result<Arc<SessionMemory>, SessionMemoryError> {
        let slot = Arc::clone(
            self.read_plan_memory
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(handle)
                .or_default(),
        );
        if let Some(memory) = slot.result.get() {
            return Ok(Arc::clone(memory));
        }
        let _open = slot.open.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(memory) = slot.result.get() {
            return Ok(Arc::clone(memory));
        }
        let memory = open_session_memory(pid)?;
        // Another opener can never have raced past the check above: it
        // would need `slot.open` too, and this call still holds it.
        let _ = slot.result.set(Arc::clone(&memory));
        Ok(memory)
    }

    /// `handle`'s session is gone (detached, declined, or superseded):
    /// drops its cached process handle, opened fresh the next time (if
    /// ever) this handle value is reused.
    pub(super) fn forget_read_plan_session(&self, handle: SessionHandle) {
        self.read_plan_memory
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&handle);
    }

    /// `handle`'s session (owned by `plugin_id`) is gone: cancels every
    /// read plan still outstanding under it, on every live instance of
    /// that plugin id (mirroring [`Self::dispatch_to`]'s own "every
    /// instance of this plugin id" fan-out).
    pub(super) fn cancel_read_plans_for_session(&self, plugin_id: &str, handle: SessionHandle) {
        let guard = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for (id, slot) in guard.iter() {
            if let InstanceSlot::Live(entry) = slot
                && entry.plugin_id == plugin_id
            {
                // F1: removes each freed plan's still-queued job from
                // this instance's own pool sub-queue immediately (mirrors
                // `Self::handle_read_plan_cancel`'s own fix); a job
                // actually found and removed there will now never call
                // its own `finish`, so `forget_cancelled` clears its
                // `cancelled`-set entry right here instead of leaking it.
                for (plan, ticket) in entry.outstanding_plans.cancel_session(handle.0) {
                    if self.background_pool.cancel(id.0, ticket) {
                        entry.outstanding_plans.forget_cancelled(plan);
                    }
                }
            }
        }
    }

    fn outstanding_plans_of(&self, instance: PluginInstanceId) -> Option<Arc<OutstandingPlans>> {
        self.instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&instance)
            .and_then(|slot| match slot {
                InstanceSlot::Live(entry) => Some(Arc::clone(&entry.outstanding_plans)),
                InstanceSlot::Creating { .. } | InstanceSlot::Destroying { .. } => None,
            })
    }

    /// The owning plugin and pid for `handle`, checked against
    /// `plugin_id`. `docs/v2/implementation-plan.md`'s own edge case ("A
    /// read plan for a detached handle answers `VOSU_ERR_TARGET_GONE`")
    /// covers an unknown handle the same way as a genuinely detached one,
    /// since `SessionRegistry::owner_and_pid` reports both as `None`.
    /// `pub(super)`: `host::scans`'s own §6.4 session resolution is the
    /// same ownership check (module doc comment), so it calls this
    /// directly instead of re-deriving it.
    pub(super) fn resolve_owned_session(
        &self,
        plugin_id: &str,
        handle: SessionHandle,
    ) -> Result<u32, vosu_status> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match sessions.owner_and_pid(handle) {
            None => Err(vosu_status::VOSU_ERR_TARGET_GONE),
            Some((owner, _)) if owner != plugin_id => Err(vosu_status::VOSU_ERR_PERMISSION_DENIED),
            Some((_, pid)) => Ok(pid),
        }
    }

    /// [`Self::resolve_owned_session`]'s allocation-free counterpart
    /// (D17): resolves `instance`'s owning plugin id and `handle`'s pid
    /// in one locked step, without [`PluginHost::plugin_id_of`]'s own
    /// `to_owned()` — on the raw `VOSU_READ_PLAN_SYNC` fast path
    /// ([`Self::handle_read_plan_submit_raw`], the only caller), that one
    /// heap allocation per read would be the last one left on this call
    /// stack (caught by
    /// `read_plan::tests::zero_alloc::in_process_sync_round_trip_allocates_nothing_after_warmup`,
    /// the core-level round-trip test the SDK-level
    /// `vosu-plugin-api::tests::plan_memory_alloc` fake-host test cannot
    /// reach).
    ///
    /// Host lock order: focus -> instances -> sessions -> matcher ->
    /// store/registry. No sessions guard may be held while acquiring instances.
    fn resolve_owned_session_for_instance(
        &self,
        instance: PluginInstanceId,
        handle: SessionHandle,
    ) -> Result<u32, vosu_status> {
        let instances = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(plugin_id) = instances.get(&instance).map(InstanceSlot::plugin_id) else {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT);
        };
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match sessions.owner_and_pid(handle) {
            None => Err(vosu_status::VOSU_ERR_TARGET_GONE),
            Some((owner, _)) if owner != plugin_id => Err(vosu_status::VOSU_ERR_PERMISSION_DENIED),
            Some((_, pid)) => Ok(pid),
        }
    }

    /// `VOSU_OP_READ_PLAN_SUBMIT` without `VOSU_READ_PLAN_SYNC` set (an
    /// async plan). A `VOSU_READ_PLAN_SYNC` submission never reaches this
    /// function at all: `decode_and_handle` (`host_services.rs`)
    /// intercepts it directly off the still-raw payload, before
    /// `Message::decode` ever runs, and this `PluginHost`'s own
    /// `handle_read_plan_sync` always answers that interception
    /// ([`Self::handle_read_plan_submit_raw`]) rather than falling
    /// through — so by the time `decode` does run for a
    /// `VOSU_OP_READ_PLAN_SUBMIT`, `raw.flags` is already known not to
    /// carry `VOSU_READ_PLAN_SYNC`, and `decode`'s own `sync = flags &
    /// VOSU_READ_PLAN_SYNC != 0` (`message.rs`) computes `false` from
    /// that same `raw.flags`. Called from
    /// [`super::PluginHostHandler::handle`].
    pub(super) fn handle_read_plan_submit(
        &self,
        instance: PluginInstanceId,
        submit: ReadPlanSubmit,
    ) -> vosu_status {
        debug_assert!(
            !submit.sync,
            "a VOSU_READ_PLAN_SYNC submission is always intercepted by \
             handle_read_plan_sync before Message::decode runs"
        );
        let Some(plugin_id) = self.plugin_id_of(instance) else {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        };
        let handle = SessionHandle(submit.handle);
        let pid = match self.resolve_owned_session(&plugin_id, handle) {
            Ok(pid) => pid,
            Err(status) => return status,
        };
        self.submit_read_plan_async(instance, handle, pid, submit)
    }

    /// `VOSU_OP_READ_PLAN_CANCEL` (§6.3): a silent no-op for an id this
    /// instance has no (or no longer has an) outstanding plan for
    /// (`OutstandingPlans::cancel`'s own doc comment), matching the
    /// opcode's own "no reply" shape — there is no failure mode here worth
    /// a non-`VOSU_OK` status.
    pub(super) fn handle_read_plan_cancel(
        &self,
        instance: PluginInstanceId,
        cancel: ReadPlanCancel,
    ) -> vosu_status {
        if let Some(outstanding) = self.outstanding_plans_of(instance) {
            // F1: a cancelled plan's job, if still queued (never started
            // by a worker), is removed from the pool's own per-key
            // sub-queue right away instead of being left to pile up
            // there until a worker happens to reach it; since its job
            // will now never call its own `finish`, `forget_cancelled`
            // clears its `cancelled`-set entry right here instead of
            // leaking it forever.
            if let Some(ticket) = outstanding.cancel(cancel.plan)
                && self.background_pool.cancel(instance.0, ticket)
            {
                outstanding.forget_cancelled(cancel.plan);
            }
        }
        vosu_status::VOSU_OK
    }

    /// `VOSU_OP_READ_PLAN_SUBMIT` with `VOSU_READ_PLAN_SYNC` set, taken
    /// directly off the still-raw, not-yet-decoded payload
    /// ([`super::super::host_services::PluginHostHandler::handle_read_plan_sync`]'s
    /// own doc comment, D17): reads only `payload`'s
    /// `vosu_read_plan_submit_header` (a `Copy` struct, no allocation) to
    /// resolve the session, then runs [`read_plan::execute_into`]
    /// straight against `payload` into [`SYNC_RESULT_BUF`]'s per-thread,
    /// reused buffer — never building an owned `ReadPlanSubmit` or
    /// `ReadPlanResult` at all — and delivers that buffer's bytes with
    /// [`deliver_sync_result_raw`], which skips [`OwnedMessage`]'s own
    /// owned-`Vec<u8>` copy too. `handle_read_plan_submit`'s own doc
    /// comment explains why this is the only function that ever handles
    /// `VOSU_READ_PLAN_SYNC`: `decode_and_handle` intercepts it here,
    /// before `Message::decode` ever runs, so a decoded `ReadPlanSubmit`
    /// is never synchronous.
    pub(super) fn handle_read_plan_submit_raw(
        &self,
        instance: PluginInstanceId,
        payload: &[u8],
        flags: u32,
    ) -> vosu_status {
        if read_plan::on_dispatch_thread() {
            return SyncReadPlanError::OnDispatchThread.into();
        }
        // P3 (F4 review): validated before touching the session or
        // opening any process memory, exactly like the deleted typed
        // path (`Message::decode`) and the async path both do — a
        // malformed or unknown-flag SYNC plan must answer
        // `VOSU_ERR_INVALID_ARGUMENT`, never `VOSU_ERR_TARGET_GONE`/
        // `VOSU_ERR_PERMISSION_DENIED` from resolving a session this
        // submission was never going to execute against anyway
        // (`execute_into`'s own doc comment: "a plan validation error
        // takes priority over a runtime/target-state one"). Both
        // allocation-free; `execute_into` repeats them on the same
        // `payload` below, which is harmless (idempotent) and keeps its
        // own contract self-sufficient for every caller, not just this
        // one.
        if validate_flags(vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT, flags).is_err()
            || validate_read_plan_submit(payload).is_err()
        {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let header: vosu_read_plan_submit_header = match read_flat_header(payload) {
            Ok(header) => header,
            Err(_) => return vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        };
        let handle = SessionHandle(header.handle);
        let pid = match self.resolve_owned_session_for_instance(instance, handle) {
            Ok(pid) => pid,
            Err(status) => return status,
        };
        let memory = match self.session_memory(handle, pid) {
            Ok(memory) => memory,
            Err(error) => return error.into(),
        };
        let modules = CachingModuleResolver {
            memory: Arc::clone(&memory),
        };
        let deadline = Instant::now() + self.read_plan_deadline();
        SYNC_RESULT_BUF.with(|buf| {
            let mut buf = buf.borrow_mut();
            if let Err(error) = read_plan::execute_into(
                payload,
                flags,
                memory.process.as_ref(),
                memory.bitness,
                &modules,
                deadline,
                &mut buf,
            ) {
                return vosu_status::from(error);
            }
            self.deliver_sync_result_raw(instance, &buf)
        })
    }

    /// The §4 SYNC exception itself (D17): delivers `payload` — already
    /// the flat `VOSU_OP_READ_PLAN_RESULT` bytes [`read_plan::execute_into`]
    /// wrote — through a direct, non-queued plugin call
    /// ([`super::super::dispatch::dispatch_raw`]) on the calling
    /// (submitting) thread, before this call — and so `send` — returns.
    /// No `Message`/owned-`Vec<u8>` copy of it is ever made. Never holds
    /// `self.instances` across that call: the plugin's own `dispatch` may
    /// call back into this host reentrant (e.g. `LOG_RECORD`), which a
    /// lock still held here would deadlock against.
    ///
    /// P1 (F4 review): holds [`DispatchThreadScope::enter`] around the
    /// `dispatch_raw` call, exactly as `dispatch.rs::run_job` already does
    /// for the ordinary queued delivery path. Without this, a plugin that
    /// submits `VOSU_READ_PLAN_SYNC` through a raw `send` (not through
    /// `PlanMemory`, e.g. any C plugin, or an SDK plugin calling
    /// `HostServices::submit_read_plan_sync` directly) and then submits
    /// another synchronous plan from inside this very `on_message` call
    /// would re-enter `handle_read_plan_submit_raw` on the same thread:
    /// `SYNC_RESULT_BUF.borrow_mut()` would panic (`BorrowMutError`)
    /// instead of the on-dispatch-thread refusal
    /// (`include/vosu_plugin.h:396-399`) firing first, and the root
    /// `panic = "abort"` profile would abort the whole process.
    fn deliver_sync_result_raw(&self, instance: PluginInstanceId, payload: &[u8]) -> vosu_status {
        let target = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&instance)
            .and_then(|slot| match slot {
                InstanceSlot::Live(entry) => Some(entry.instance.ctx_and_vtable()),
                InstanceSlot::Creating { .. } | InstanceSlot::Destroying { .. } => None,
            });
        let Some((vtable, ctx)) = target else {
            // P3: the instance tore down between this submission
            // arriving and its result finishing executing. The
            // submission itself genuinely ran to completion, but
            // `VOSU_OK` here would tell the plugin's own SDK the result
            // was delivered when nothing was: `VOSU_ERR_NOT_READY`
            // instead reports "no result" accurately.
            return vosu_status::VOSU_ERR_NOT_READY;
        };
        let _dispatch_thread = DispatchThreadScope::enter();
        dispatch_raw(
            &vtable,
            ctx,
            vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
            0,
            0,
            payload,
        );
        vosu_status::VOSU_OK
    }

    fn submit_read_plan_async(
        &self,
        instance: PluginInstanceId,
        handle: SessionHandle,
        pid: u32,
        submit: ReadPlanSubmit,
    ) -> vosu_status {
        let Some(outstanding) = self.outstanding_plans_of(instance) else {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        };
        let memory = match self.session_memory(handle, pid) {
            Ok(memory) => memory,
            Err(error) => return error.into(),
        };
        // P1-3: a wrong `ref_op` width is validated synchronously, before
        // admission — an accepted async plan has no synchronous caller
        // left to report `VOSU_ERR_INVALID_ARGUMENT` to once a worker
        // discovers this later.
        let pointer_width = u32::try_from(memory.bitness.pointer_size()).unwrap_or(u32::MAX);
        if !read_plan::ref_op_width_ok(&submit, pointer_width) {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let plan = submit.plan;
        if let Err(error) = outstanding.begin(plan, handle.0) {
            return error.into();
        }
        // N3: deterministic test seam for the race this method's own
        // `set_queue_ticket` call below closes — a test installs a hook
        // here that cancels `plan` right in this window, standing in for
        // `READ_PLAN_CANCEL`/a detach landing on another thread between
        // `begin` (just above) and the job actually being queued.
        #[cfg(test)]
        run_async_submit_race_hook_for_tests(pid);
        let modules = CachingModuleResolver {
            memory: Arc::clone(&memory),
        };
        let deadline_duration = self.read_plan_deadline();
        // P1-2: a `Weak` reference, upgraded only to deliver. A strong
        // `Arc<PluginHost>` here could let a background-pool worker end
        // up being the thread that drops the last reference — which
        // self-joins (`ScanWorkerPool::drop` joins every worker,
        // including whichever one is running this very job) and aborts
        // or hangs instead of shutting down cleanly.
        let host = self.self_ref.clone();
        let spawned_outstanding = Arc::clone(&outstanding);
        let job = move || {
            // P2-7: skip the real work entirely if this plan was already
            // cancelled while queued — a plugin that cancels work it no
            // longer needs should not also cost the shared pool the time
            // of running it.
            if spawned_outstanding.is_cancelled(plan) {
                spawned_outstanding.finish(plan);
                return;
            }
            // P1-3: the deadline starts when execution actually begins,
            // not at submission time — a plan queued behind other work on
            // the shared pool must not already be expired before a
            // worker even picks it up.
            let deadline = Instant::now() + deadline_duration;
            let result = match read_plan::execute(
                &submit,
                memory.process.as_ref(),
                memory.bitness,
                &modules,
                deadline,
            ) {
                Ok(result) => result,
                Err(_ref_op_wrong_width) => {
                    // Unreachable in practice: already validated
                    // synchronously above against this same (cached)
                    // bitness. Handled defensively instead of panicking a
                    // worker thread.
                    spawned_outstanding.finish(plan);
                    return;
                }
            };
            let should_deliver = spawned_outstanding.finish(plan) == FinishOutcome::Deliver;
            if should_deliver && let Some(host) = host.upgrade() {
                let message = Message::ReadPlanResult(result);
                if let Err(error) = host.dispatch_to_instance(instance, &message, 0) {
                    host.log_dispatch_dropped(instance, message.opcode(), &error);
                }
            }
        };
        // P2-7: this plan was already admitted against the instance's own
        // 64-outstanding cap, so `instance`'s own sub-queue
        // (`ScanWorkerPool`'s own doc comment) always has room for it —
        // another instance's own admitted work can never delay it.
        let ticket = self.background_pool.submit(instance.0, job);
        // F1: lets a later `READ_PLAN_CANCEL`/session detach remove this
        // job from the pool's own sub-queue immediately if it still can
        // — a no-op if the job already ran by the time this runs. N3: a
        // cancel can still slip in before this runs (`begin` already
        // admitted the plan, but `READ_PLAN_CANCEL`/a detach arrives on
        // another thread before this call reaches `set_queue_ticket`'s
        // own lock); `CancelledMeanwhile` means the ticket was never
        // recorded, so remove the job from the pool directly with the
        // `ticket` already in hand and forget the `cancelled`-set entry
        // in turn — otherwise the job stays queued until a worker
        // happens to reach it, and `cancelled` leaks the entry forever
        // (F1's pattern, on this narrower window).
        if let read_plan::SetQueueTicketOutcome::CancelledMeanwhile =
            outstanding.set_queue_ticket(plan, ticket)
            && self.background_pool.cancel(instance.0, ticket)
        {
            outstanding.forget_cancelled(plan);
        }
        vosu_status::VOSU_OK
    }

    fn read_plan_deadline(&self) -> Duration {
        Duration::from_millis(self.read_plan_deadline_ms.load(Ordering::Relaxed))
    }
}

/// Test-only: shortens [`PluginHost::read_plan_deadline`] from
/// [`READ_PLAN_DEADLINE`] so a deliberately slow [`ProcessMemory`] (set up
/// through [`set_session_memory_hook_for_tests`]) trips `VOSU_ERR_TIMED_OUT`
/// in milliseconds instead of seconds.
#[cfg(test)]
pub(crate) fn set_read_plan_deadline_for_tests(host: &PluginHost, deadline: Duration) {
    host.read_plan_deadline_ms.store(
        u64::try_from(deadline.as_millis()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

#[cfg(test)]
mod tests;
