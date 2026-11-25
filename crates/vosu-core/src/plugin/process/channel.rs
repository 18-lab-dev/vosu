//! One side of a plugin-host channel: a serialized producer on the
//! outbound ring, the single consumer of the inbound ring, and their
//! event-driven waits (`docs/v2/plugin-abi.md` §7.2).
//!
//! **Doorbells.** Each side waits on exactly one doorbell, the one paired
//! with its inbound ring, and rings exactly one, the one paired with its
//! outbound ring (the peer's inbound doorbell). A side rings the peer for
//! two reasons: it published frames while the peer's consumer was parked,
//! or it freed inbound space the peer's producer asked for. A ring is only
//! a hint; whoever wakes rechecks the shared cursors.
//!
//! **Space waits.** A sender facing a full outbound ring marks the ring
//! "space wanted", rechecks, and parks on a local condition variable for at
//! most [`FULL_RING_WAIT`] in total. The peer's consumer rings this side's
//! doorbell after freeing space; this side's [`Receiver`] wakes and
//! announces it to parked senders, and also announces on every receive
//! while senders are parked, so a receiver busy with inbound traffic cannot
//! starve them. Senders therefore make progress only while this side's
//! `Receiver` is being serviced, which the relay's dedicated ring-reader
//! thread always does. Nothing polls, spins or sleeps; healthy idle
//! silence waits indefinitely and is never a failure.
//!
//! **Termination.** A full window without space (the peer stopped
//! draining), any peer protocol violation, a doorbell OS failure, or a
//! local [`Closer::close`] terminates the channel once. Termination wakes
//! the receiver and every parked sender; every later call fails fast with
//! [`Termination`]. An oversize payload is refused without writing and
//! without terminating anything.

use std::io;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vosu_sys::plugin_process::{Doorbell, RingObjects, SharedRegion};

use super::frame::{FrameHeader, FramePolicy, HEAD_BYTES, Ledger, MAX_PAYLOAD_BYTES, encode_head};
use super::ring::{PayloadBuffer, RingConsumer, RingError, RingProducer};

/// The longest a legal frame waits for space in a full ring before the
/// peer is treated as unresponsive (§7.2 backpressure window).
pub const FULL_RING_WAIT: Duration = Duration::from_secs(2);

/// Why a channel ended. Recorded once; the first cause wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    /// [`Closer::close`] was called on this side.
    Closed,
    /// A legal frame found no space for the whole [`FULL_RING_WAIT`].
    Stalled,
    /// The peer broke the ring protocol.
    PeerViolation,
    /// A doorbell operation failed.
    Os,
    /// [`Closer::peer_exited`]: the peer process is gone. The receiver
    /// still returns every frame the peer completed, then this.
    PeerExited,
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// Refused before anything was written; the channel is unaffected.
    #[error("payload of {len} bytes exceeds the {MAX_PAYLOAD_BYTES}-byte frame cap")]
    Oversize { len: usize },
    #[error("outbound ring stayed full for the whole backpressure window")]
    Stalled,
    #[error("peer broke the ring protocol: {0}")]
    PeerViolation(RingError),
    #[error("doorbell failed: {0}")]
    Os(io::Error),
    #[error("channel already terminated: {0:?}")]
    Terminated(Termination),
}

#[derive(Debug, thiserror::Error)]
pub enum RecvError {
    #[error("peer broke the ring protocol: {0}")]
    PeerViolation(RingError),
    #[error("doorbell failed: {0}")]
    Os(io::Error),
    #[error("channel already terminated: {0:?}")]
    Terminated(Termination),
}

/// One ring direction as the channel takes it.
#[derive(Debug)]
pub struct RingEnd {
    pub region: SharedRegion,
    pub doorbell: Doorbell,
}

impl From<RingObjects> for RingEnd {
    fn from(objects: RingObjects) -> Self {
        let (region, doorbell) = objects.into_parts();
        Self { region, doorbell }
    }
}

/// The handles of one attached side.
#[derive(Debug)]
pub struct Endpoint {
    pub sender: Sender,
    pub receiver: Receiver,
    pub closer: Closer,
    /// The live acknowledged requests the receiving policy pairs replies
    /// with (`frame::Ledger`); a fresh, unshared one for a policy that
    /// keeps none.
    pub ledger: Arc<Ledger>,
}

