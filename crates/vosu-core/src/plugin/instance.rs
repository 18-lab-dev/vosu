//! Plugin instance lifecycle: `create`, dispatch (one dedicated thread per
//! instance, or the shared `concurrent_dispatch` pool), and `destroy` with
//! a grace period (`docs/v2/plugin-abi.md` §4).
//!
//! An instance runs on one backend (`super::execution`): local code this
//! process calls directly, or a process endpoint a `ProcessLauncher`
//! returned. Dispatch scheduling, the reserved outbox and in-flight
//! accounting are the same for both; only the call target and teardown
//! differ.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

use libloading::Library;
use vosu_plugin_api::{vosu_bytes, vosu_plugin_vtable, vosu_status};

use super::dispatch::{
    DispatchTarget, InFlightTracker, Job, OwnedMessage, PluginCtx, SerialWorker, WorkerPool,
};
use super::error::HostError;
use super::execution::{
    Execution, InstanceLoss, LocalCode, LossState, PluginIngress, ProcessEndpoint, ProcessLaunch,
    ProcessLauncher,
};
use super::host_services::{self, HostContext, PluginHostHandler};
use super::registry::LoadedPlugin;
use crate::identity::PluginInstanceId;

/// Bound for a serial instance's own dispatch queue (`AGENTS.md`: no
/// unbounded channels); the shared `concurrent_dispatch` pool's queue is
/// bounded separately by whoever builds its `WorkerPool`.
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;

/// Grace period `Drop::drop` gives an instance that was never explicitly
/// `destroy()`d (an early `?` return before cleanup ran, a `Vec` drop,
/// ...): the same precedent `dev_plugins::PLUGIN_DESTROY_GRACE` cites, but
/// defined here independently since there is no caller in a `Drop` to pick
/// one.
const DROP_DESTROY_GRACE: Duration = Duration::from_secs(5);

enum Dispatch {
    /// One dedicated thread per instance (default, §4).
    Serial(SerialWorker),
    /// `concurrent_dispatch = true`: the shared worker pool (§4).
    Concurrent(Arc<WorkerPool>),
}

/// What an instance owns besides its dispatch scheduling.
enum Backend {
    Local {
        vtable: vosu_plugin_vtable,
        ctx: PluginCtx,
        // `Some` until `destroy_inner` either runs the plugin's own
        // `destroy` to completion or times out: the ABI documents
        // `host`/`host_ctx` as outliving every call `create` can make
        // through them for the instance's whole lifetime, so a timed-out
        // teardown leaks this (`leak_on_timeout`) instead of freeing it
        // while a stuck plugin thread may still read it.
        host_ctx: Option<Box<HostContext>>,
        // Co-owned, not just held: unloading the library while this
        // instance's `plugin_ctx` still points into it would be immediate
        // undefined behavior the next `dispatch`/`destroy` call makes.
        // `None` for a bundled plugin, which is linked into this
        // executable rather than `dlopen`ed. Leaked alongside `host_ctx`
        // on a `destroy` timeout for the same reason.
        library: Option<Arc<Library>>,
    },
    Process {
        endpoint: Arc<dyn ProcessEndpoint>,
        loss: Arc<LossState>,
    },
}

impl Backend {
    /// Where this backend's host → plugin messages go: a copy of the local
    /// vtable/context pair, or a shared reference to the process endpoint.
    fn target(&self) -> DispatchTarget {
        match self {
            Self::Local { vtable, ctx, .. } => DispatchTarget::Local {
                vtable: *vtable,
                ctx: *ctx,
            },
            Self::Process { endpoint, .. } => DispatchTarget::Process(Arc::clone(endpoint)),
        }
    }
}

/// A created, running plugin instance.
pub struct PluginInstance {
    id: PluginInstanceId,
    backend: Backend,
    in_flight: InFlightTracker,
    dispatch: Dispatch,
    outbox: Arc<super::outbox::ReservedOutbox>,
    /// Set once `destroy_inner` has run, whether from an explicit
    /// `destroy()` call or from `Drop`, so `Drop` never runs it twice.
    destroyed: bool,
}

