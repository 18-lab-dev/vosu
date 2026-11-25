//! The plugin host's local `vosu_host_services` and the threads of its
//! one instance (`docs/v2/plugin-abi.md` §7.2, §7.6): the `send`
//! trampoline into [`Relay`], the ring reader started before `create` (its
//! status replies serve `send` calls `create` makes), and the dispatch
//! workers started after it.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::mpsc::{Receiver as EventReceiver, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use vosu_plugin_api::{
    VOSU_FRAME_MAX_BYTES, VOSU_PLUGIN_ABI_MAJOR, vosu_host_services, vosu_message, vosu_status,
};

use super::super::channel::Endpoint;
use super::super::control::EXIT_CHANNEL_LOST;
use super::super::relay::{CONCURRENT_WORKERS, Relay};
use super::super::waiters::Waiters;
use crate::plugin::execution::DispatchTarget;

/// The `vosu_host_services` every plugin host passes to `create`.
pub(super) static HOST_SERVICES: vosu_host_services = vosu_host_services {
    struct_size: core::mem::size_of::<vosu_host_services>() as u32,
    abi_version: VOSU_PLUGIN_ABI_MAJOR,
    send: Some(send_trampoline),
};

unsafe extern "C" fn send_trampoline(host_ctx: *mut c_void, message: *const vosu_message) -> u32 {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if host_ctx.is_null() || message.is_null() {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        // SAFETY: `host_ctx` is this process's relay, which `create`
        // received and which `Runtime::start` keeps alive until the process
        // exits.
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

/// Exits the plugin host: the relay already closed its channel.
fn exit_process(code: i32) -> vosu_status {
    std::process::exit(code)
}

/// Why the lifecycle wait ended.
enum Event {
    Destroy,
    Lost,
}

/// The ring reader before `create` returned.
pub(super) struct Runtime {
    relay: Arc<Relay>,
    reader: JoinHandle<()>,
    events: EventReceiver<Event>,
}

/// After `READY`: the reader plus the running dispatch workers.
pub(super) struct Running {
    relay: Arc<Relay>,
    reader: JoinHandle<()>,
    events: EventReceiver<Event>,
    workers: Vec<JoinHandle<()>>,
}

/// After `DESTROY`: the workers are joined and only the reader remains.
pub(super) struct Destroying {
    relay: Arc<Relay>,
    reader: JoinHandle<()>,
}

pub(super) enum Outcome {
    Destroy(Destroying),
    Lost,
}

impl Runtime {
    /// Starts the ring reader for `endpoint`. `sync_budget` is `CREATE`'s
    /// `sync_budget_ms`.
    pub(super) fn start(endpoint: Endpoint, sync_budget: Duration) -> Self {
        let Endpoint {
            sender,
            receiver,
            closer,
            ledger,
        } = endpoint;
        let relay = Arc::new(Relay::new(
            sender,
            closer,
            Waiters::new(ledger),
            sync_budget,
            Box::new(exit_process),
        ));
        // Plugin threads may call `send` until the process exits.
        std::mem::forget(Arc::clone(&relay));
        let (events_tx, events) = sync_channel(2);
        let reader = std::thread::Builder::new()
            .name("vosu-plugin-host-reader".to_owned())
            .spawn({
                let relay = Arc::clone(&relay);
                move || {
                    let destroy = events_tx.clone();
                    let lost = relay.read(receiver, move || {
                        let _ = destroy.send(Event::Destroy);
                    });
                    if lost {
                        let _ = events_tx.send(Event::Lost);
                    }
                }
            })
            .unwrap_or_else(|_| {
                let _ = relay.closer().close();
                std::process::exit(EXIT_CHANNEL_LOST)
            });
        Self {
            relay,
            reader,
            events,
        }
    }

    /// The `host_ctx` for `create`.
    pub(super) fn host_ctx(&self) -> *mut c_void {
        Arc::as_ptr(&self.relay).cast_mut().cast()
    }

    /// Starts the dispatch workers for `target`: one, or
    /// [`CONCURRENT_WORKERS`] under `concurrent_dispatch`.
    pub(super) fn start_dispatch(self, target: DispatchTarget, concurrent: bool) -> Running {
        self.relay.set_target(target);
        let count = if concurrent { CONCURRENT_WORKERS } else { 1 };
        let workers = (0..count)
            .map(|index| {
                let relay = Arc::clone(&self.relay);
                std::thread::Builder::new()
                    .name(format!("vosu-plugin-host-dispatch-{index}"))
                    .spawn(move || relay.work())
                    .unwrap_or_else(|_| {
                        let _ = self.relay.closer().close();
                        std::process::exit(EXIT_CHANNEL_LOST)
                    })
            })
            .collect();
        Running {
            relay: self.relay,
            reader: self.reader,
            events: self.events,
            workers,
        }
    }
}

impl Running {
    /// Waits for `DESTROY` (then joins the workers once they delivered
    /// everything queued) or for the channel to end.
    pub(super) fn wait(self) -> Outcome {
        match self.events.recv() {
            Ok(Event::Destroy) => {
                for worker in self.workers {
                    let _ = worker.join();
                }
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
        let _ = self.relay.closer().close();
        let _ = self.reader.join();
    }
}