/// Attaches one side: `outbound` is the ring this side produces into,
/// `inbound` the ring it consumes, already initialized by the creator.
/// Core passes (host→plugin, plugin→host); the child passes them swapped.
/// `policy` admits each inbound frame before its payload is copied.
pub fn connect(
    outbound: RingEnd,
    inbound: RingEnd,
    policy: Box<dyn FramePolicy>,
) -> Result<Endpoint, RingError> {
    let producer = RingProducer::attach(Arc::new(outbound.region))?;
    let consumer = RingConsumer::attach(Arc::new(inbound.region))?;
    let shared = Arc::new(Shared {
        producer: Mutex::new(producer),
        outbound_bell: outbound.doorbell,
        inbound_bell: inbound.doorbell,
        termination: AtomicU8::new(OPEN),
        space_epoch: Mutex::new(0),
        space_changed: Condvar::new(),
        space_waiters: AtomicUsize::new(0),
    });
    let ledger = policy.ledger().unwrap_or_default();
    Ok(Endpoint {
        sender: Sender {
            shared: shared.clone(),
        },
        receiver: Receiver {
            shared: shared.clone(),
            consumer,
            policy,
        },
        closer: Closer { shared },
        ledger,
    })
}

const OPEN: u8 = 0;

#[derive(Debug)]
struct Shared {
    /// The one logical producer: every sender serializes here.
    producer: Mutex<RingProducer>,
    outbound_bell: Doorbell,
    inbound_bell: Doorbell,
    termination: AtomicU8,
    /// Bumped whenever outbound space may have been freed.
    space_epoch: Mutex<u64>,
    space_changed: Condvar,
    space_waiters: AtomicUsize,
}

impl Shared {
    fn termination(&self) -> Option<Termination> {
        match self.termination.load(Ordering::SeqCst) {
            OPEN => None,
            1 => Some(Termination::Closed),
            2 => Some(Termination::Stalled),
            3 => Some(Termination::PeerViolation),
            4 => Some(Termination::Os),
            _ => Some(Termination::PeerExited),
        }
    }

    /// Records `cause` unless another cause already ended the channel, and
    /// wakes everything parked on this side.
    fn terminate(&self, cause: Termination) -> io::Result<()> {
        let code = match cause {
            Termination::Closed => 1,
            Termination::Stalled => 2,
            Termination::PeerViolation => 3,
            Termination::Os => 4,
            Termination::PeerExited => 5,
        };
        if self
            .termination
            .compare_exchange(OPEN, code, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(());
        }
        self.announce_space();
        // Releases a local ring the peer made block on its saturated
        // counter (Linux), then wakes this side's receiver.
        self.outbound_bell.consume()?;
        self.inbound_bell.ring()
    }

    /// [`terminate`](Self::terminate) on a failure path that already reports
    /// its own error: the cause is recorded first, so a wake failure here
    /// is only logged.
    fn fail(&self, cause: Termination) {
        if let Err(error) = self.terminate(cause) {
            tracing::warn!(%error, ?cause, "plugin-host channel wake failed during termination");
        }
    }

    fn announce_space(&self) {
        let mut epoch = lock(&self.space_epoch);
        *epoch = epoch.wrapping_add(1);
        drop(epoch);
        self.space_changed.notify_all();
    }

    fn space_epoch(&self) -> u64 {
        *lock(&self.space_epoch)
    }

