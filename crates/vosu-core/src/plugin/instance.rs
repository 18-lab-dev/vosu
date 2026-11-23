//! Plugin instance lifecycle: `create`, dispatch (one dedicated thread per
//! instance, or the shared `concurrent_dispatch` pool), and `destroy` with
//! a grace period (`docs/v2/plugin-abi.md` §4).

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

use libloading::Library;
use vosu_plugin_api::{vosu_bytes, vosu_plugin_vtable, vosu_status};

use super::dispatch::{InFlightTracker, Job, OwnedMessage, PluginCtx, SerialWorker, WorkerPool};
use super::error::HostError;
use super::host_services::{self, HostContext, PluginHostHandler, PluginInstanceId};
use super::registry::LoadedPlugin;

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

/// A created, running plugin instance.
pub struct PluginInstance {
    id: PluginInstanceId,
    vtable: vosu_plugin_vtable,
    ctx: PluginCtx,
    // `Some` until `destroy_inner` either runs the plugin's own `destroy`
    // to completion or times out: the ABI documents `host`/`host_ctx` as
    // outliving every call `create` can make through them for the
    // instance's whole lifetime, so a timed-out teardown leaks this
    // (`destroy_inner`'s `leak_on_timeout`) instead of freeing it while a
    // stuck plugin thread may still read it.
    host_ctx: Option<Box<HostContext>>,
    // Co-owned, not just held: unloading the library while this
    // instance's `plugin_ctx` still points into it would be immediate
    // undefined behavior the next `dispatch`/`destroy` call makes. `None`
    // for a bundled plugin, which is linked into this executable rather
    // than `dlopen`ed. Leaked alongside `host_ctx` on a `destroy` timeout
    // for the same reason.
    library: Option<Arc<Library>>,
    in_flight: InFlightTracker,
    dispatch: Dispatch,
    /// Set once `destroy_inner` has run, whether from an explicit
    /// `destroy()` call or from `Drop`, so `Drop` never runs it twice.
    destroyed: bool,
}

impl PluginInstance {
    /// Calls `create`, then starts this instance's dedicated dispatch
    /// thread, or registers it with `concurrent_pool` when the manifest
    /// sets `concurrent_dispatch = true`.
    pub fn create(
        id: PluginInstanceId,
        plugin: &LoadedPlugin,
        init: &[u8],
        handler: Arc<dyn PluginHostHandler>,
        concurrent_pool: Option<Arc<WorkerPool>>,
    ) -> Result<Self, HostError> {
        let vtable = plugin.vtable();
        let create = vtable.create.ok_or(HostError::MissingRequiredFunction)?;
        // Resolved and validated before the plugin's own `create` runs: a
        // `concurrent_dispatch` plugin with no pool must never reach
        // `create` at all. Checking this after `create` already succeeded
        // would leak the `plugin_ctx` it just returned and free `host_ctx`
        // out from under a plugin that may still be holding it
        // (`include/vosu_plugin.h`'s `host`/`host_ctx` lifetime contract).
        let concurrent_pool = if plugin.manifest().concurrent_dispatch {
            Some(concurrent_pool.ok_or(HostError::ConcurrentPoolMissing)?)
        } else {
            None
        };
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
        let ctx = PluginCtx(plugin_ctx);
        let dispatch = match concurrent_pool {
            Some(pool) => Dispatch::Concurrent(pool),
            None => Dispatch::Serial(SerialWorker::spawn(
                format!("vosu-plugin-{}", id.0),
                DEFAULT_QUEUE_CAPACITY,
            )),
        };
        Ok(Self {
            id,
            vtable,
            ctx,
            host_ctx: Some(host_ctx),
            library: plugin.library_handle(),
            in_flight: InFlightTracker::new(),
            dispatch,
            destroyed: false,
        })
    }

    pub fn id(&self) -> PluginInstanceId {
        self.id
    }

