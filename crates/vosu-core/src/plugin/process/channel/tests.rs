//! Native channel tests: real `RingObjects` (memfd/eventfd on Linux,
//! section/event on Windows), each direction mapped separately by the
//! "core" and "child" sides as two processes would, plus a third mapping
//! standing in for a hostile peer.

use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use vosu_plugin_api::vosu_opcode;
use vosu_sys::plugin_process::{Doorbell, RingObjects, SharedRegion};

use super::super::frame::{Direction, DirectionPolicy, FrameHeader, MAX_PAYLOAD_BYTES};
use super::super::ring::{PayloadBuffer, REGION_BYTES, RingError, initialize};
use super::*;

const BOUND: Duration = Duration::from_secs(20);

#[cfg(target_os = "linux")]
fn adopt(objects: &RingObjects) -> RingObjects {
    RingObjects::from_inherited(
        objects.mapping_fd().try_clone_to_owned().unwrap(),
        objects.doorbell_fd().try_clone_to_owned().unwrap(),
        REGION_BYTES,
    )
    .unwrap()
}

#[cfg(windows)]
fn adopt(objects: &RingObjects) -> RingObjects {
    RingObjects::from_inherited(
        objects.mapping_handle().try_clone_to_owned().unwrap(),
        objects.doorbell_handle().try_clone_to_owned().unwrap(),
        REGION_BYTES,
    )
    .unwrap()
}

/// A hostile peer's own mapping of one direction.
struct Raw {
    region: SharedRegion,
    doorbell: Doorbell,
}

struct Pair {
    core: Endpoint,
    child: Endpoint,
    /// plugin → host ring, as the child could scribble on it.
    to_host_raw: Raw,
}

fn pair() -> Pair {
    let to_plugin = RingObjects::create(REGION_BYTES).unwrap();
    let to_host = RingObjects::create(REGION_BYTES).unwrap();
    initialize(to_plugin.region()).unwrap();
    initialize(to_host.region()).unwrap();
    let child_to_plugin = adopt(&to_plugin);
    let child_to_host = adopt(&to_host);
    let (region, doorbell) = adopt(&to_host).into_parts();
    let core = connect(
        to_plugin.into(),
        to_host.into(),
        Box::new(DirectionPolicy::new(Direction::PluginToHost)),
    )
    .unwrap();
    let child = connect(
        child_to_host.into(),
        child_to_plugin.into(),
        Box::new(DirectionPolicy::new(Direction::HostToPlugin)),
    )
    .unwrap();
    Pair {
        core,
        child,
        to_host_raw: Raw { region, doorbell },
    }
}

fn to_host(correlation: u64) -> FrameHeader {
    FrameHeader {
        opcode: vosu_opcode::VOSU_OP_PUBLISH_BATCH as u32,
        flags: 0,
        correlation,
    }
}

fn to_plugin(correlation: u64) -> FrameHeader {
    FrameHeader {
        opcode: vosu_opcode::VOSU_OP_STATE_UPDATE as u32,
        flags: 0,
        correlation,
    }
}

fn payload(correlation: u64, len: usize) -> Vec<u8> {
    (0..len)
        .map(|index| (correlation.rotate_left(index as u32 % 64) as u8) ^ index as u8)
        .collect()
}

/// Runs `body` on its own thread; fails instead of hanging past `BOUND`.
fn bounded<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = sync_channel(1);
    thread::spawn(move || {
        let _ = sender.send(body());
    });
    receiver
        .recv_timeout(BOUND)
        .expect("blocked past the test bound")
}

fn recv_one(receiver: &mut Receiver, buffer: &mut PayloadBuffer) -> FrameHeader {
    receiver
        .recv_until(buffer, Some(Instant::now() + BOUND))
        .unwrap()
        .expect("no frame within the test bound")
}

type Seen = (Vec<(FrameHeader, Vec<u8>)>, RecvError);

/// Services a side's receiver until the channel ends, as the relay's
/// ring-reader does; returns every frame it saw.
fn service(mut receiver: Receiver) -> thread::JoinHandle<Seen> {
    thread::spawn(move || {
        let mut buffer = PayloadBuffer::new();
        let mut frames = Vec::new();
        loop {
            match receiver.recv(&mut buffer) {
                Ok(header) => frames.push((header, buffer.bytes().to_vec())),
                Err(error) => return (frames, error),
            }
        }
    })
}

