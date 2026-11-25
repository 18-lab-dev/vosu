//! The plugin host's local `vosu_host_services`, its ring reader and its
//! dispatch worker (`docs/v2/plugin-abi.md` §7.2, §7.6 "Acknowledged
//! sends").
//!
//! **`send`.** Applies the in-process pre-handler checks locally (size and
//! null data, unknown or private opcode, unrecognized flags, SDK decode),
//! so a refused message writes nothing. A host → plugin opcode a plugin
//! sends has no effect in-process and returns `VOSU_OK` there, so it
//! returns `VOSU_OK` here without a write. `VOSU_OP_LOG_RECORD` is written
//! unacknowledged. Every other message reserves one of
//! [`MAX_WAITERS`] waiters, is written as `ACK_NEXT` plus the unmodified
//! frame, and blocks until core's `STATUS`, so the plugin sees core's own
//! status. A waiter past its 2-second backstop means core stopped serving
//! the channel: the process closes the channel and exits rather than
//! fabricate a status.
//!
//! **Interim until M5 T1/B0.** A `VOSU_READ_PLAN_SYNC` submission is
//! refused with `VOSU_ERR_UNSUPPORTED` (its `REPLY_NEXT` result path is
//! B0/T1 work); the per-instance local admission caps are left to core,
//! which enforces the same caps authoritatively; dispatch is one serial
//! worker even under `concurrent_dispatch`; and the delivery queue is a
//! bounded FIFO the ring reader blocks on when full, rather than T1's
//! coalescing queue. A full queue therefore stalls this ring until core's
//! 2-second backpressure window declares the channel lost, never an
//! unbounded queue or a dropped message.
//!
//! **Ring reader.** Only validates, copies into aligned storage and routes:
//! `STATUS` to its waiter, public frames to the dispatch queue, `DESTROY`
//! to the lifecycle. It never calls plugin code.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::mpsc::{Receiver as QueueReceiver, SyncSender, sync_channel};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vosu_plugin_api::{
    Message, VOSU_FRAME_MAX_BYTES, VOSU_PLUGIN_ABI_MAJOR, VOSU_READ_PLAN_SYNC, validate_flags,
    vosu_bytes, vosu_host_services, vosu_message, vosu_opcode, vosu_status,
};

use super::super::channel::{Closer, Receiver, Sender};
use super::super::control::{EXIT_CHANNEL_LOST, EXIT_TRANSPORT_TIMEOUT};
use super::super::frame::{FrameHeader, PRIVATE_OPCODE_BIT, control};
use super::super::ring::PayloadBuffer;
use crate::plugin::dispatch::PluginCtx;

/// Concurrent status waiters per instance (A5.2).
pub(super) const MAX_WAITERS: usize = 64;
/// A status waiter's backstop after its write completed (A5.2).
const STATUS_BACKSTOP: Duration = Duration::from_secs(2);
/// Public frames the reader may hold for the dispatch worker.
const DISPATCH_QUEUE: usize = 256;

/// The `vosu_host_services` every plugin host passes to `create`.
pub(super) static HOST_SERVICES: vosu_host_services = vosu_host_services {
    struct_size: core::mem::size_of::<vosu_host_services>() as u32,
    abi_version: VOSU_PLUGIN_ABI_MAJOR,
    send: Some(send_trampoline),
};

/// What the plugin's `host_ctx` points at: the relay to core. A plugin
/// host serves exactly one instance, so this is the process's one [`RELAY`],
/// alive until the process exits: plugin threads may call `send` until then.
struct Relay {
    sender: Sender,
    closer: Closer,
    waiters: Waiters,
}

static RELAY: OnceLock<Relay> = OnceLock::new();

#[derive(Default)]
struct Waiters {
    state: Mutex<WaiterState>,
    changed: Condvar,
}

