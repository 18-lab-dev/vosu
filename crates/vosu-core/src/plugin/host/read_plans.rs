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

#[cfg(test)]
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use vosu_plugin_api::{Message, ReadPlanCancel, ReadPlanSubmit, vosu_status};
use vosu_sys::process::{MemoryError, ProcessBitness, ProcessMemory};

use super::super::dispatch::OwnedMessage;
use super::super::host_services::PluginInstanceId;
use super::super::read_plan::{
    self, FinishOutcome, ModuleResolver, OutstandingPlans, SyncReadPlanError,
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
#[cfg(test)]
struct NoModules;

#[cfg(test)]
impl ModuleResolver for NoModules {
    fn resolve(&self, _module: &str) -> Option<u64> {
        None
    }
}

#[cfg(test)]
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
#[cfg(test)]
pub(crate) fn set_session_memory_hook_for_tests(
    pid: u32,
    process: Arc<dyn ProcessMemory>,
    bitness: ProcessBitness,
) {
    session_memory_hooks()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            pid,
            Arc::new(SessionMemory {
                process,
                bitness,
                pid,
                modules: Mutex::new(None),
            }),
        );
}

fn open_session_memory(pid: u32) -> Result<Arc<SessionMemory>, SessionMemoryError> {
    #[cfg(test)]
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
    #[cfg(test)]
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
#[cfg(test)]
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

impl PluginHost {
    /// Returns `handle`'s cached process handle, opening (and caching) it
    /// on first use (point 1 of this phase). `pub(super)`: `host::scans`
    /// reuses this directly (module doc comment) instead of opening a
    /// second handle for a session this instance also has read plans
    /// outstanding against.
    pub(super) fn session_memory(
        &self,
        handle: SessionHandle,
        pid: u32,
    ) -> Result<Arc<SessionMemory>, SessionMemoryError> {
        // P3: held across the whole check-open-insert sequence (opening
        // a process handle is a handful of syscalls, not unbounded I/O)
        // instead of releasing between the cache check and the insert —
        // the previous two-lock version let a `forget_read_plan_session`
        // for this same `handle` land in that gap and then have this
        // call's own insert put the now-stale handle straight back,
        // leaking an open process handle nothing would ever forget again.
        let mut cache = self
            .read_plan_memory
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match cache.entry(handle) {
            std::collections::hash_map::Entry::Occupied(entry) => Ok(Arc::clone(entry.get())),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let memory = open_session_memory(pid)?;
                entry.insert(Arc::clone(&memory));
                Ok(memory)
            }
        }
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
        for slot in guard.values() {
            if let InstanceSlot::Live(entry) = slot
                && entry.plugin_id == plugin_id
            {
                entry.outstanding_plans.cancel_session(handle.0);
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

    /// `VOSU_OP_READ_PLAN_SUBMIT` (§6.3), `VOSU_READ_PLAN_SYNC` included
    /// (D5). Called from [`super::PluginHostHandler::handle`].
    pub(super) fn handle_read_plan_submit(
        &self,
        instance: PluginInstanceId,
        submit: ReadPlanSubmit,
    ) -> vosu_status {
        let Some(plugin_id) = self.plugin_id_of(instance) else {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        };
        if submit.sync && read_plan::on_dispatch_thread() {
            // Checked up front, before touching the session at all:
            // `read_plan::deliver_sync` re-checks this itself once this
            // call reaches it below, but a submission from the host's own
            // dispatch thread is refused regardless of whether the named
            // session or process even exists (D5, §4).
            return SyncReadPlanError::OnDispatchThread.into();
        }
        let handle = SessionHandle(submit.handle);
        let pid = match self.resolve_owned_session(&plugin_id, handle) {
            Ok(pid) => pid,
            Err(status) => return status,
        };
        if submit.sync {
            self.deliver_read_plan_sync(instance, handle, pid, submit)
        } else {
            self.submit_read_plan_async(instance, handle, pid, submit)
        }
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
            outstanding.cancel(cancel.plan);
        }
        vosu_status::VOSU_OK
    }

    fn deliver_read_plan_sync(
        &self,
        instance: PluginInstanceId,
        handle: SessionHandle,
        pid: u32,
        submit: ReadPlanSubmit,
    ) -> vosu_status {
        let memory = match self.session_memory(handle, pid) {
            Ok(memory) => memory,
            Err(error) => return error.into(),
        };
        let modules = CachingModuleResolver {
            memory: Arc::clone(&memory),
        };
        let deadline = Instant::now() + self.read_plan_deadline();
        let result = match read_plan::deliver_sync(
            &submit,
            memory.process.as_ref(),
            memory.bitness,
            &modules,
            deadline,
        ) {
            Ok(result) => result,
            Err(error) => return error.into(),
        };
        self.deliver_sync_result(instance, result)
    }

    /// The §4 SYNC exception itself: delivers `VOSU_OP_READ_PLAN_RESULT`
    /// through a direct, non-queued plugin call
    /// ([`super::super::instance::PluginInstance::ctx_and_vtable`] plus
    /// [`OwnedMessage::call`]) on the calling (submitting) thread, before
    /// this call — and so `send` — returns. Never holds `self.instances`
    /// across that call: the plugin's own `dispatch` may call back into
    /// this host reentrant (e.g. `LOG_RECORD`), which a lock still held
    /// here would deadlock against.
    fn deliver_sync_result(
        &self,
        instance: PluginInstanceId,
        result: vosu_plugin_api::ReadPlanResult,
    ) -> vosu_status {
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
            // instead reports "no result" accurately (the SDK's own
            // `NoResult` case this leg of the race used to hide).
            return vosu_status::VOSU_ERR_NOT_READY;
        };
        if let Ok(owned) = OwnedMessage::from_message(&Message::ReadPlanResult(result), 0) {
            let _ = owned.call(&vtable, ctx);
        }
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
        self.background_pool.submit(instance.0, job);
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