#[test]
fn frames_cross_both_directions_through_separate_mappings() {
    let Pair {
        mut core,
        mut child,
        ..
    } = pair();
    let mut buffer = PayloadBuffer::new();
    for len in [0, 1, 1000, MAX_PAYLOAD_BYTES] {
        let body = payload(len as u64, len);
        core.sender.send(&to_plugin(len as u64), &body).unwrap();
        assert_eq!(
            recv_one(&mut child.receiver, &mut buffer),
            to_plugin(len as u64)
        );
        assert_eq!(buffer.bytes(), body);
        child.sender.send(&to_host(len as u64), &body).unwrap();
        assert_eq!(
            recv_one(&mut core.receiver, &mut buffer),
            to_host(len as u64)
        );
        assert_eq!(buffer.bytes(), body);
    }
}

#[test]
fn concurrent_senders_share_one_producer_through_repeated_full_rings() {
    let Pair { core, child, .. } = pair();
    const THREADS: u64 = 8;
    const FRAMES: u64 = 200;
    // The child's receiver must be serviced: it relays the core's
    // "space freed" rings to the parked senders.
    let child_service = service(child.receiver);
    let collector = thread::spawn(move || {
        let mut receiver = core.receiver;
        let mut buffer = PayloadBuffer::new();
        let mut next = [0_u64; THREADS as usize];
        for _ in 0..THREADS * FRAMES {
            let header = recv_one(&mut receiver, &mut buffer);
            let (thread, sequence) = (header.correlation >> 32, header.correlation & 0xffff_ffff);
            assert_eq!(sequence, next[thread as usize], "per-sender order kept");
            next[thread as usize] += 1;
            let len = (header.correlation as usize * 7919) % 30_000;
            assert_eq!(
                buffer.bytes(),
                payload(header.correlation, len),
                "no torn frame"
            );
        }
        receiver
    });
    let senders: Vec<_> = (0..THREADS)
        .map(|thread| {
            let sender = child.sender.clone();
            thread::spawn(move || {
                for sequence in 0..FRAMES {
                    let correlation = thread << 32 | sequence;
                    let len = (correlation as usize * 7919) % 30_000;
                    sender
                        .send(&to_host(correlation), &payload(correlation, len))
                        .unwrap();
                }
            })
        })
        .collect();
    let receiver = bounded(move || {
        for sender in senders {
            sender.join().unwrap();
        }
        collector.join().unwrap()
    });
    assert_eq!(child.closer.termination(), None);
    child.closer.close().unwrap();
    let (frames, error) = bounded(move || child_service.join().unwrap());
    assert!(frames.is_empty());
    assert!(matches!(error, RecvError::Terminated(Termination::Closed)));
    drop(receiver);
}

#[test]
fn a_full_ring_parks_the_sender_until_the_peer_drains() {
    let Pair {
        mut core, child, ..
    } = pair();
    let child_service = service(child.receiver);
    let max = payload(1, MAX_PAYLOAD_BYTES);
    child.sender.send(&to_host(1), &max).unwrap();
    let (done, finished) = sync_channel(1);
    let sender = child.sender.clone();
    let blocked = thread::spawn(move || {
        let result = sender.send(&to_host(2), &payload(2, MAX_PAYLOAD_BYTES));
        let _ = done.send(());
        result
    });
    assert_eq!(
        finished.recv_timeout(Duration::from_millis(200)),
        Err(RecvTimeoutError::Timeout),
        "second maximum frame must wait for space"
    );
    let mut buffer = PayloadBuffer::new();
    assert_eq!(recv_one(&mut core.receiver, &mut buffer), to_host(1));
    assert_eq!(buffer.bytes(), max);
    finished
        .recv_timeout(BOUND)
        .expect("drain released the sender");
    blocked.join().unwrap().unwrap();
    assert_eq!(recv_one(&mut core.receiver, &mut buffer), to_host(2));
    child.closer.close().unwrap();
    bounded(move || child_service.join().unwrap());
}