    /// Enqueues one message for this instance's dispatch thread (serial)
    /// or the shared pool (`concurrent_dispatch`). The returned receiver
    /// yields the plugin's `dispatch` status once it runs; drop it if the
    /// caller does not need to observe completion.
    pub fn dispatch(&self, message: OwnedMessage) -> Result<Receiver<vosu_status>, HostError> {
        let (reply_tx, reply_rx) = sync_channel(1);
        // Entered before the job is submitted, so a job still sitting in
        // the pool's queue (not yet picked up by a worker) still counts as
        // in flight for `destroy` to wait on.
        let in_flight = match &self.dispatch {
            Dispatch::Concurrent(_) => Some(self.in_flight.enter()),
            Dispatch::Serial(_) => None,
        };
        let job = Job {
            vtable: self.vtable,
            ctx: self.ctx,
            message,
            reply: Some(reply_tx),
            in_flight,
        };
        match &self.dispatch {
            Dispatch::Serial(worker) => worker.submit(job)?,
            Dispatch::Concurrent(pool) => pool.submit(job)?,
        }
        Ok(reply_rx)
    }

    /// This instance's raw vtable/ctx pair, for a direct, non-queued call
    /// that bypasses `Dispatch::Serial`/`Dispatch::Concurrent` entirely
    /// (`docs/v2/plugin-abi.md` §4's one documented exception: a
    /// `VOSU_READ_PLAN_SYNC` result, delivered on the submitting thread
    /// itself rather than through this instance's own dispatch queue).
    /// Both fields are `Copy`; the caller must not hold any lock the
    /// direct call could reentrantly need while making it (e.g. a plugin
    /// calling back into the host from inside this call).
    pub(crate) fn ctx_and_vtable(&self) -> (vosu_plugin_vtable, PluginCtx) {
        (self.vtable, self.ctx)
    }

    /// Stops accepting new dispatch work, waits for every already queued
    /// or running job to finish, then calls `destroy`, giving the whole
    /// sequence up to `grace` (§4). A timeout leaves the plugin's own
    /// internal state running, and leaks `host_ctx` and the library
    /// reference rather than freeing them while a stuck plugin thread may
    /// still use them: the accepted cost of the in-process trust grant.
    pub fn destroy(mut self, grace: Duration) -> Result<(), HostError> {
        let result = self.destroy_inner(grace);
        self.destroyed = true;
        result
    }

    fn destroy_inner(&mut self, grace: Duration) -> Result<(), HostError> {
        let deadline = Instant::now() + grace;
        let drained = match &mut self.dispatch {
            Dispatch::Serial(worker) => {
                worker.shutdown_within(deadline.saturating_duration_since(Instant::now()))
            }
            Dispatch::Concurrent(_) => self
                .in_flight
                .wait_until_zero(deadline.saturating_duration_since(Instant::now())),
        };
        if !drained {
            // A job for this instance is still queued or running past the
            // grace period: calling the plugin's own `destroy` now would
            // free state (and, for `--plugin <dir>`, unmap the library)
            // that job may still be touching.
            self.leak_on_timeout();
            return Err(HostError::DestroyTimedOut);
        }
        let vtable = self.vtable;
        let ctx = self.ctx;
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

    /// On a `destroy` timeout, leaks `host_ctx` and the library reference
    /// instead of dropping them: the detached `destroy` thread (or a
    /// job this instance gave up waiting for) may still be running past
    /// the grace period and touch either through the ABI's `host`/
    /// `host_ctx` pointers or the library's own code (`include/
    /// vosu_plugin.h`'s lifetime contract; §4 already treats a stuck
    /// in-process plugin as an accepted cost, and a stuck plugin's thread
    /// dereferencing freed memory or calling into an unmapped library
    /// would turn that accepted cost into undefined behavior instead).
    fn leak_on_timeout(&mut self) {
        if let Some(host_ctx) = self.host_ctx.take() {
            std::mem::forget(host_ctx);
        }
        if let Some(library) = self.library.take() {
            std::mem::forget(library);
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