    /// Parks until the epoch moves past `seen`, the channel ends, or
    /// `deadline` (`None`: none) passes. Returns false only for the deadline.
    fn wait_for_space(&self, seen: u64, deadline: Option<Instant>) -> bool {
        let mut epoch = lock(&self.space_epoch);
        while *epoch == seen && self.termination().is_none() {
            epoch = match deadline {
                None => self
                    .space_changed
                    .wait(epoch)
                    .unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return false;
                    }
                    self.space_changed
                        .wait_timeout(epoch, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
        true
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panicking holder cannot leave either lock's data inconsistent: the
    // producer publishes a frame only after writing all of it.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sends frames on the outbound ring. Cloneable: every clone shares the one
/// serialized producer.
#[derive(Debug, Clone)]
pub struct Sender {
    shared: Arc<Shared>,
}

impl Sender {
    /// Writes one frame, parking at most [`FULL_RING_WAIT`] for space.
    pub fn send(&self, header: &FrameHeader, payload: &[u8]) -> Result<(), SendError> {
        self.send_within(header, payload, FULL_RING_WAIT)
    }

    /// Writes an `ACK_NEXT` frame carrying `request` and then the public
    /// frame it acknowledges, back to back under the one producer lock, so
    /// no other sender's frame can come between them (§7.6). The pair
    /// shares one backpressure window. An oversize payload is refused
    /// before either frame is written.
    pub fn send_acknowledged(
        &self,
        request: u64,
        header: &FrameHeader,
        payload: &[u8],
    ) -> Result<(), SendError> {
        let ack = FrameHeader {
            opcode: super::frame::control::ACK_NEXT,
            flags: 0,
            correlation: request,
        };
        self.send_frames(&[(&ack, &[]), (header, payload)], FULL_RING_WAIT)
    }

    /// Writes a `REPLY_NEXT` frame carrying `request` and then the public
    /// `READ_PLAN_RESULT` answering that `SYNC` request, back to back under
    /// the one producer lock, like [`send_acknowledged`](Self::send_acknowledged).
    pub fn send_reply(
        &self,
        request: u64,
        header: &FrameHeader,
        payload: &[u8],
    ) -> Result<(), SendError> {
        let reply = FrameHeader {
            opcode: super::frame::control::REPLY_NEXT,
            flags: 0,
            correlation: request,
        };
        self.send_frames(&[(&reply, &[]), (header, payload)], FULL_RING_WAIT)
    }

    /// [`send`](Self::send) with an explicit backpressure window.
    pub(super) fn send_within(
        &self,
        header: &FrameHeader,
        payload: &[u8],
        window: Duration,
    ) -> Result<(), SendError> {
        self.send_frames(&[(header, payload)], window)
    }

    /// Writes `frames` (at most two) in order under one hold of the
    /// producer lock, then rings the peer once if its consumer is parked.
    fn send_frames(
        &self,
        frames: &[(&FrameHeader, &[u8])],
        window: Duration,
    ) -> Result<(), SendError> {
        let mut heads = [[0_u8; HEAD_BYTES]; 2];
        assert!(frames.len() <= heads.len(), "at most two frames per write");
        for ((header, payload), head) in frames.iter().zip(heads.iter_mut()) {
            *head = encode_head(header, payload.len())
                .map_err(|_| SendError::Oversize { len: payload.len() })?;
        }
        let shared = &*self.shared;
        let deadline = Instant::now().checked_add(window);
        let mut producer = lock(&shared.producer);
        for ((_, payload), head) in frames.iter().zip(heads.iter()) {
            self.write_one(&mut producer, head, payload, deadline)?;
        }
        let parked = producer
            .consumer_parked()
            .map_err(|error| self.violation(error))?;
        drop(producer);
        if parked {
            self.ring_peer()?;
        }
        Ok(())
    }

    /// Writes one frame with the producer lock held, parking until
    /// `deadline` for space.
    fn write_one(
        &self,
        producer: &mut RingProducer,
        head: &[u8; HEAD_BYTES],
        payload: &[u8],
        deadline: Option<Instant>,
    ) -> Result<(), SendError> {
        let shared = &*self.shared;
        loop {
            if let Some(cause) = shared.termination() {
                return Err(SendError::Terminated(cause));
            }
            if producer
                .try_write(head, payload)
                .map_err(|error| self.violation(error))?
            {
                return Ok(());
            }
            // A frame this hold already published (the first of a pair)
            // may be all a parked peer consumer has to read before it frees
            // space: ring it before parking.
            if producer
                .consumer_parked()
                .map_err(|error| self.violation(error))?
            {
                self.ring_peer()?;
            }
            // Full: capture the epoch, ask for a ring, then recheck, so a
            // consumer that freed space just before seeing the flag (and
            // so never rings) is still noticed.
            let seen = shared.space_epoch();
            shared.space_waiters.fetch_add(1, Ordering::SeqCst);
            let outcome = producer
                .want_space()
                .and_then(|()| producer.free())
                .map(|free| {
                    free as usize >= head.len() + payload.len()
                        || shared.wait_for_space(seen, deadline)
                });
            shared.space_waiters.fetch_sub(1, Ordering::SeqCst);
            if !outcome.map_err(|error| self.violation(error))? {
                shared.fail(Termination::Stalled);
                return Err(SendError::Stalled);
            }
        }
    }

    fn ring_peer(&self) -> Result<(), SendError> {
        self.shared.outbound_bell.ring().map_err(|error| {
            self.shared.fail(Termination::Os);
            SendError::Os(error)
        })
    }

    fn violation(&self, error: RingError) -> SendError {
        self.shared.fail(Termination::PeerViolation);
        SendError::PeerViolation(error)
    }
}

/// The single consumer of the inbound ring. Not cloneable: the relay's one
/// ring-reader thread owns it.
pub struct Receiver {
    shared: Arc<Shared>,
    consumer: RingConsumer,
    policy: Box<dyn FramePolicy>,
}

impl std::fmt::Debug for Receiver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Receiver")
            .field("consumer", &self.consumer)
            .finish_non_exhaustive()
    }
}

impl Receiver {
    /// Waits for the next frame, its payload copied into `buffer`. Waits
    /// indefinitely: idle silence is healthy.
    pub fn recv(&mut self, buffer: &mut PayloadBuffer) -> Result<FrameHeader, RecvError> {
        loop {
            if let Some(header) = self.recv_until(buffer, None)? {
                return Ok(header);
            }
        }
    }

    /// Waits until `deadline` (`None`: no limit) for the next frame.
    /// `Ok(None)` means the deadline passed with nothing published.
    pub fn recv_until(
        &mut self,
        buffer: &mut PayloadBuffer,
        deadline: Option<Instant>,
    ) -> Result<Option<FrameHeader>, RecvError> {
        loop {
            if let Some(cause) = self.shared.termination() {
                if cause != Termination::PeerExited {
                    return Err(RecvError::Terminated(cause));
                }
                // The peer can publish nothing more: drain what it did,
                // without parking, then report the end.
                return match self.consumer.try_read(buffer, &mut *self.policy) {
                    Ok(Some((header, _))) => Ok(Some(header)),
                    Ok(None) => Err(RecvError::Terminated(cause)),
                    Err(error) => Err(RecvError::PeerViolation(error)),
                };
            }
            if self.shared.space_waiters.load(Ordering::SeqCst) > 0 {
                self.shared.announce_space();
            }
            match self.consumer.try_read(buffer, &mut *self.policy) {
                Ok(Some((header, space_wanted))) => {
                    if space_wanted {
                        self.shared
                            .outbound_bell
                            .ring()
                            .map_err(|error| self.os(error))?;
                    }
                    return Ok(Some(header));
                }
                Ok(None) => {}
                Err(error) => return Err(self.violation(error)),
            }
            // Park: mark parked, then recheck, so a frame published before
            // the producer could see the mark is still taken.
            self.consumer
                .set_parked(true)
                .map_err(|error| self.violation(error))?;
            let available = self.consumer.available();
            if !matches!(available, Ok(0)) {
                self.consumer
                    .set_parked(false)
                    .map_err(|error| self.violation(error))?;
                available.map_err(|error| self.violation(error))?;
                continue;
            }
            let timeout = match deadline {
                None => None,
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        self.consumer
                            .set_parked(false)
                            .map_err(|error| self.violation(error))?;
                        return Ok(None);
                    }
                    Some(remaining)
                }
            };
            let woke = self.shared.inbound_bell.wait(timeout);
            self.consumer
                .set_parked(false)
                .map_err(|error| self.violation(error))?;
            if woke.map_err(|error| self.os(error))? {
                // Any wake may be the peer announcing freed outbound space.
                self.shared.announce_space();
            }
        }
    }

    fn violation(&self, error: RingError) -> RecvError {
        self.shared.fail(Termination::PeerViolation);
        RecvError::PeerViolation(error)
    }

    fn os(&self, error: io::Error) -> RecvError {
        self.shared.fail(Termination::Os);
        RecvError::Os(error)
    }
}

/// Ends the channel from this side: wakes the receiver and every parked
/// sender, which then fail with [`Termination::Closed`]. Idempotent.
#[derive(Debug, Clone)]
pub struct Closer {
    shared: Arc<Shared>,
}

impl Closer {
    pub fn close(&self) -> io::Result<()> {
        self.shared.terminate(Termination::Closed)
    }

    /// Ends the channel because the peer process exited: senders fail at
    /// once, and the receiver drains the frames the peer completed before
    /// failing with [`Termination::PeerExited`]. Idempotent; a cause
    /// already recorded wins.
    pub fn peer_exited(&self) -> io::Result<()> {
        self.shared.terminate(Termination::PeerExited)
    }

    /// Why the channel ended, if it has.
    pub fn termination(&self) -> Option<Termination> {
        self.shared.termination()
    }
}

#[cfg(test)]
mod tests;