#[derive(Default)]
struct WaiterState {
    /// The last request id handed out; ids start at 1 and only increase.
    last_id: u64,
    /// Reserved waiters, with core's status once it arrived.
    pending: HashMap<u64, Option<vosu_status>>,
    /// Replies for an id nobody waits on any more, discarded.
    discarded: u64,
    /// The channel ended: every waiter exits the process.
    closed: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Waiters {
    fn reserve(&self) -> Option<u64> {
        let mut state = lock(&self.state);
        if state.pending.len() >= MAX_WAITERS {
            return None;
        }
        state.last_id += 1;
        let id = state.last_id;
        state.pending.insert(id, None);
        Some(id)
    }

    fn release(&self, id: u64) {
        lock(&self.state).pending.remove(&id);
    }

    fn complete(&self, id: u64, status: vosu_status) {
        let mut state = lock(&self.state);
        match state.pending.get_mut(&id) {
            Some(slot @ None) => *slot = Some(status),
            _ => state.discarded += 1,
        }
        drop(state);
        self.changed.notify_all();
    }

    fn close(&self) {
        lock(&self.state).closed = true;
        self.changed.notify_all();
    }

    /// Waits for `id`'s status until `deadline`. `Err` carries the exit
    /// status when the channel ended or the backstop expired.
    fn wait(&self, id: u64, deadline: Instant) -> Result<vosu_status, i32> {
        let mut state = lock(&self.state);
        loop {
            if let Some(Some(status)) = state.pending.get(&id) {
                let status = *status;
                state.pending.remove(&id);
                return Ok(status);
            }
            if state.closed {
                return Err(EXIT_CHANNEL_LOST);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(EXIT_TRANSPORT_TIMEOUT);
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// Exits the plugin host after closing its channel, from whichever thread
/// found it unusable (§7.6 backstops, A5.8).
fn exit_closed(closer: &Closer, status: i32) -> ! {
    let _ = closer.close();
    std::process::exit(status)
}

unsafe extern "C" fn send_trampoline(host_ctx: *mut c_void, message: *const vosu_message) -> u32 {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if host_ctx.is_null() || message.is_null() {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        // SAFETY: `host_ctx` is the process's `RELAY`, which `create`
        // received and which lives until the process exits.
        let relay = unsafe { &*host_ctx.cast::<Relay>() };
        // SAFETY: the ABI borrows `message` for the duration of this call.
        let message = unsafe { &*message };
        if message.payload.len > VOSU_FRAME_MAX_BYTES {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let payload: &[u8] = if message.payload.len == 0 {
            &[]
        } else if message.payload.data.is_null() {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        } else {
            // SAFETY: the ABI requires `payload.data` to be valid for
            // `payload.len` bytes for the duration of this call.
            unsafe {
                std::slice::from_raw_parts(message.payload.data, message.payload.len as usize)
            }
        };
        relay.send(
            message.opcode,
            message.flags,
            message.correlation_id,
            payload,
        )
    }));
    outcome.unwrap_or(vosu_status::VOSU_ERR_BACKEND) as u32
}

/// Whether a plugin → host message of this opcode needs core's status.
enum Route {
    /// Has no effect in-process (`VOSU_OK`); nothing is written.
    NoEffect,
    /// `VOSU_OP_LOG_RECORD`: written unacknowledged, `VOSU_OK`.
    Unacknowledged,
    Acknowledged,
}

fn route(opcode: vosu_opcode) -> Route {
    use vosu_opcode::*;
    match opcode {
        VOSU_OP_LOG_RECORD => Route::Unacknowledged,
        VOSU_OP_PUBLISH_FIELD
        | VOSU_OP_READ_PLAN_SUBMIT
        | VOSU_OP_READ_PLAN_CANCEL
        | VOSU_OP_PUBLISH_BATCH
        | VOSU_OP_SESSION_IDENTIFY
        | VOSU_OP_SESSION_DECLINE
        | VOSU_OP_SCAN_REQUEST
        | VOSU_OP_RESOURCE_RESOLVED
        | VOSU_OP_STREAM_CHUNK
        | VOSU_OP_ROOT_DECLARE
        | VOSU_OP_ACTION_CALL
        | VOSU_OP_ACTION_RESULT
        | VOSU_OP_ACTION_CANCEL
        | VOSU_OP_ACTION_PROGRESS
        | VOSU_OP_TIMER_SET
        | VOSU_OP_TIMER_CANCEL
        | VOSU_OP_STATE_SUBSCRIBE => Route::Acknowledged,
        VOSU_OP_DEMAND_CHANGED
        | VOSU_OP_READ_PLAN_RESULT
        | VOSU_OP_SESSION_ATTACHED
        | VOSU_OP_SESSION_DETACHED
        | VOSU_OP_FOCUS_CHANGED
        | VOSU_OP_SCAN_RESULT
        | VOSU_OP_RESOURCE_RESOLVE
        | VOSU_OP_TREE_LIST
        | VOSU_OP_TREE_LOOKUP
        | VOSU_OP_STREAM_READ
        | VOSU_OP_STREAM_CLOSE
        | VOSU_OP_ACTION_INVOKE
        | VOSU_OP_SETTINGS_CHANGED
        | VOSU_OP_TIMER_FIRED
        | VOSU_OP_STATE_UPDATE => Route::NoEffect,
    }
}

impl Relay {
    fn send(&self, opcode: u32, flags: u32, correlation: u64, payload: &[u8]) -> vosu_status {
        if opcode & PRIVATE_OPCODE_BIT != 0 {
            return vosu_status::VOSU_ERR_UNSUPPORTED;
        }
        let Ok(known) = vosu_opcode::try_from(opcode) else {
            return vosu_status::VOSU_ERR_UNSUPPORTED;
        };
        if validate_flags(known, flags).is_err() {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        if Message::decode(known, flags, payload).is_err() {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        let header = FrameHeader {
            opcode,
            flags,
            correlation,
        };
        match route(known) {
            Route::NoEffect => vosu_status::VOSU_OK,
            Route::Unacknowledged => {
                if self.sender.send(&header, payload).is_err() {
                    exit_closed(&self.closer, EXIT_CHANNEL_LOST);
                }
                vosu_status::VOSU_OK
            }
            Route::Acknowledged => {
                if known == vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT
                    && flags & VOSU_READ_PLAN_SYNC != 0
                {
                    return vosu_status::VOSU_ERR_UNSUPPORTED;
                }
                let Some(id) = self.waiters.reserve() else {
                    return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
                };
                if self.sender.send_acknowledged(id, &header, payload).is_err() {
                    self.waiters.release(id);
                    exit_closed(&self.closer, EXIT_CHANNEL_LOST);
                }
                let deadline = Instant::now() + STATUS_BACKSTOP;
                match self.waiters.wait(id, deadline) {
                    Ok(status) => status,
                    Err(exit) => {
                        self.waiters.release(id);
                        exit_closed(&self.closer, exit)
                    }
                }
            }
        }
    }
}

/// One public host → plugin frame, its payload copied into 8-byte-aligned
/// storage for `dispatch`.
struct QueuedFrame {
    header: FrameHeader,
    words: Vec<u64>,
    len: usize,
}

impl QueuedFrame {
    fn copy(header: FrameHeader, payload: &[u8]) -> Self {
        let mut words = vec![0_u64; payload.len().div_ceil(8)];
        for (word, chunk) in words.iter_mut().zip(payload.chunks(8)) {
            let mut bytes = [0_u8; 8];
            bytes[..chunk.len()].copy_from_slice(chunk);
            *word = u64::from_ne_bytes(bytes);
        }
        Self {
            header,
            words,
            len: payload.len(),
        }
    }
}

/// Why the lifecycle wait ended.
enum Event {
    Destroy,
    Lost,
}

/// The ring reader and lifecycle wiring before `create` returned.
pub(super) struct Runtime {
    relay: &'static Relay,
    reader: JoinHandle<()>,
    events: QueueReceiver<Event>,
    queue: QueueReceiver<QueuedFrame>,
}

/// After `READY`: the reader plus the running dispatch worker.
pub(super) struct Running {
    relay: &'static Relay,
    reader: JoinHandle<()>,
    events: QueueReceiver<Event>,
    worker: JoinHandle<()>,
}

/// After `DESTROY`: the worker is joined and only the reader remains.
pub(super) struct Destroying {
    relay: &'static Relay,
    reader: JoinHandle<()>,
}

pub(super) enum Outcome {
    Destroy(Destroying),
    Lost,
}

impl Runtime {
    /// Starts the ring reader. `create` may already `send`, and its status
    /// replies need the reader.
    pub(super) fn start(sender: Sender, closer: Closer, receiver: Receiver) -> Self {
        if RELAY
            .set(Relay {
                sender,
                closer,
                waiters: Waiters::default(),
            })
            .is_err()
        {
            // One instance per plugin host: there is no second start.
            std::process::exit(EXIT_CHANNEL_LOST);
        }
        let relay: &'static Relay = RELAY.get().expect("set just above");
        let (events_tx, events) = sync_channel(2);
        let (queue_tx, queue) = sync_channel(DISPATCH_QUEUE);
        let reader = std::thread::Builder::new()
            .name("vosu-plugin-host-reader".to_owned())
            .spawn(move || read_ring(relay, receiver, queue_tx, events_tx))
            .unwrap_or_else(|_| exit_closed(&relay.closer, EXIT_CHANNEL_LOST));
        Self {
            relay,
            reader,
            events,
            queue,
        }
    }

    /// The `host_ctx` for `create`.
    pub(super) fn host_ctx(&self) -> *mut c_void {
        (self.relay as *const Relay).cast_mut().cast()
    }

    /// Starts the one dispatch worker for `ctx`.
    pub(super) fn start_dispatch(
        self,
        ctx: PluginCtx,
        dispatch: unsafe extern "C" fn(*mut c_void, *const vosu_message) -> u32,
    ) -> Running {
        let queue = self.queue;
        let relay = self.relay;
        let worker = std::thread::Builder::new()
            .name("vosu-plugin-host-dispatch".to_owned())
            .spawn(move || {
                for frame in queue {
                    let message = vosu_message {
                        opcode: frame.header.opcode,
                        flags: frame.header.flags,
                        correlation_id: frame.header.correlation,
                        payload: vosu_bytes {
                            data: frame.words.as_ptr().cast(),
                            len: frame.len as u32,
                        },
                    };
                    // SAFETY: `ctx` is the live `plugin_ctx` `create`
                    // returned; `destroy` runs only after this worker is
                    // joined. `message` borrows `frame`, alive for the
                    // call. Core ignores dispatch statuses (§7.6).
                    unsafe { dispatch(ctx.raw(), &message) };
                }
            })
            .unwrap_or_else(|_| exit_closed(&relay.closer, EXIT_CHANNEL_LOST));
        Running {
            relay: self.relay,
            reader: self.reader,
            events: self.events,
            worker,
        }
    }
}

impl Running {
    /// Waits for `DESTROY` (then drains and joins the dispatch worker) or
    /// for the channel to end.
    pub(super) fn wait(self) -> Outcome {
        match self.events.recv() {
            Ok(Event::Destroy) => {
                // The reader dropped the queue's sender on DESTROY, so the
                // worker ends after the frames already queued.
                let _ = self.worker.join();
                Outcome::Destroy(Destroying {
                    relay: self.relay,
                    reader: self.reader,
                })
            }
            Ok(Event::Lost) | Err(_) => Outcome::Lost,
        }
    }
}

impl Destroying {
    /// Ends the channel after `DESTROYED` and joins the reader.
    pub(super) fn finish(self) {
        let _ = self.relay.closer.close();
        let _ = self.reader.join();
    }
}

fn read_ring(
    relay: &'static Relay,
    mut receiver: Receiver,
    queue: SyncSender<QueuedFrame>,
    events: SyncSender<Event>,
) {
    let mut buffer = PayloadBuffer::new();
    let mut queue = Some(queue);
    let lost = loop {
        let header = match receiver.recv(&mut buffer) {
            Ok(header) => header,
            Err(_) => break true,
        };
        match header.opcode {
            control::STATUS => {
                let bytes = buffer.bytes();
                let raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                let Ok(status) = vosu_status::try_from(raw) else {
                    break true;
                };
                relay.waiters.complete(header.correlation, status);
            }
            control::DESTROY => {
                if queue.take().is_none() {
                    // DESTROY twice.
                    break true;
                }
                let _ = events.send(Event::Destroy);
            }
            // A second CREATE, or a SYNC reply this host never requested.
            _ if header.is_private() => break true,
            _ => {
                let Some(queue) = &queue else {
                    // Core sends nothing after DESTROY.
                    break true;
                };
                if queue
                    .send(QueuedFrame::copy(header, buffer.bytes()))
                    .is_err()
                {
                    break true;
                }
            }
        }
    };
    relay.waiters.close();
    if lost {
        let _ = relay.closer.close();
        let _ = events.send(Event::Lost);
    }
}
