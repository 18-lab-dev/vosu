//! One plugin-host generation after spawn: its exit watcher, its ring
//! reader, the startup handshake and the [`ProcessEndpoint`] core
//! dispatches through (`docs/v2/plugin-abi.md` §7.6).
//!
//! Every outcome passes through one [`Shared`] state under one lock,
//! which no host lock ever nests in: the startup result (latest truth until
//! the launch consumes it, so an exit or channel failure right after
//! `READY` still fails the launch), the phase, and the exit status that only
//! the watcher, the one reaper, records. A loss core did not request
//! (`Ready` phase only) is reported once through [`InstanceLoss`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vosu_plugin_api::{vosu_opcode, vosu_status};

use super::super::channel::{Endpoint, Receiver, RecvError, SendError, Sender, Termination};
use super::super::control::{Failed, Ready, StartupFailure, StartupStage};
use super::super::frame::{FrameHeader, Ledger, PRIVATE_OPCODE_BIT, control};
use super::super::ring::PayloadBuffer;
use super::ingress::{Ingress, StatusGate, SyncClaim, claim_sync_reply};
use super::platform::{HostProcess, Started};
use crate::plugin::error::HostError;
use crate::plugin::execution::descriptor::check_header;
use crate::plugin::execution::{
    InstanceLoss, LossCause, MessageRef, PluginIngress, ProcessEndpoint,
};

/// What [`start`] needs besides the spawned child.
pub(super) struct Start {
    pub generation: u64,
    pub plugin_id: String,
    /// The encoded `CREATE` payload.
    pub create: Vec<u8>,
    pub deadline: Instant,
    pub ingress: PluginIngress,
    pub loss: InstanceLoss,
    /// The `SYNC` budget sent in `CREATE`: core answers a plan still
    /// unanswered after it `VOSU_ERR_TIMED_OUT` (§7.6).
    pub sync_budget: Duration,
}

/// The next endpoint identity ([`claim_sync_reply`]).
static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Spawned; `READY` not yet consumed by the launch.
    Starting,
    /// Published; dispatch flows.
    Ready,
    /// `DESTROY` was sent; exit is expected.
    Destroying,
    /// Torn down, lost, or failed: nothing more is delivered.
    Gone,
}

struct State {
    phase: Phase,
    startup: Option<Result<(), StartupFailure>>,
    /// The reaped exit status, recorded once by the watcher.
    exit: Option<i64>,
    destroyed: bool,
    /// A `Ready` instance's channel or process failed.
    lost: bool,
    /// Core already sent the kill.
    killed: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    process: Arc<dyn HostProcess>,
    closer: super::super::channel::Closer,
    loss: InstanceLoss,
    generation: u64,
    plugin_id: String,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn lifecycle(opcode: u32, correlation: u64) -> FrameHeader {
    FrameHeader {
        opcode,
        flags: 0,
        correlation,
    }
}

/// The startup failure a child's exit before `READY` names.
fn exit_failure(status: i64) -> StartupFailure {
    let stage = i32::try_from(status)
        .ok()
        .and_then(StartupStage::from_exit_status);
    match stage {
        Some(StartupStage::Generation) => StartupFailure {
            stage: StartupStage::Generation,
            code: "executable_updated".to_owned(),
            errno: None,
            message: "the vosu executable changed since this daemon started; restart vosu"
                .to_owned(),
        },
        Some(stage) => StartupFailure::new(
            stage,
            stage.as_str(),
            format!(
                "plugin host exited with status {status} at stage {}",
                stage.as_str()
            ),
        ),
        None => StartupFailure::new(
            StartupStage::Crash,
            "exited",
            format!("plugin host exited with status {status} before READY"),
        ),
    }
}

impl Shared {
    /// Kills the process once; later calls do nothing.
    fn kill(&self) {
        let first = {
            let mut state = lock(&self.state);
            let first = !state.killed && state.exit.is_none();
            state.killed = true;
            first
        };
        if first && let Err(error) = self.process.kill() {
            tracing::warn!(plugin = %self.plugin_id, %error, "failed to kill a plugin host");
        }
    }

    /// The watcher's one report of the reaped exit. During startup the
    /// reader first drains what the child completed (its `FAILED` or
    /// `READY` names the outcome better than the bare exit), then
    /// [`Self::drained`] settles it.
    fn exited(&self, status: i64) {
        let (report, starting) = {
            let mut state = lock(&self.state);
            state.exit = Some(status);
            match state.phase {
                Phase::Starting => (false, true),
                Phase::Ready => (!std::mem::replace(&mut state.lost, true), false),
                Phase::Destroying | Phase::Gone => (false, false),
            }
        };
        self.changed.notify_all();
        if report {
            self.loss.report(LossCause::ProcessExited {
                status: Some(status),
            });
        }
        let _ = if starting {
            self.closer.peer_exited()
        } else {
            self.closer.close()
        };
    }