impl PluginInstance {
    /// Creates an instance of a package whose execution is local code. A
    /// `runtime = "process"` package needs a launcher
    /// ([`Self::create_with_launcher`]) and is refused here with
    /// `HostError::ProcessRuntimeUnavailable`, never run in this process.
    pub fn create(
        id: PluginInstanceId,
        plugin: &LoadedPlugin,
        init: &[u8],
        handler: Arc<dyn PluginHostHandler>,
        concurrent_pool: Option<Arc<WorkerPool>>,
    ) -> Result<Self, HostError> {
        Self::create_with_launcher(id, plugin, init, handler, concurrent_pool, None)
    }

    /// Calls the local `create`, or launches one process child through
    /// `launcher`, then starts this instance's dedicated dispatch thread, or
    /// registers it with `concurrent_pool` when the manifest sets
    /// `concurrent_dispatch = true`.
    pub fn create_with_launcher(
        id: PluginInstanceId,
        plugin: &LoadedPlugin,
        init: &[u8],
        handler: Arc<dyn PluginHostHandler>,
        concurrent_pool: Option<Arc<WorkerPool>>,
        launcher: Option<&dyn ProcessLauncher>,
    ) -> Result<Self, HostError> {
        // Resolved and validated before any plugin code runs: a
        // `concurrent_dispatch` plugin with no pool must never reach
        // `create` at all. Checking this after `create` already succeeded
        // would leak the `plugin_ctx` it just returned and free `host_ctx`
        // out from under a plugin that may still be holding it
        // (`include/vosu_plugin.h`'s `host`/`host_ctx` lifetime contract),
        // or leave a launched child running.
        let concurrent_pool = if plugin.manifest().concurrent_dispatch {
            Some(concurrent_pool.ok_or(HostError::ConcurrentPoolMissing)?)
        } else {
            None
        };
        let backend = match plugin.execution() {
            Execution::Local(code) => Self::create_local(id, code, init, handler)?,
            Execution::Process(package) => {
                let launcher = launcher.ok_or(HostError::ProcessRuntimeUnavailable)?;
                let loss = LossState::new();
                let endpoint = launcher.launch(ProcessLaunch {
                    instance: id,
                    manifest: plugin.manifest(),
                    package,
                    init,
                    ingress: PluginIngress::new(id, Arc::clone(&handler)),
                    loss: InstanceLoss::new(id, Arc::clone(&loss), &handler),
                })?;
                Backend::Process { endpoint, loss }
            }
        };
        let dispatch = match concurrent_pool {
            Some(pool) => Dispatch::Concurrent(pool),
            None => Dispatch::Serial(SerialWorker::spawn(
                format!("vosu-plugin-{}", id.0),
                DEFAULT_QUEUE_CAPACITY,
            )),
        };
        let in_flight = InFlightTracker::new();
        let queue = match &dispatch {
            Dispatch::Serial(worker) => worker.queue(),
            Dispatch::Concurrent(pool) => pool.queue(),
        };
        let outbox = super::outbox::ReservedOutbox::new(queue, backend.target(), in_flight.clone());
        Ok(Self {
            id,
            backend,
            in_flight,
            outbox,
            dispatch,
            destroyed: false,
        })
    }

    fn create_local(
        id: PluginInstanceId,
        code: &LocalCode,
        init: &[u8],
        handler: Arc<dyn PluginHostHandler>,
    ) -> Result<Backend, HostError> {
        let vtable = code.vtable;
        let create = vtable.create.ok_or(HostError::MissingRequiredFunction)?;
        let mut host_ctx = Box::new(HostContext::new(id, handler));
        let init_bytes = vosu_bytes {
            data: init.as_ptr(),
            len: init.len() as u32,
        };
        // SAFETY: `host_services::HOST_SERVICES` is a process-wide
        // `'static`, so `host` already satisfies the ABI's "outlives every
        // call" contract on its own, rather than relying on a local this
        // call borrows only for its own stack frame; `host_ctx`'s address
        // is borrowed for the instance's whole lifetime, the ABI's own
        // contract on `create`'s `host_ctx`, which this instance keeps
        // boxed and alive until `destroy` (or leaks on a `destroy`
        // timeout, never freeing it while still possibly in use);
        // `init_bytes` points at `init`, alive for the call.
        let plugin_ctx = unsafe {
            create(
                &host_services::HOST_SERVICES as *const _,
                (host_ctx.as_mut() as *mut HostContext).cast::<c_void>(),
                init_bytes,
            )
        };
        if plugin_ctx.is_null() {
            return Err(HostError::CreateRejected);
        }
        Ok(Backend::Local {
            vtable,
            ctx: PluginCtx(plugin_ctx),
            host_ctx: Some(host_ctx),
            library: code.library.clone(),
        })
    }

