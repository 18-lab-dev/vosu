//! Routes `VOSU_OP_SCAN_REQUEST` through [`PluginHost`]
//! (`docs/v2/plugin-abi.md` §5, §6.4; `docs/v2/implementation-plan.md`
//! section 10, scan routing being read-plan routing's sibling item):
//! resolves a session handle to its live process exactly the way
//! `read_plans.rs` does for `VOSU_OP_READ_PLAN_SUBMIT` — reusing
//! [`PluginHost::session_memory`]/[`PluginHost::resolve_owned_session`]
//! directly (`read_plans.rs`'s own module doc comment), so a session this
//! instance also has read plans outstanding against shares the one cached
//! process handle rather than this module opening a second one, and
//! mapping a failed open the same way (P2-8: only a dead process is
//! `VOSU_ERR_TARGET_GONE`; a live one this host failed to open is
//! `VOSU_ERR_PERMISSION_DENIED`/`VOSU_ERR_BACKEND`) — and a module table
//! built fresh per request (`scan.rs`'s own documented rule for
//! `WindowsModuleTable`/`LinuxModuleTable`). A snapshot that cannot be
//! built at all falls back to a resolver that resolves nothing rather than
//! refusing the whole request: `scan::execute` already answers an
//! unresolved `module` with its own `module_not_found` error form, never a
//! call rejection, so a transient snapshot failure degrades the same way
//! (mirroring `read_plans.rs`'s own P1-1 fix).
//!
//! Every accepted scan then runs through [`scan::submit`] on
//! `PluginHost`'s own `background_pool` — one [`ScanWorkerPool`] shared
//! with `read_plans.rs`'s own async submissions (that field's own doc
//! comment: both opcodes' work is off the hot path, so splitting worker
//! threads between two pools buys nothing but doubles the idle OS
//! threads every `PluginHost` keeps parked). `scan::submit` already owns
//! the whole admission (the 64-outstanding cap, §5/§6.4), malformed-
//! pattern refusal, and exactly-once-delivery race against a concurrent
//! [`scan::OutstandingScans::cancel_session`] (`scan.rs`'s own F6 fix)
//! itself; this module never re-derives any of that bookkeeping, only
//! wires it to `PluginHost`'s session/instance state and delivers the
//! result through the normal dispatch queue
//! ([`PluginHost::dispatch_to_instance`]), carrying the request's own
//! `correlation_id` (§6.4: "there is no dedicated scan-id type: a scan is
//! correlated like every other CBOR request/reply pair" — unlike a read
//! plan, which is instead correlated by its own `plan` id). The delivery
//! closure captures only a `Weak<PluginHost>` (P1-2), upgraded once, only
//! to deliver — the same reason `read_plans.rs`'s own async job does:
//! `ScanWorkerPool::drop` joins every worker, including whichever one is
//! running the job that would otherwise hold the last strong
//! `Arc<PluginHost>`, which self-joins and aborts or hangs instead of
//! shutting down.
//!
//! `VOSU_OP_SCAN_REQUEST` has no `VOSU_READ_PLAN_SYNC`-equivalent opt-in
//! (§6.4 names none): every scan, however cheap, is delivered
//! asynchronously through the dispatch queue — so, unlike
//! `read_plans.rs`, this module has no SYNC/async split at all.
//!
//! `VOSU_OP_SCAN_REQUEST` carries no cancel opcode of its own (§6.4 names
//! none, unlike `VOSU_OP_READ_PLAN_CANCEL`): a session's detach or decline
//! is the only way an outstanding scan is ever cancelled, handled by
//! [`PluginHost::cancel_scans_for_session`] exactly where
//! `cancel_read_plans_for_session` is already called.

use std::sync::Arc;

use vosu_plugin_api::{Message, ScanRequest, vosu_status};

use super::super::scan::{self, ModuleRegion, ModuleRegions, OutstandingScans};
use super::super::session::SessionHandle;
use super::{InstanceSlot, PluginHost};
use crate::identity::PluginInstanceId;

#[cfg(target_os = "linux")]
fn build_module_regions_real(pid: u32) -> Option<Arc<dyn ModuleRegions + Send + Sync>> {
    scan::LinuxModuleTable::snapshot(pid)
        .ok()
        .map(|table| Arc::new(table) as Arc<dyn ModuleRegions + Send + Sync>)
}

#[cfg(windows)]
fn build_module_regions_real(pid: u32) -> Option<Arc<dyn ModuleRegions + Send + Sync>> {
    scan::WindowsModuleTable::snapshot(pid)
        .ok()
        .map(|table| Arc::new(table) as Arc<dyn ModuleRegions + Send + Sync>)
}

/// A [`ModuleRegions`] that never resolves anything: used for a test's
/// synthetic pid, mirroring `read_plans.rs`'s own `NoModules` — a test
/// using `super::read_plans::set_session_memory_hook_for_tests` has no
/// real `/proc`/loader table to enumerate, and its scans never set
/// `module` anyway. Also the production fallback when a real snapshot
/// cannot be built at all (module doc comment): a module-scoped scan then
/// answers `module_not_found` instead of the whole request being refused.
struct UnresolvedModuleRegions;

impl ModuleRegions for UnresolvedModuleRegions {
    fn resolve(&self, _module: &str) -> Option<ModuleRegion> {
        None
    }
}