    /// The reader drained every frame an exited child completed. A startup
    /// still without an outcome failed with the exit.
    fn drained(&self) {
        let mut state = lock(&self.state);
        if state.phase == Phase::Starting && state.startup.is_none() {
            let status = state.exit.unwrap_or(i64::MIN);
            state.startup = Some(Err(exit_failure(status)));
        }
        drop(state);
        self.changed.notify_all();
    }

    /// The reader's report of a channel or protocol failure.
    fn channel_failed(&self, reason: &str) {
        let report = {
            let mut state = lock(&self.state);
            match state.phase {
                Phase::Starting => {
                    if !matches!(state.startup, Some(Err(_))) {
                        state.startup = Some(Err(StartupFailure::new(
                            StartupStage::Crash,
                            "channel_failed",
                            reason,
                        )));
                    }
                    false
                }
                Phase::Ready => !std::mem::replace(&mut state.lost, true),
                Phase::Destroying | Phase::Gone => false,
            }
        };
        self.changed.notify_all();
        tracing::warn!(plugin = %self.plugin_id, reason, "plugin-host channel failed");
        let _ = self.closer.close();
        self.kill();
        if report {
            self.loss.report(LossCause::ChannelFailed);
        }
    }

    fn ready(&self, ready: Ready) -> Result<(), String> {
        let mut state = lock(&self.state);
        if state.phase != Phase::Starting || state.startup.is_some() {
            return Err("READY out of order".to_owned());
        }
        if ready.generation != self.generation {
            return Err(format!(
                "READY for generation {} of {}",
                ready.generation, self.generation
            ));
        }
        state.startup = Some(
            check_header(ready.struct_size, ready.abi_version).map_err(|error| {
                StartupFailure::new(StartupStage::Abi, "abi_mismatch", error.to_string())
            }),
        );
        drop(state);
        self.changed.notify_all();
        Ok(())
    }

    fn failed(&self, failed: Failed) -> Result<(), String> {
        let mut state = lock(&self.state);
        if state.phase != Phase::Starting || state.startup.is_some() {
            return Err("FAILED out of order".to_owned());
        }
        if failed.generation != 0 && failed.generation != self.generation {
            return Err(format!(
                "FAILED for generation {} of {}",
                failed.generation, self.generation
            ));
        }
        state.startup = Some(Err(failed.into_failure()));
        drop(state);
        self.changed.notify_all();
        Ok(())
    }

    fn destroyed(&self) -> Result<(), String> {
        let mut state = lock(&self.state);
        if state.phase != Phase::Destroying || state.destroyed {
            return Err("DESTROYED without DESTROY".to_owned());
        }
        state.destroyed = true;
        Ok(())
    }

