//! Plugin instance lifecycle: `create`, dispatch (one dedicated thread per
//! instance, or the shared `concurrent_dispatch` pool), and `destroy` with
//! a grace period (`docs/v2/plugin-abi.md` §4).

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::Duration;

use vosu_plugin_api::{vosu_bytes, vosu_plugin_vtable, vosu_status};

use super::dispatch::{Job, OwnedMessage, PluginCtx, SerialWorker, WorkerPool};
use super::error::HostError;
use super::host_services::{
    HostContext, PluginHostHandler, PluginInstanceId, host_services_vtable,
};
use super::registry::LoadedPlugin;

/// Bound for a serial instance's own dispatch queue (`AGENTS.md`: no
/// unbounded channels); the shared `concurrent_dispatch` pool's queue is
/// bounded separately by whoever builds its `WorkerPool`.
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;

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
    // Kept alive until `destroy` runs: the ABI documents `host`/`host_ctx`
    // as outliving every call `create` can make through them for the
    // instance's whole lifetime.
    #[expect(
        dead_code,
        reason = "keeps the boxed HostContext's address valid for `create`'s host_ctx lifetime contract until destroy() drops it; never read directly"
    )]
    host_ctx: Box<HostContext>,
    dispatch: Dispatch,
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
        let mut host_ctx = Box::new(HostContext::new(id, handler));
        let host_vtable = host_services_vtable();
        let init_bytes = vosu_bytes {
            data: init.as_ptr(),
            len: init.len() as u32,
        };
        // SAFETY: `host_vtable` is a local value borrowed for this call;
        // `host_ctx`'s address is borrowed for the instance's whole
        // lifetime, the ABI's own contract on `create`'s `host_ctx`, which
        // this instance keeps boxed and alive until `destroy`; `init_bytes`
        // points at `init`, alive for the call.
        let plugin_ctx = unsafe {
            create(
                &host_vtable as *const _,
                (host_ctx.as_mut() as *mut HostContext).cast::<c_void>(),
                init_bytes,
            )
        };
        if plugin_ctx.is_null() {
            return Err(HostError::CreateRejected);
        }
        let ctx = PluginCtx(plugin_ctx);
        let dispatch = if plugin.manifest().concurrent_dispatch {
            Dispatch::Concurrent(concurrent_pool.ok_or(HostError::ConcurrentPoolMissing)?)
        } else {
            Dispatch::Serial(SerialWorker::spawn(
                format!("vosu-plugin-{}", id.0),
                DEFAULT_QUEUE_CAPACITY,
            ))
        };
        Ok(Self {
            id,
            vtable,
            ctx,
            host_ctx,
            dispatch,
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
        let job = Job {
            vtable: self.vtable,
            ctx: self.ctx,
            message,
            reply: Some(reply_tx),
        };
        match &self.dispatch {
            Dispatch::Serial(worker) => worker.submit(job)?,
            Dispatch::Concurrent(pool) => pool.submit(job)?,
        }
        Ok(reply_rx)
    }

    /// Stops accepting new dispatch work, then calls `destroy`, giving it
    /// up to `grace` to return (§4). A timeout leaves the plugin's own
    /// internal state running: the accepted cost of the in-process trust
    /// grant.
    pub fn destroy(mut self, grace: Duration) -> Result<(), HostError> {
        if let Dispatch::Serial(worker) = &mut self.dispatch {
            worker.shutdown();
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
        match done_rx.recv_timeout(grace) {
            Ok(()) => {
                let _ = handle.join();
                Ok(())
            }
            Err(_) => {
                // The host proceeds without it (§4): detach the thread
                // rather than block on a plugin that may never return.
                drop(handle);
                Err(HostError::DestroyTimedOut)
            }
        }
    }
}