#[test]
fn a_stalled_peer_ends_the_channel_once_after_the_window() {
    let Pair {
        mut core, child, ..
    } = pair();
    let mut child_receiver = child.receiver;
    let max = payload(1, MAX_PAYLOAD_BYTES);
    child.sender.send(&to_host(1), &max).unwrap();
    let window = Duration::from_millis(150);
    let start = Instant::now();
    let error = child
        .sender
        .send_within(&to_host(2), &max, window)
        .unwrap_err();
    assert!(matches!(error, SendError::Stalled), "{error}");
    assert!(start.elapsed() >= window);
    assert_eq!(child.closer.termination(), Some(Termination::Stalled));
    let again = child.sender.send(&to_host(3), &[1]).unwrap_err();
    assert!(
        matches!(again, SendError::Terminated(Termination::Stalled)),
        "fails fast"
    );
    let mut buffer = PayloadBuffer::new();
    assert!(matches!(
        child_receiver.recv(&mut buffer),
        Err(RecvError::Terminated(Termination::Stalled))
    ));
    // The peer side is untouched by the stall verdict itself.
    assert_eq!(core.closer.termination(), None);
    assert_eq!(recv_one(&mut core.receiver, &mut buffer), to_host(1));
}

#[test]
fn the_production_backpressure_window_is_two_seconds() {
    let Pair {
        core: _core, child, ..
    } = pair();
    let max = payload(1, MAX_PAYLOAD_BYTES);
    child.sender.send(&to_host(1), &max).unwrap();
    let start = Instant::now();
    let error = bounded(move || child.sender.send(&to_host(2), &max).unwrap_err());
    let elapsed = start.elapsed();
    assert!(matches!(error, SendError::Stalled), "{error}");
    assert!(elapsed >= FULL_RING_WAIT, "{elapsed:?}");
    assert!(
        elapsed < FULL_RING_WAIT + Duration::from_secs(2),
        "{elapsed:?}"
    );
}

#[test]
fn healthy_idle_silence_past_the_window_is_not_a_failure() {
    let Pair {
        mut core, child, ..
    } = pair();
    let mut buffer = PayloadBuffer::new();
    let idle = FULL_RING_WAIT + Duration::from_millis(300);
    let start = Instant::now();
    assert_eq!(
        core.receiver
            .recv_until(&mut buffer, Some(start + idle))
            .unwrap(),
        None
    );
    assert!(start.elapsed() >= idle);
    assert_eq!(core.closer.termination(), None);
    assert_eq!(child.closer.termination(), None);
    child.sender.send(&to_host(9), b"late").unwrap();
    assert_eq!(recv_one(&mut core.receiver, &mut buffer), to_host(9));
}

#[test]
fn an_oversize_payload_is_refused_immediately_without_writing_or_ending_anything() {
    let Pair {
        mut core, child, ..
    } = pair();
    let start = Instant::now();
    let error = child
        .sender
        .send(&to_host(1), &vec![0; MAX_PAYLOAD_BYTES + 1])
        .unwrap_err();
    assert!(matches!(error, SendError::Oversize { len } if len == MAX_PAYLOAD_BYTES + 1));
    assert!(
        start.elapsed() < FULL_RING_WAIT / 4,
        "refused, never parked"
    );
    assert_eq!(child.closer.termination(), None);
    let mut buffer = PayloadBuffer::new();
    assert_eq!(
        core.receiver
            .recv_until(&mut buffer, Some(Instant::now()))
            .unwrap(),
        None
    );
    // The empty ring still takes the legal maximum.
    child
        .sender
        .send(&to_host(2), &payload(2, MAX_PAYLOAD_BYTES))
        .unwrap();
    assert_eq!(recv_one(&mut core.receiver, &mut buffer), to_host(2));
}

#[test]
fn close_wakes_a_parked_receiver_and_a_parked_sender() {
    let Pair { core, child, .. } = pair();
    let receiver = service(core.receiver);
    core.closer.close().unwrap();
    let (frames, error) = bounded(move || receiver.join().unwrap());
    assert!(frames.is_empty());
    assert!(matches!(error, RecvError::Terminated(Termination::Closed)));
    assert!(matches!(
        core.sender.send(&to_plugin(1), b"x"),
        Err(SendError::Terminated(Termination::Closed))
    ));

    // Nobody drains the plugin → host ring any more: the second maximum
    // frame parks until the child's own close wakes it.
    let child_service = service(child.receiver);
    child
        .sender
        .send(&to_host(1), &payload(1, MAX_PAYLOAD_BYTES))
        .unwrap();
    let (done, finished) = sync_channel(1);
    let sender = child.sender.clone();
    let parked = thread::spawn(move || {
        let result = sender.send(&to_host(2), &payload(2, MAX_PAYLOAD_BYTES));
        let _ = done.send(());
        result
    });
    assert_eq!(
        finished.recv_timeout(Duration::from_millis(200)),
        Err(RecvTimeoutError::Timeout),
        "the sender is parked"
    );
    child.closer.close().unwrap();
    finished
        .recv_timeout(BOUND)
        .expect("close woke the parked sender");
    let result = parked.join().unwrap();
    assert!(
        matches!(result, Err(SendError::Terminated(Termination::Closed))),
        "{result:?}"
    );
    let (_, error) = bounded(move || child_service.join().unwrap());
    assert!(matches!(error, RecvError::Terminated(Termination::Closed)));
    child.closer.close().unwrap();
    assert_eq!(
        child.closer.termination(),
        Some(Termination::Closed),
        "idempotent"
    );
}

