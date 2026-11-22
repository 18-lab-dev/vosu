//! Dispatch primitives: the owned, channel-safe message job, one
//! instance's dedicated serial worker thread, and the shared
//! `concurrent_dispatch` worker pool (`docs/v2/plugin-abi.md` §4).

use std::ffi::c_void;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use vosu_plugin_api::{vosu_bytes, vosu_message, vosu_plugin_vtable, vosu_status};

use super::error::HostError;

/// A raw plugin pointer, valid to call from any thread for the instance's
/// whole lifetime (`docs/v2/plugin-abi.md` §4's `concurrent_dispatch`
/// depends on exactly this contract, which every conforming plugin must
/// honor).
#[derive(Clone, Copy)]
pub(crate) struct PluginCtx(pub(crate) *mut c_void);

// SAFETY: see the type's doc comment; the ABI documents `plugin_ctx` as
// callable from any thread for the instance's whole lifetime.
unsafe impl Send for PluginCtx {}
// SAFETY: see above; a `concurrent_dispatch` plugin's own `dispatch` must
// already be internally synchronized against concurrent callers, which is
// exactly what sharing this across worker-pool threads requires.
unsafe impl Sync for PluginCtx {}

impl PluginCtx {
    /// Takes the raw pointer out of this wrapper. A closure that captures
    /// `ctx.0` directly would capture only that field (Rust's disjoint
    /// closure captures), losing this type's `Send`/`Sync` impls; calling
    /// this method instead forces the whole `PluginCtx` to be captured.
    pub(crate) fn raw(self) -> *mut c_void {
        self.0
    }
}

/// One encoded `vosu_message`, owned so it can cross a channel: the raw
/// ABI pointer inside `vosu_message` is only valid for the duration of one
/// call, never across a channel hop to another thread.
pub struct OwnedMessage {
    pub opcode: u32,
    pub flags: u32,
    pub correlation_id: u64,
    pub payload: Vec<u8>,
}

impl OwnedMessage {
    /// Encodes a typed [`vosu_plugin_api::Message`] into the owned,
    /// channel-safe form `PluginInstance::dispatch` sends to a plugin.
    pub fn from_message(
        message: &vosu_plugin_api::Message,
        correlation_id: u64,
    ) -> Result<Self, vosu_plugin_api::MessageError> {
        Ok(Self {
            opcode: message.opcode() as u32,
            flags: message.flags(),
            correlation_id,
            payload: message.encode_payload()?,
        })
    }
}

impl OwnedMessage {
    pub(crate) fn call(&self, vtable: &vosu_plugin_vtable, ctx: PluginCtx) -> vosu_status {
        let message = vosu_message {
            opcode: self.opcode,
            flags: self.flags,
            correlation_id: self.correlation_id,
            payload: vosu_bytes {
                data: self.payload.as_ptr(),
                len: self.payload.len() as u32,
            },
        };
        let dispatch = vtable
            .dispatch
            .expect("the registry's check_descriptor verifies dispatch is Some before load");
        // SAFETY: `ctx.0` is this instance's live `plugin_ctx`; `message`'s
        // `payload` points at `self.payload`, which outlives this call.
        let raw = unsafe { dispatch(ctx.0, &message as *const vosu_message) };
        vosu_status::try_from(raw).unwrap_or(vosu_status::VOSU_ERR_BACKEND)
    }
}

/// One dispatch call plus everything needed to run it and report its
/// status back to whoever is waiting on it.
pub(crate) struct Job {
    pub vtable: vosu_plugin_vtable,
    pub ctx: PluginCtx,
    pub message: OwnedMessage,
    pub reply: Option<SyncSender<vosu_status>>,
}

fn run_job(job: Job) {
    let status = job.message.call(&job.vtable, job.ctx);
    if let Some(reply) = job.reply {
        let _ = reply.send(status);
    }
}

/// One instance's dedicated dispatch thread (default, §4): serializes
/// every call onto one thread, matching the existing single-worker-per-
/// instance model.
pub(crate) struct SerialWorker {
    tx: Option<SyncSender<Job>>,
    worker: Option<JoinHandle<()>>,
}

impl SerialWorker {
    pub(crate) fn spawn(name: String, queue_capacity: usize) -> Self {
        let (tx, rx) = sync_channel::<Job>(queue_capacity.max(1));
        let worker = std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    run_job(job);
                }
            })
            .expect("spawning this instance's dispatch thread");
        Self {
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    pub(crate) fn submit(&self, job: Job) -> Result<(), HostError> {
        self.tx
            .as_ref()
            .ok_or(HostError::DispatchQueueFull)?
            .try_send(job)
            .map_err(|_| HostError::DispatchQueueFull)
    }

    /// Closes the queue and joins the thread: every already-queued job
    /// still runs to completion, no new one is accepted.
    pub(crate) fn shutdown(&mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Shared worker pool for every `concurrent_dispatch = true` instance
/// (`docs/v2/plugin-abi.md` §4): the host dispatches from its own pool
/// instead of one dedicated thread per instance. Its queue is bounded by
/// `queue_capacity` (`AGENTS.md`: no unbounded channels).
pub struct WorkerPool {
    tx: Mutex<Option<SyncSender<Job>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl WorkerPool {
    pub fn new(worker_count: usize, queue_capacity: usize) -> Arc<Self> {
        let (tx, rx) = sync_channel::<Job>(queue_capacity.max(1));
        let rx = Arc::new(Mutex::new(rx));
        let mut workers = Vec::with_capacity(worker_count.max(1));
        for _ in 0..worker_count.max(1) {
            let rx = Arc::clone(&rx);
            workers.push(std::thread::spawn(move || {
                loop {
                    let job = {
                        let rx = rx.lock().unwrap_or_else(PoisonError::into_inner);
                        rx.recv()
                    };
                    match job {
                        Ok(job) => run_job(job),
                        Err(_) => break,
                    }
                }
            }));
        }
        Arc::new(Self {
            tx: Mutex::new(Some(tx)),
            workers: Mutex::new(workers),
        })
    }

    pub(crate) fn submit(&self, job: Job) -> Result<(), HostError> {
        let guard = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = guard.as_ref().ok_or(HostError::DispatchQueueFull)?;
        tx.try_send(job).map_err(|_| HostError::DispatchQueueFull)
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let workers =
            std::mem::take(&mut *self.workers.lock().unwrap_or_else(PoisonError::into_inner));
        for worker in workers {
            let _ = worker.join();
        }
    }
}
