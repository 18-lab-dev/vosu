//! Routes `VOSU_OP_READ_PLAN_SUBMIT`/`VOSU_OP_READ_PLAN_CANCEL` through
//! [`PluginHost`] (`docs/v2/plugin-abi.md` §4's SYNC exception, §5, §6.3;
//! `docs/v2/implementation-plan.md` section 10, D5): resolves a session
//! handle to its live process, opening it once and caching the handle per
//! session ([`PluginHost::session_memory`]; dropped on detach,
//! [`PluginHost::forget_read_plan_session`]) — a module table is built
//! fresh on every call instead, `scan.rs`'s own documented rule for
//! `WindowsModuleTable`/`LinuxModuleTable`. An async
//! (non-`VOSU_READ_PLAN_SYNC`) submission then executes on
//! `PluginHost`'s own `background_pool` — one [`ScanWorkerPool`] shared
//! with `scans.rs` (that field's own doc comment: both opcodes' work is
//! off the hot path, so splitting worker threads between two pools buys
//! nothing but doubles the idle OS threads every `PluginHost` keeps
//! parked) — and delivers its result through the normal dispatch queue
//! ([`PluginHost::dispatch_to_instance`]); a `VOSU_READ_PLAN_SYNC`
//! submission instead delivers its result through a direct, non-queued
//! plugin call on the submitting thread itself, before this call (and so
//! `send`) returns — §4's one documented exception to per-instance
//! serialization.
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
//! the one cached handle.

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant};

use vosu_plugin_api::{Message, ReadPlanCancel, ReadPlanSubmit, vosu_status};
use vosu_sys::process::{ProcessBitness, ProcessMemory};

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

/// [`ScanWorkerPool::new`], queue capacity
/// [`super::super::read_plan::MAX_OUTSTANDING_ASYNC_PLANS`] +
/// [`super::super::scan::MAX_OUTSTANDING_SCANS`] (128): both caps admit
/// independently, so up to that many jobs can be genuinely queued at
/// once; sized for the sum so neither submission path's own
/// `OutstandingPlans`/`OutstandingScans` admission cap can ever be
/// second-guessed by this pool's queue being the thing that actually
/// refuses a legitimately-admitted job.
pub(super) fn new_background_pool() -> ScanWorkerPool {
    ScanWorkerPool::new(
        BACKGROUND_WORKER_COUNT,
        super::super::read_plan::MAX_OUTSTANDING_ASYNC_PLANS
            + super::super::scan::MAX_OUTSTANDING_SCANS,
    )
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
/// backing a `VOSU_BASE_MODULE` root is deliberately NOT cached alongside
/// it — see this module's own doc comment.
pub(super) struct SessionMemory {
    /// `pub(super)`: `host::scans` reads this directly (module doc
    /// comment) rather than caching a second handle for the same session.
    pub(super) process: Arc<dyn ProcessMemory>,
    pub(super) bitness: ProcessBitness,
}

#[cfg(target_os = "linux")]
fn open_session_memory_real(pid: u32) -> Option<SessionMemory> {
    let process = vosu_sys::process::open_process(pid).ok()?;
    let bitness = vosu_sys::process::process_bitness(pid).ok()?;
    Some(SessionMemory {
        process: Arc::new(process),
        bitness,
    })
}

#[cfg(windows)]
fn open_session_memory_real(pid: u32) -> Option<SessionMemory> {
    let process = vosu_sys::process::WindowsProcessMemory::open(pid).ok()?;
    let bitness = process.bitness().ok()?;
    Some(SessionMemory {
        process: Arc::new(process),
        bitness,
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
        .insert(pid, Arc::new(SessionMemory { process, bitness }));
}

fn open_session_memory(pid: u32) -> Option<Arc<SessionMemory>> {
    #[cfg(test)]
    {
        let hooked = session_memory_hooks()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&pid)
            .cloned();
        if let Some(hooked) = hooked {
            return Some(hooked);
        }
    }
    open_session_memory_real(pid).map(Arc::new)
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
    ) -> Option<Arc<SessionMemory>> {
        {
            let cache = self
                .read_plan_memory
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(memory) = cache.get(&handle) {
                return Some(Arc::clone(memory));
            }
        }
        let memory = open_session_memory(pid)?;
        self.read_plan_memory
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(handle, Arc::clone(&memory));
        Some(memory)
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
            self.deliver_read_plan_sync(instance, pid, submit)
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
        pid: u32,
        submit: ReadPlanSubmit,
    ) -> vosu_status {
        let handle = SessionHandle(submit.handle);
        let Some(memory) = self.session_memory(handle, pid) else {
            return vosu_status::VOSU_ERR_TARGET_GONE;
        };
        let Some(modules) = build_module_table(pid) else {
            return vosu_status::VOSU_ERR_TARGET_GONE;
        };
        let deadline = Instant::now() + self.read_plan_deadline();
        let result = match read_plan::deliver_sync(
            &submit,
            memory.process.as_ref(),
            memory.bitness,
            modules.as_ref(),
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
            // The instance tore down between this submission arriving and
            // its result finishing executing: the submission itself is
            // still accepted (it ran to completion), there is simply
            // nothing left to deliver the result to.
            return vosu_status::VOSU_OK;
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
        let plan = submit.plan;
        if let Err(error) = outstanding.begin(plan, handle.0) {
            return error.into();
        }
        let Some(memory) = self.session_memory(handle, pid) else {
            outstanding.finish(plan);
            return vosu_status::VOSU_ERR_TARGET_GONE;
        };
        let deadline = Instant::now() + self.read_plan_deadline();
        let host = self.arc();
        let spawned_outstanding = Arc::clone(&outstanding);
        let spawn_result = self.background_pool.try_spawn(move || {
            // §6.3/§5: `RefOpWrongWidth`/`TargetGone`/`TimedOut` reject the
            // whole submission, which for an async plan means no later
            // `VOSU_OP_READ_PLAN_RESULT` at all — there is no synchronous
            // caller left to report a status to (recorded as a spec
            // ambiguity in this phase's handoff notes).
            let outcome = build_module_table(pid).and_then(|modules| {
                read_plan::execute(
                    &submit,
                    memory.process.as_ref(),
                    memory.bitness,
                    modules.as_ref(),
                    deadline,
                )
                .ok()
            });
            let should_deliver = spawned_outstanding.finish(plan) == FinishOutcome::Deliver;
            if let (true, Some(result)) = (should_deliver, outcome) {
                let _ = host.dispatch_to_instance(instance, &Message::ReadPlanResult(result), 0);
            }
        });
        if spawn_result.is_err() {
            outstanding.finish(plan);
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
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