    /// Waits for the startup result until `deadline`, then publishes the
    /// instance (`Ready`) or retires it (`Gone`) atomically with that read.
    fn finish_startup(&self, deadline: Instant) -> Result<(), StartupFailure> {
        let mut state = lock(&self.state);
        loop {
            if let Some(result) = state.startup.take() {
                let result = result.and_then(|()| match state.exit {
                    Some(status) => Err(exit_failure(status)),
                    None => Ok(()),
                });
                state.phase = if result.is_ok() {
                    Phase::Ready
                } else {
                    Phase::Gone
                };
                return result;
            }
            let now = Instant::now();
            if now >= deadline {
                state.phase = Phase::Gone;
                return Err(StartupFailure::new(
                    StartupStage::Timeout,
                    "timeout",
                    "no READY or FAILED within the startup deadline",
                ));
            }
            state = self
                .changed
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Waits for the reaped exit until `deadline` (`None`: no limit).
    fn wait_exit(&self, deadline: Option<Instant>) -> Option<i64> {
        let mut state = lock(&self.state);
        loop {
            if let Some(status) = state.exit {
                return Some(status);
            }
            state = match deadline {
                None => self
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return None;
                    }
                    self.changed
                        .wait_timeout(state, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
    }
}

/// Starts supervision of `started`, sends `CREATE`, and waits for the
/// startup result. On failure the child is killed and reaped and every
/// thread joined before this returns.
pub(super) fn start(
    started: Started,
    start: Start,
) -> Result<Arc<dyn ProcessEndpoint>, StartupFailure> {
    let Started { endpoint, process } = started;
    let Endpoint {
        sender,
        receiver,
        closer,
        ledger,
    } = endpoint;
    let shared = Arc::new(Shared {
        state: Mutex::new(State {
            phase: Phase::Starting,
            startup: None,
            exit: None,
            destroyed: false,
            lost: false,
            killed: false,
        }),
        changed: Condvar::new(),
        process,
        closer,
        loss: start.loss,
        generation: start.generation,
        plugin_id: start.plugin_id,
    });
    let gate = Arc::new(StatusGate::default());
    // The endpoint's identity for SYNC reply claims: never reused while
    // core runs, so a broker worker still answering an older generation's
    // request can never be mistaken for one of this endpoint's.
    let identity = NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed);
    let mut threads = Vec::with_capacity(2);
    let ingress = Ingress::start(
        start.ingress,
        sender.clone(),
        Arc::clone(&ledger),
        Arc::clone(&gate),
        identity,
        start.sync_budget,
    )
    .map(Arc::new);
    let watcher = spawn_thread("vosu-plugin-exit", {
        let shared = Arc::clone(&shared);
        move || watch(&shared)
    });
    let spawned = match (watcher, &ingress) {
        (Ok(watcher), Ok(ingress)) => {
            threads.push(watcher);
            spawn_thread("vosu-plugin-ring", {
                let shared = Arc::clone(&shared);
                let ingress = Arc::clone(ingress);
                move || read_ring(&shared, receiver, &ingress)
            })
        }
        (Ok(watcher), Err(error)) => {
            threads.push(watcher);
            shared.kill();
            Err(std::io::Error::new(error.kind(), error.to_string()))
        }
        (Err(error), _) => {
            // No watcher will ever reap it: do that here, so teardown's
            // wait for the exit ends.
            shared.kill();
            watch(&shared);
            Err(error)
        }
    };
    let endpoint = ChildEndpoint {
        shared,
        sender,
        ledger,
        gate,
        identity,
        ingress: ingress.ok(),
        threads: Mutex::new(threads),
    };
    let result = spawned
        .map(|reader| lock(&endpoint.threads).push(reader))
        .map_err(|error| StartupFailure::new(StartupStage::Spawn, "thread", error.to_string()))
        .and_then(|()| {
            // A failed write means the channel already ended; the watcher
            // or reader records why, and `finish_startup` reports that.
            if let Err(error) = endpoint
                .sender
                .send(&lifecycle(control::CREATE, 0), &start.create)
            {
                tracing::debug!(%error, "CREATE could not be written");
            }
            endpoint.shared.finish_startup(start.deadline)
        });
    match result {
        Ok(()) => Ok(Arc::new(endpoint)),
        Err(failure) => {
            // `endpoint` drops here: kill, reap and join.
            lock(&endpoint.shared.state).phase = Phase::Gone;
            Err(failure)
        }
    }
}

fn spawn_thread(
    name: &str,
    body: impl FnOnce() + Send + 'static,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
}

/// The one reaper: waits for the exit and records it.
fn watch(shared: &Shared) {
    let status = shared.process.wait().unwrap_or_else(|error| {
        tracing::warn!(plugin = %shared.plugin_id, %error, "waiting on a plugin host failed");
        i64::MIN
    });
    shared.exited(status);
}

/// The ring reader: lifecycle frames, and every public frame handed to the
/// ingress lanes with the `ACK_NEXT` id the policy paired with it. A
/// violation of the §7.6 state machine loses the channel.
fn read_ring(shared: &Shared, mut receiver: Receiver, ingress: &Ingress) {
    let mut buffer = PayloadBuffer::new();
    let mut acknowledge: Option<u64> = None;
    let failure = loop {
        let header = match receiver.recv(&mut buffer) {
            Ok(header) => header,
            // Closed only by core itself, after it already knows why.
            Err(RecvError::Terminated(Termination::Closed)) => break None,
            Err(RecvError::Terminated(Termination::PeerExited)) => {
                shared.drained();
                break None;
            }
            Err(error) => break Some(error.to_string()),
        };
        let outcome = match header.opcode {
            control::ACK_NEXT => {
                acknowledge = Some(header.correlation);
                Ok(())
            }
            control::READY => Ready::decode(buffer.bytes())
                .map_err(|error| error.to_string())
                .and_then(|ready| shared.ready(ready)),
            control::FAILED => Failed::decode(buffer.bytes())
                .map_err(|error| error.to_string())
                .and_then(|failed| shared.failed(failed)),
            control::DESTROYED => shared.destroyed(),
            opcode if opcode & PRIVATE_OPCODE_BIT != 0 => {
                Err(format!("unexpected private frame {opcode:#x}"))
            }
            // The policy already paired every public frame but a log record
            // with its ACK_NEXT and admitted it under the request cap.
            _ => {
                ingress.submit(acknowledge.take(), header, &mut buffer);
                Ok(())
            }
        };
        if let Err(reason) = outcome {
            break Some(reason);
        }
    };
    if let Some(reason) = failure {
        shared.channel_failed(&reason);
    }
}

/// Core's end of one plugin host, as the instance dispatches through it.
struct ChildEndpoint {
    shared: Arc<Shared>,
    sender: Sender,
    ledger: Arc<Ledger>,
    gate: Arc<StatusGate>,
    /// This endpoint's identity for [`claim_sync_reply`].
    identity: u64,
    ingress: Option<Arc<Ingress>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl ChildEndpoint {
    /// Kills the child if it still runs, waits for the watcher's reap,
    /// closes the channel and joins both threads.
    fn retire(&self) {
        self.shared.kill();
        self.shared.wait_exit(None);
        let _ = self.shared.closer.close();
        if let Some(ingress) = &self.ingress {
            ingress.shutdown();
        }
        let current = std::thread::current().id();
        for thread in std::mem::take(&mut *lock(&self.threads)) {
            if thread.thread().id() != current {
                let _ = thread.join();
            }
        }
    }
}

impl ProcessEndpoint for ChildEndpoint {
    fn dispatch(&self, message: MessageRef<'_>) -> vosu_status {
        if message.opcode & PRIVATE_OPCODE_BIT != 0 {
            return vosu_status::VOSU_ERR_UNSUPPORTED;
        }
        let header = FrameHeader {
            opcode: message.opcode,
            flags: message.flags,
            correlation: message.correlation_id,
        };
        // An accepted SYNC plan's result, delivered by the broker worker
        // handling it: REPLY_NEXT routes it to the child's waiter. The
        // budget timer may have answered the request first; a worker
        // answering another endpoint's request delivers nothing here.
        if message.opcode == vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32 {
            match claim_sync_reply(self.identity) {
                SyncClaim::Ordinary => {}
                SyncClaim::Reply(request) => {
                    if self.ledger.close(request).is_none() {
                        return vosu_status::VOSU_ERR_TIMED_OUT;
                    }
                    return match self.sender.send_reply(request, &header, message.payload) {
                        Ok(()) => vosu_status::VOSU_OK,
                        Err(SendError::Oversize { .. }) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
                        Err(_) => vosu_status::VOSU_ERR_NOT_READY,
                    };
                }
                SyncClaim::Foreign => return vosu_status::VOSU_ERR_TARGET_GONE,
            }
        }
        {
            let state = lock(&self.shared.state);
            if state.phase != Phase::Ready || state.lost {
                return vosu_status::VOSU_ERR_NOT_READY;
            }
        }
        // After the STATUS of whatever request the ingress worker is
        // handling: that STATUS precedes the results it causes.
        self.gate.pass();
        match self.sender.send(&header, message.payload) {
            Ok(()) => vosu_status::VOSU_OK,
            Err(SendError::Oversize { .. }) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            // The reader observes the terminated channel and reports the
            // loss once.
            Err(_) => vosu_status::VOSU_ERR_NOT_READY,
        }
    }

    fn destroy(&self, grace: Duration) -> Result<(), HostError> {
        let deadline = Instant::now() + grace;
        let controlled = {
            let mut state = lock(&self.shared.state);
            let controlled = state.phase == Phase::Ready && !state.lost;
            state.phase = if controlled {
                Phase::Destroying
            } else {
                Phase::Gone
            };
            controlled
        };
        if !controlled {
            self.retire();
            return Ok(());
        }
        let sent = self.sender.send(&lifecycle(control::DESTROY, 0), &[]);
        let exited = sent.is_ok() && self.shared.wait_exit(Some(deadline)).is_some();
        let result = if exited {
            let state = lock(&self.shared.state);
            if !state.destroyed || state.exit != Some(0) {
                tracing::warn!(
                    plugin = %self.shared.plugin_id,
                    status = ?state.exit,
                    destroyed = state.destroyed,
                    "plugin host exited during destroy without a clean DESTROYED"
                );
            }
            Ok(())
        } else {
            tracing::warn!(
                plugin = %self.shared.plugin_id,
                pid = self.shared.process.pid(),
                "destroy_timeout: killing the plugin host"
            );
            Err(HostError::DestroyTimedOut)
        };
        lock(&self.shared.state).phase = Phase::Gone;
        self.retire();
        result
    }

    fn terminate(&self) {
        lock(&self.shared.state).phase = Phase::Gone;
        self.retire();
    }
}

impl Drop for ChildEndpoint {
    fn drop(&mut self) {
        lock(&self.shared.state).phase = Phase::Gone;
        self.retire();
    }
}