/// Mirrors `read_plans.rs`'s own `build_module_table`: a fresh snapshot
/// every call (`scan.rs`'s documented rule), substituting
/// [`UnresolvedModuleRegions`] for a test's synthetic pid
/// ([`super::read_plans::is_hooked_for_tests`] — the same hook registry
/// `read_plans.rs` itself checks, so a test only ever registers one hook
/// per pid regardless of which opcode it exercises) or a real snapshot
/// that failed to build at all (module doc comment).
fn build_module_regions(pid: u32) -> Arc<dyn ModuleRegions + Send + Sync> {
    #[cfg(any(test, feature = "test-harness"))]
    {
        if super::read_plans::is_hooked_for_tests(pid) {
            return Arc::new(UnresolvedModuleRegions);
        }
    }
    build_module_regions_real(pid).unwrap_or_else(|| Arc::new(UnresolvedModuleRegions))
}

impl PluginHost {
    fn outstanding_scans_of(&self, instance: PluginInstanceId) -> Option<Arc<OutstandingScans>> {
        self.instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&instance)
            .and_then(|slot| match slot {
                InstanceSlot::Live(entry) => Some(Arc::clone(&entry.outstanding_scans)),
                InstanceSlot::Creating { .. } | InstanceSlot::Destroying { .. } => None,
            })
    }

    /// `handle`'s session (owned by `plugin_id`) is gone: cancels every
    /// scan still outstanding under it, on every live instance of that
    /// plugin id (mirroring
    /// [`Self::cancel_read_plans_for_session`]/`read_plans.rs`'s own
    /// "every instance of this plugin id" fan-out, itself mirroring
    /// [`Self::dispatch_to`]).
    pub(super) fn cancel_scans_for_session(&self, plugin_id: &str, handle: SessionHandle) {
        // Collects the matching instances' `OutstandingScans` (with their
        // owning instance id, F1: needed to remove a still-queued scan
        // from that instance's own pool sub-queue below) first and drops
        // `instances` before calling `cancel_session` on any of them:
        // `cancel_session` may synchronously call this module's own
        // `deliver` closure (the A1 error form, §6.4), which calls
        // `dispatch_to_instance` — itself locking `instances` — so still
        // holding that lock here would self-deadlock on every session
        // detach with a scan outstanding.
        let matching: Vec<(PluginInstanceId, Arc<OutstandingScans>)> = self
            .instances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .filter_map(|(id, slot)| match slot {
                InstanceSlot::Live(entry) if entry.plugin_id == plugin_id => {
                    Some((*id, Arc::clone(&entry.outstanding_scans)))
                }
                _ => None,
            })
            .collect();
        for (id, outstanding) in matching {
            // F1: removes each freed scan's still-queued job from this
            // instance's own pool sub-queue immediately, instead of
            // leaving it to pile up there until a worker happens to
            // reach it (mirrors `read_plans.rs::handle_read_plan_cancel`'s
            // own fix; a scan has no `VOSU_OP_READ_PLAN_CANCEL` equivalent,
            // so this session-detach path is the only one that can ever
            // need it).
            for ticket in outstanding.cancel_session(handle.0) {
                self.background_pool.cancel(id.0, ticket);
            }
        }
    }

    /// `VOSU_OP_SCAN_REQUEST` (§6.4). Called from
    /// [`super::PluginHostHandler::handle`], which also supplies the
    /// request's own `correlation_id` (module doc comment: echoed back
    /// unchanged in the matching `VOSU_OP_SCAN_RESULT`, never `0` the way
    /// every `read_plans.rs` delivery uses, since a scan has no `plan`-id
    /// equivalent to correlate by instead).
    pub(super) fn handle_scan_request(
        &self,
        instance: PluginInstanceId,
        correlation_id: u64,
        request: ScanRequest,
    ) -> vosu_status {
        let Some(plugin_id) = self.plugin_id_of(instance) else {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        };
        let Some(outstanding) = self.outstanding_scans_of(instance) else {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        };
        let handle = SessionHandle(request.session);
        let pid = match self.resolve_owned_session(&plugin_id, handle) {
            Ok(pid) => pid,
            Err(status) => return status,
        };
        let memory = match self.session_memory(handle, pid) {
            Ok(memory) => memory,
            Err(error) => return error.into(),
        };
        let modules = build_module_regions(pid);
        // P1-2: a `Weak` reference, upgraded only to deliver — see this
        // module's own doc comment.
        let host = self.self_ref.clone();
        let process = Arc::clone(&memory.process);
        let result = scan::submit(
            &outstanding,
            &self.background_pool,
            request,
            process,
            modules,
            scan::ScanRouting {
                session: handle.0,
                queue_key: instance.0,
            },
            move |result| {
                if let Some(host) = host.upgrade() {
                    let message = Message::ScanResult(result);
                    if let Err(error) =
                        host.dispatch_to_instance(instance, &message, correlation_id)
                    {
                        host.log_dispatch_dropped(instance, message.opcode(), &error);
                    }
                }
            },
        );
        match result {
            Ok(()) => vosu_status::VOSU_OK,
            Err(error) => error.into(),
        }
    }
}

#[cfg(test)]
mod tests;