    pub fn id(&self) -> PluginInstanceId {
        self.id
    }

    /// Whether this instance's process child or channel was reported lost
    /// (`InstanceLoss::report`). Always `false` for local code.
    pub fn is_lost(&self) -> bool {
        match &self.backend {
            Backend::Local { .. } => false,
            Backend::Process { loss, .. } => loss.is_lost(),
        }
    }

    pub fn reserve_outbox(&self) -> Result<super::outbox::OutboxSlot, HostError> {
        self.outbox.reserve()
    }

    /// Enqueues one message for this instance's dispatch thread (serial)
    /// or the shared pool (`concurrent_dispatch`). The returned receiver
    /// yields the plugin's `dispatch` status once it runs; drop it if the
    /// caller does not need to observe completion.
    pub fn dispatch(&self, message: OwnedMessage) -> Result<Receiver<vosu_status>, HostError> {
        let (reply_tx, reply_rx) = sync_channel(1);
        let job = Job {
            target: self.backend.target(),
            message,
            task: None,
            reply: Some(reply_tx),
            in_flight: self.enter_in_flight(),
        };
        self.submit(job)?;
        Ok(reply_rx)
    }

    pub(crate) fn dispatch_task(
        &self,
        task: super::dispatch::DispatchTask,
    ) -> Result<(), HostError> {
        let job = Job {
            target: self.backend.target(),
            message: OwnedMessage {
                opcode: 0,
                flags: 0,
                correlation_id: 0,
                payload: Vec::new(),
            },
            task: Some(task),
            reply: None,
            in_flight: self.enter_in_flight(),
        };
        self.submit(job)
    }

    /// Entered before the job is submitted, so a job still sitting in the
    /// pool's queue (not yet picked up by a worker) still counts as in
    /// flight for `destroy` to wait on.
    fn enter_in_flight(&self) -> Option<super::dispatch::InFlightGuard> {
        match &self.dispatch {
            Dispatch::Concurrent(_) => Some(self.in_flight.enter()),
            Dispatch::Serial(_) => None,
        }
    }

    fn submit(&self, job: Job) -> Result<(), HostError> {
        match &self.dispatch {
            Dispatch::Serial(worker) => worker.submit(job),
            Dispatch::Concurrent(pool) => pool.submit(job),
        }
    }

    /// This instance's dispatch target, for a direct, non-queued call that
    /// bypasses `Dispatch::Serial`/`Dispatch::Concurrent` entirely
    /// (`docs/v2/plugin-abi.md` §4's one documented exception: a
    /// `VOSU_READ_PLAN_SYNC` result, delivered on the submitting thread
    /// itself rather than through this instance's own dispatch queue).
    /// The caller must not hold any lock the direct call could reentrantly
    /// need while making it (e.g. a plugin calling back into the host from
    /// inside this call).
    pub(crate) fn dispatch_target(&self) -> DispatchTarget {
        self.backend.target()
    }

    /// Stops accepting new dispatch work, waits for every already queued
    /// or running job to finish, then calls `destroy`, giving the whole
    /// sequence up to `grace` (§4). A local timeout leaves the plugin's own
    /// internal state running, and leaks `host_ctx` and the library
    /// reference rather than freeing them while a stuck plugin thread may
    /// still use them: the accepted cost of the in-process trust grant. A
    /// process timeout terminates the child instead.
    pub fn destroy(mut self, grace: Duration) -> Result<(), HostError> {
        let result = self.destroy_inner(grace);
        self.destroyed = true;
        result
    }