#[test]
fn stale_and_spurious_rings_are_harmless_hints() {
    let Pair {
        mut core,
        child,
        to_host_raw,
    } = pair();
    for _ in 0..5 {
        to_host_raw.doorbell.ring().unwrap();
    }
    let mut buffer = PayloadBuffer::new();
    assert_eq!(
        core.receiver
            .recv_until(
                &mut buffer,
                Some(Instant::now() + Duration::from_millis(50))
            )
            .unwrap(),
        None
    );
    assert_eq!(core.closer.termination(), None);
    child.sender.send(&to_host(5), b"after").unwrap();
    assert_eq!(recv_one(&mut core.receiver, &mut buffer), to_host(5));
}

#[test]
fn a_hostile_cursor_ends_only_that_channel() {
    let Pair {
        mut core,
        child: _child,
        to_host_raw,
    } = pair();
    let mut other = pair();
    // The child publishes more than a ring's worth.
    to_host_raw.region.word(64).unwrap().store(
        (REGION_BYTES as u64).to_le(),
        std::sync::atomic::Ordering::SeqCst,
    );
    to_host_raw.doorbell.ring().unwrap();
    let mut buffer = PayloadBuffer::new();
    let error = core.receiver.recv(&mut buffer).unwrap_err();
    assert!(
        matches!(
            error,
            RecvError::PeerViolation(RingError::WriteCursor { .. })
        ),
        "{error}"
    );
    assert_eq!(core.closer.termination(), Some(Termination::PeerViolation));
    assert!(matches!(
        core.sender.send(&to_plugin(1), b"x"),
        Err(SendError::Terminated(Termination::PeerViolation))
    ));
    assert_eq!(buffer.capacity_bytes(), 0);
    // An unrelated channel is unaffected.
    other.child.sender.send(&to_host(7), b"fine").unwrap();
    assert_eq!(recv_one(&mut other.core.receiver, &mut buffer), to_host(7));
}

#[test]
fn a_frame_the_policy_forbids_ends_the_channel_before_its_payload_is_copied() {
    let Pair {
        mut core, child, ..
    } = pair();
    // A child sending a host → plugin opcode.
    child.sender.send(&to_plugin(1), &[0; 4096]).unwrap();
    let mut buffer = PayloadBuffer::new();
    let error = core.receiver.recv(&mut buffer).unwrap_err();
    assert!(
        matches!(error, RecvError::PeerViolation(RingError::Policy(_))),
        "{error}"
    );
    assert_eq!(buffer.capacity_bytes(), 0);
}

#[test]
fn ping_pong_never_loses_a_wakeup() {
    let Pair { core, child, .. } = pair();
    const ROUNDS: u64 = 5_000;
    let echo = thread::spawn(move || {
        let mut receiver = child.receiver;
        let mut buffer = PayloadBuffer::new();
        for _ in 0..ROUNDS {
            let header = receiver.recv(&mut buffer).unwrap();
            child
                .sender
                .send(&to_host(header.correlation), buffer.bytes())
                .unwrap();
        }
    });
    bounded(move || {
        let mut receiver = core.receiver;
        let mut buffer = PayloadBuffer::new();
        for round in 0..ROUNDS {
            let body = payload(round, (round % 97) as usize);
            core.sender.send(&to_plugin(round), &body).unwrap();
            assert_eq!(receiver.recv(&mut buffer).unwrap(), to_host(round));
            assert_eq!(buffer.bytes(), body);
        }
        echo.join().unwrap();
    });
}
