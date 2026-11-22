//! Dispatch primitives: the owned, channel-safe message job, one
//! instance's dedicated serial worker thread, and the shared
//! `concurrent_dispatch` worker pool (`docs/v2/plugin-abi.md` §4).

use std::ffi::c_void;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
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

/// Counts the jobs queued or running for one instance's shared
/// `concurrent_dispatch` pool, so `PluginInstance::destroy` can wait for
/// them to finish before calling the plugin's own `destroy`
/// (`docs/v2/plugin-abi.md` §4): otherwise a pool job still holding this
/// instance's vtable and `ctx` races the plugin's own teardown, which
/// frees them. A serial instance does not need this: its own queue is
/// fully drained by `SerialWorker::shutdown_within` instead.
#[derive(Clone)]
pub(crate) struct InFlightTracker(Arc<(Mutex<usize>, Condvar)>);

impl InFlightTracker {
    pub(crate) fn new() -> Self {
        Self(Arc::new((Mutex::new(0), Condvar::new())))
    }

    /// Marks one job in flight for the returned guard's whole lifetime.
    /// Call this before the job is submitted, so a job still sitting in
    /// the queue (not yet picked up by a pool worker) counts too.
    pub(crate) fn enter(&self) -> InFlightGuard {
        let mut count = self.0.0.lock().unwrap_or_else(PoisonError::into_inner);
        *count += 1;
        InFlightGuard(Arc::clone(&self.0))
    }

    /// Waits up to `timeout` for every entered guard to have dropped.
    /// Returns `false` once `timeout` elapses with jobs still in flight,
    /// so a hung job cannot block `destroy` past its grace period.
    pub(crate) fn wait_until_zero(&self, timeout: std::time::Duration) -> bool {
        let (lock, condvar) = &*self.0;
        let count = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let (_count_guard, result) = condvar
            .wait_timeout_while(count, timeout, |count| *count != 0)
            .unwrap_or_else(PoisonError::into_inner);
        !result.timed_out()
    }
}

/// Decrements its [`InFlightTracker`] when dropped: held by a [`Job`] for
/// as long as the job is queued or running, including a job dropped unrun
/// (e.g. rejected by a full queue), so the count always matches reality.
pub(crate) struct InFlightGuard(Arc<(Mutex<usize>, Condvar)>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let (lock, condvar) = &*self.0;
        let mut count = lock.lock().unwrap_or_else(PoisonError::into_inner);
        *count -= 1;
        if *count == 0 {
            condvar.notify_all();
        }
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
    /// Held for this job's whole lifetime (queued and running); `None`
    /// for a serial job, whose queue `SerialWorker::shutdown_within`
    /// drains directly instead.
    #[expect(
        dead_code,
        reason = "never read directly; only its Drop (decrementing the in-flight count once this job finishes or is dropped unrun) matters"
    )]
    pub in_flight: Option<InFlightGuard>,
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
    /// Signaled once the worker thread's `recv` loop exits (the queue is
    /// closed and every already-queued job has run), so
    /// `shutdown_within` can bound how long it waits for that without a
    /// timed `JoinHandle::join`, which `std::thread` has no API for.
    done_rx: Receiver<()>,
}

impl SerialWorker {
    pub(crate) fn spawn(name: String, queue_capacity: usize) -> Self {
        let (tx, rx) = sync_channel::<Job>(queue_capacity.max(1));
        let (done_tx, done_rx) = sync_channel::<()>(1);
        let worker = std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    run_job(job);
                }
                let _ = done_tx.send(());
            })
            .expect("spawning this instance's dispatch thread");
        Self {
            tx: Some(tx),
            worker: Some(worker),
            done_rx,
        }
    }

    pub(crate) fn submit(&self, job: Job) -> Result<(), HostError> {
        self.tx
            .as_ref()
            .ok_or(HostError::DispatchQueueFull)?
            .try_send(job)
            .map_err(|_| HostError::DispatchQueueFull)
    }

    /// Closes the queue, then waits up to `timeout` for every
    /// already-queued job to finish and the worker thread to exit (§4's
    /// grace period bounds this drain too, not just the plugin's own
    /// `destroy` call: one hung job in this instance's queue must not
    /// delay another instance's shutdown past its own grace period).
    /// Returns `false`, detaching the thread without joining it, if that
    /// budget runs out before the thread signals it drained.
    pub(crate) fn shutdown_within(&mut self, timeout: std::time::Duration) -> bool {
        self.tx.take();
        match self.done_rx.recv_timeout(timeout) {
            Ok(()) => {
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
                true
            }
            Err(_) => {
                self.worker.take();
                false
            }
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