    /// Retires an instance whose process was reported lost: terminates the
    /// child first so every job blocked on it fails promptly, then drains
    /// dispatch within `grace`. The plugin's `destroy` never runs. A local
    /// instance cannot be lost and is destroyed normally.
    pub(crate) fn abandon(mut self, grace: Duration) {
        self.destroyed = true;
        let endpoint = match &self.backend {
            Backend::Process { endpoint, .. } => Arc::clone(endpoint),
            Backend::Local { .. } => {
                if let Err(error) = self.destroy_inner(grace) {
                    tracing::warn!(id = ?self.id, %error, "plugin did not destroy within its grace period");
                }
                return;
            }
        };
        let deadline = Instant::now() + grace;
        self.outbox.close();
        endpoint.terminate();
        if !self.drain_dispatch(deadline) {
            tracing::warn!(
                id = ?self.id,
                "a lost plugin instance's dispatch did not drain within its grace period"
            );
        }
    }

    fn drain_dispatch(&mut self, deadline: Instant) -> bool {
        match &mut self.dispatch {
            Dispatch::Serial(worker) => {
                worker.shutdown_within(deadline.saturating_duration_since(Instant::now()))
            }
            Dispatch::Concurrent(_) => self
                .in_flight
                .wait_until_zero(deadline.saturating_duration_since(Instant::now())),
        }
    }

    fn destroy_inner(&mut self, grace: Duration) -> Result<(), HostError> {
        let deadline = Instant::now() + grace;
        self.outbox.close();
        let drained = self.drain_dispatch(deadline);
        let (vtable, ctx) = match &self.backend {
            Backend::Process { endpoint, .. } => {
                if !drained {
                    // A queued delivery is still blocked on the child past
                    // the grace period: end the child, which also fails
                    // that delivery, rather than wait on it.
                    endpoint.terminate();
                    return Err(HostError::DestroyTimedOut);
                }
                return endpoint.destroy(deadline.saturating_duration_since(Instant::now()));
            }
            Backend::Local { vtable, ctx, .. } => (*vtable, *ctx),
        };
        if !drained {
            // A job for this instance is still queued or running past the
            // grace period: calling the plugin's own `destroy` now would
            // free state (and, for `--plugin <dir>`, unmap the library)
            // that job may still be touching.
            self.leak_on_timeout();
            return Err(HostError::DestroyTimedOut);
        }
        let (done_tx, done_rx) = sync_channel::<()>(1);
        let handle = std::thread::spawn(move || {
            if let Some(destroy) = vtable.destroy {
                // SAFETY: `ctx`'s pointer is this instance's live
                // `plugin_ctx`, never reused after this call returns (§4:
                // the host is tearing this instance down).
                unsafe { destroy(ctx.raw()) };
            }
            let _ = done_tx.send(());
        });
        match done_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(()) => {
                let _ = handle.join();
                Ok(())
            }
            Err(_) => {
                // The host proceeds without it (§4): detach the thread
                // rather than block on a plugin that may never return.
                drop(handle);
                self.leak_on_timeout();
                Err(HostError::DestroyTimedOut)
            }
        }
    }

    /// On a local `destroy` timeout, leaks `host_ctx` and the library
    /// reference instead of dropping them: the detached `destroy` thread
    /// (or a job this instance gave up waiting for) may still be running
    /// past the grace period and touch either through the ABI's `host`/
    /// `host_ctx` pointers or the library's own code (`include/
    /// vosu_plugin.h`'s lifetime contract; §4 already treats a stuck
    /// in-process plugin as an accepted cost, and a stuck plugin's thread
    /// dereferencing freed memory or calling into an unmapped library
    /// would turn that accepted cost into undefined behavior instead).
    fn leak_on_timeout(&mut self) {
        if let Backend::Local {
            host_ctx, library, ..
        } = &mut self.backend
        {
            if let Some(host_ctx) = host_ctx.take() {
                std::mem::forget(host_ctx);
            }
            if let Some(library) = library.take() {
                std::mem::forget(library);
            }
        }
    }
}

impl Drop for PluginInstance {
    fn drop(&mut self) {
        if self.destroyed {
            return;
        }
        self.destroyed = true;
        // Best-effort: an instance dropped without an explicit `destroy()`
        // call (an early `?` return before cleanup ran, a `Vec` drop, ...)
        // must still not leave dispatch running forever or free `host_ctx`
        // while the plugin may still touch it. There is no caller here to
        // pick a grace period.
        let _ = self.destroy_inner(DROP_DESTROY_GRACE);
    }
}
