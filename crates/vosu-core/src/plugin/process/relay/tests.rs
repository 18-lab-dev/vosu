//! The plugin host's relay over a real channel (memfd/eventfd on Linux,
//! section/event on Windows), with core played by the test and a Rust
//! plugin `dispatch` ([`FakePlugin`]). The real child is exercised by the
//! root `plugin_host_native` target's `relay` module.

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vosu_plugin_api::{
    ActionInvoke, Caller, CallerKind, CborValue, FocusChanged, Message, PlanBase, PlanOp,
    ReadPlanResult, ReadPlanSubmit, ReadResult, ScanRequest, Selector, Stage, StageBase,
    StateEntry, StateSubscribe, StateUpdate, TimerSet, vosu_opcode, vosu_status,
};

use super::super::channel::{Endpoint, Termination};
use super::super::control::EXIT_TRANSPORT_TIMEOUT;
use super::super::fake_plugin::{FakePlugin, Gate, channel_pair as pair};
use super::super::frame::{FrameHeader, Ledger, control};
use super::*;

const BOUND: Duration = Duration::from_secs(20);

/// One plugin host relay with its reader and workers running.
struct Child {
    relay: Arc<Relay>,
    plugin: Arc<FakePlugin>,
    fatal: Arc<Mutex<Vec<i32>>>,
    destroyed: Arc<Mutex<bool>>,
    threads: Vec<JoinHandle<()>>,
}

impl Child {
    fn start(child: Endpoint, workers: usize) -> Self {
        let Endpoint {
            sender,
            receiver,
            closer,
            ledger,
        } = child;
        let fatal = Arc::new(Mutex::new(Vec::new()));
        let relay = Arc::new(Relay::new(
            sender,
            closer,
            Waiters::new(ledger),
            Duration::from_millis(2_000),
            Box::new({
                let fatal = Arc::clone(&fatal);
                move |code| {
                    fatal.lock().unwrap().push(code);
                    vosu_status::VOSU_ERR_BACKEND
                }
            }),
        ));
        let plugin = FakePlugin::new();
        relay.set_target(plugin.target());
        let destroyed = Arc::new(Mutex::new(false));
        let mut threads = vec![std::thread::spawn({
            let relay = Arc::clone(&relay);
            let destroyed = Arc::clone(&destroyed);
            move || {
                relay.read(receiver, || *destroyed.lock().unwrap() = true);
            }
        })];
        for _ in 0..workers {
            let relay = Arc::clone(&relay);
            threads.push(std::thread::spawn(move || relay.work()));
        }
        Self {
            relay,
            plugin,
            fatal,
            destroyed,
            threads,
        }
    }

    /// `send` on a plugin thread of its own, as a plugin's worker would.
    fn send_async(
        &self,
        message: &Message,
        correlation: u64,
    ) -> JoinHandle<(vosu_status, std::thread::ThreadId)> {
        let relay = Arc::clone(&self.relay);
        let opcode = message.opcode() as u32;
        let flags = message.flags();
        let payload = message.encode_payload().unwrap();
        std::thread::spawn(move || {
            let status = relay.send(opcode, flags, correlation, &payload);
            (status, std::thread::current().id())
        })
    }

    fn send(&self, message: &Message, correlation: u64) -> vosu_status {
        self.send_async(message, correlation).join().unwrap().0
    }

    fn stop(self) {
        let _ = self.relay.closer().close();
        for thread in self.threads {
            thread.join().unwrap();
        }
    }
}

type Frames = Arc<Mutex<Vec<(FrameHeader, Vec<u8>)>>>;

/// Plays core's reader: records every frame and answers each acknowledged
/// one with `answer`'s status (`None`: never answers).
fn respond(
    core: Endpoint,
    answer: impl Fn(&FrameHeader, &[u8]) -> Option<vosu_status> + Send + 'static,
) -> (
    Frames,
    super::super::channel::Sender,
    Arc<Ledger>,
    JoinHandle<()>,
) {
    let Endpoint {
        sender,
        mut receiver,
        ledger,
        ..
    } = core;
    let frames: Frames = Arc::default();
    let thread = std::thread::spawn({
        let frames = Arc::clone(&frames);
        let sender = sender.clone();
        let ledger = Arc::clone(&ledger);
        move || {
            let mut buffer = PayloadBuffer::new();
            let mut acknowledge = None;
            while let Ok(header) = receiver.recv(&mut buffer) {
                if header.opcode == control::ACK_NEXT {
                    acknowledge = Some(header.correlation);
                    continue;
                }
                frames
                    .lock()
                    .unwrap()
                    .push((header, buffer.bytes().to_vec()));
                if let Some(request) = acknowledge.take()
                    && let Some(status) = answer(&header, buffer.bytes())
                {
                    ledger.close(request);
                    let _ = sender.send(&status_frame(request), &(status as u32).to_le_bytes());
                }
            }
        }
    });
    (frames, sender, ledger, thread)
}

fn status_frame(request: u64) -> FrameHeader {
    FrameHeader {
        opcode: control::STATUS,
        flags: 0,
        correlation: request,
    }
}

fn frame(message: &Message) -> (FrameHeader, Vec<u8>) {
    (
        FrameHeader {
            opcode: message.opcode() as u32,
            flags: message.flags(),
            correlation: 0,
        },
        message.encode_payload().unwrap(),
    )
}

fn focus(handle: u64) -> Message {
    Message::FocusChanged(FocusChanged {
        handle,
        focused: true,
        foreground: None,
    })
}

fn submit(plan: u64, sync: bool) -> Message {
    Message::ReadPlanSubmit(ReadPlanSubmit {
        plan,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![PlanOp {
                offset: 0,
                length: 8,
            }],
        }],
        sync,
    })
}

fn plan_result(plan: u64) -> Message {
    Message::ReadPlanResult(ReadPlanResult {
        plan,
        results: vec![ReadResult::Ok(plan.to_le_bytes().to_vec())],
    })
}

fn invoke(invocation: u64, args: usize) -> Message {
    Message::ActionInvoke(ActionInvoke {
        invocation,
        action: "acme.fixture.run".into(),
        session: None,
        caller: Caller {
            kind: CallerKind::Tool,
            id: "tool".into(),
        },
        args: CborValue::Bytes(vec![invocation as u8; args]),
    })
}

fn timer_set(timer: u64) -> Message {
    Message::TimerSet(TimerSet {
        timer,
        interval_ms: Some(1_000),
    })
}

fn wait_until(bound: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + bound;
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::yield_now();
    }
    true
}

/// T1 counterexample: ordinary dispatch is blocked while another thread's
/// SYNC result is ready. The result reaches its submitting thread (and is
/// dispatched there) without waiting for the blocked dispatch.
#[test]
fn relay_sync_result_reaches_its_submitter_while_dispatch_is_blocked() {
    let (mut core, child) = pair();
    let child = Child::start(child, 1);
    let gate = Gate::new();
    child.plugin.set_hook({
        let gate = Arc::clone(&gate);
        move |call| {
            if call.opcode == vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32 {
                gate.wait();
            }
        }
    });
    let (header, payload) = frame(&focus(7));
    core.sender.send(&header, &payload).unwrap();
    assert!(gate.wait_arrivals(1, BOUND), "dispatch is blocked in FOCUS");

    let submitter = child.send_async(&submit(11, true), 0);
    let mut buffer = PayloadBuffer::new();
    let mut recv = || {
        core.receiver
            .recv_until(&mut buffer, Some(Instant::now() + BOUND))
            .unwrap()
            .unwrap()
    };
    let ack = recv();
    assert_eq!(ack.opcode, control::ACK_NEXT);
    let request = ack.correlation;
    let submitted = recv();
    assert_eq!(
        submitted.opcode,
        vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT as u32
    );
    assert_eq!(submitted.flags, vosu_plugin_api::VOSU_READ_PLAN_SYNC);
    assert_eq!(core.ledger.kind(request), Some(RequestKind::Sync));
    core.ledger.close(request);
    let (result, payload) = frame(&plan_result(11));
    core.sender.send_reply(request, &result, &payload).unwrap();

    let (status, thread) = submitter.join().unwrap();
    assert_eq!(status, vosu_status::VOSU_OK);
    let calls = child.plugin.calls();
    assert_eq!(
        calls.len(),
        1,
        "FOCUS is still blocked; only the result ran"
    );
    assert_eq!(
        calls[0].opcode,
        vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32
    );
    assert_eq!(
        calls[0].thread, thread,
        "dispatched on the submitting thread"
    );
    assert_eq!(calls[0].payload, payload, "relayed byte for byte");
    assert!(calls[0].aligned);
    assert_eq!(child.relay.waiters().reserved(), 0);

    gate.open();
    assert_eq!(child.plugin.wait_calls(2, BOUND).len(), 2);
    child.stop();
}

/// §6.3: a SYNC submission from a dispatch thread is refused locally, as
/// in-process, with nothing written.
#[test]
fn relay_refuses_a_sync_submission_from_a_dispatch_thread() {
    let (mut core, child) = pair();
    let child = Child::start(child, 1);
    let refused = Arc::new(Mutex::new(None));
    child.plugin.set_hook({
        let relay = Arc::clone(&child.relay);
        let refused = Arc::clone(&refused);
        move |call| {
            if call.opcode == vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32 {
                let message = submit(1, true);
                *refused.lock().unwrap() = Some(relay.send(
                    message.opcode() as u32,
                    message.flags(),
                    0,
                    &message.encode_payload().unwrap(),
                ));
            }
        }
    });
    let (header, payload) = frame(&focus(1));
    core.sender.send(&header, &payload).unwrap();
    assert_eq!(child.plugin.wait_calls(1, BOUND).len(), 1);
    assert_eq!(
        *refused.lock().unwrap(),
        Some(vosu_status::VOSU_ERR_INVALID_ARGUMENT)
    );
    let mut buffer = PayloadBuffer::new();
    assert_eq!(
        core.receiver
            .recv_until(
                &mut buffer,
                Some(Instant::now() + Duration::from_millis(100))
            )
            .unwrap(),
        None,
        "nothing reached the ring"
    );
    child.stop();
}

/// T1 counterexample: the 65th asynchronous plan never reaches the ring;
/// a result frees its slot before the plugin sees it.
#[test]
fn relay_the_sixty_fifth_async_plan_never_reaches_the_ring() {
    let (core, child) = pair();
    let child = Child::start(child, 1);
    let (frames, sender, _ledger, responder) = respond(core, |_, _| Some(vosu_status::VOSU_OK));
    for plan in 0..64 {
        assert_eq!(child.send(&submit(plan, false), 0), vosu_status::VOSU_OK);
    }
    assert_eq!(
        child.send(&submit(64, false), 0),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    let submits = |frames: &Frames| {
        frames
            .lock()
            .unwrap()
            .iter()
            .filter(|(header, _)| header.opcode == vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT as u32)
            .count()
    };
    assert_eq!(
        submits(&frames),
        64,
        "the 65th was refused before any write"
    );
    let (header, payload) = frame(&plan_result(3));
    sender.send(&header, &payload).unwrap();
    assert_eq!(child.plugin.wait_calls(1, BOUND).len(), 1);
    assert_eq!(child.send(&submit(64, false), 0), vosu_status::VOSU_OK);
    assert_eq!(submits(&frames), 65);
    child.stop();
    drop(responder);
}

/// T1 counterexample: a send core refuses produces no later result and
/// leaves no slot behind; every attempt still reaches core.
#[test]
fn relay_a_refused_send_releases_its_slot_and_produces_no_result() {
    let (core, child) = pair();
    let child = Child::start(child, 1);
    let (frames, _sender, _ledger, responder) =
        respond(core, |_, _| Some(vosu_status::VOSU_ERR_TARGET_GONE));
    for correlation in 0..100 {
        let scan = Message::ScanRequest(ScanRequest {
            session: 3,
            module: None,
            pattern: "AA".into(),
            non_zero_mask: false,
            all_matches: false,
        });
        assert_eq!(
            child.send(&scan, correlation),
            vosu_status::VOSU_ERR_TARGET_GONE,
            "core's own status, never a fabricated success"
        );
    }
    assert_eq!(frames.lock().unwrap().len(), 100);
    assert_eq!(child.relay.admission().counts(), (0, 0, 0, 0));
    assert!(child.plugin.calls().is_empty());
    child.stop();
    drop(responder);
}

/// The ring reader never blocks on delivery: with dispatch blocked, core
/// writes far more than a ring's capacity, a status round trip still
/// completes, and every frame is delivered in order afterwards.
#[test]
fn relay_a_blocked_dispatch_never_blocks_the_ring_reader() {
    let (core, child) = pair();
    let child = Child::start(child, 1);
    let gate = Gate::new();
    child.plugin.set_hook({
        let gate = Arc::clone(&gate);
        move |call| {
            if call.opcode == vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32 {
                gate.wait();
            }
        }
    });
    let (_frames, sender, _ledger, responder) = respond(core, |_, _| Some(vosu_status::VOSU_OK));
    let (header, payload) = frame(&focus(1));
    sender.send(&header, &payload).unwrap();
    assert!(gate.wait_arrivals(1, BOUND));
    // 600 × ~8 KiB: more than twice the 2 MiB ring, never parked for long.
    let started = Instant::now();
    for invocation in 0..600 {
        let (header, payload) = frame(&invoke(invocation, 8 * 1024));
        sender.send(&header, &payload).unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(child.send(&timer_set(1), 0), vosu_status::VOSU_OK);
    assert!(child.relay.delivery().bytes().1 > 4 * 1024 * 1024);

    gate.open();
    let calls = child.plugin.wait_calls(601, BOUND);
    assert_eq!(calls.len(), 601);
    for (index, call) in calls[1..].iter().enumerate() {
        let message =
            Message::decode(vosu_opcode::VOSU_OP_ACTION_INVOKE, 0, &call.payload).unwrap();
        assert_eq!(message, invoke(index as u64, 8 * 1024));
    }
    child.stop();
    drop(responder);
}

/// `concurrent_dispatch`: the worker pool runs two deliveries at once.
#[test]
fn relay_concurrent_dispatch_runs_deliveries_in_parallel() {
    let (mut core, child) = pair();
    let child = Child::start(child, CONCURRENT_WORKERS);
    let both = Gate::new();
    let overlapped = Arc::new(Mutex::new(Vec::new()));
    child.plugin.set_hook({
        let both = Arc::clone(&both);
        let overlapped = Arc::clone(&overlapped);
        move |call| {
            if call.opcode == vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32 {
                both.arrive();
                overlapped
                    .lock()
                    .unwrap()
                    .push(both.wait_arrivals(2, BOUND));
            }
        }
    });
    for handle in [1, 2] {
        let (header, payload) = frame(&focus(handle));
        core.sender.send(&header, &payload).unwrap();
    }
    assert_eq!(child.plugin.wait_calls(2, BOUND).len(), 2);
    assert_eq!(*overlapped.lock().unwrap(), vec![true, true]);
    let mut buffer = PayloadBuffer::new();
    assert!(
        core.receiver
            .recv_until(&mut buffer, Some(Instant::now()))
            .unwrap()
            .is_none()
    );
    child.stop();
}

/// §7.6 backstops: a status nobody answers ends the channel after 2 s; the
/// child never fabricates a status.
#[test]
fn relay_an_unanswered_waiter_ends_the_channel_at_its_backstop() {
    let (core, child) = pair();
    let child = Child::start(child, 1);
    let (_frames, _sender, _ledger, responder) = respond(core, |_, _| None);
    let started = Instant::now();
    let status = child.send(&timer_set(2), 0);
    let elapsed = started.elapsed();
    assert!(elapsed >= STATUS_BACKSTOP && elapsed < BOUND, "{elapsed:?}");
    assert_eq!(*child.fatal.lock().unwrap(), vec![EXIT_TRANSPORT_TIMEOUT]);
    assert_eq!(
        status,
        vosu_status::VOSU_ERR_BACKEND,
        "the fatal hook's own value"
    );
    assert_eq!(
        child.relay.closer().termination(),
        Some(Termination::Closed)
    );
    child.stop();
    drop(responder);
}

/// A STATE_SUBSCRIBE core accepted clears the updates for that session
/// not yet dispatched, as core clears its own pending ones; the new
/// selection's updates follow.
#[test]
fn relay_an_accepted_resubscription_clears_undelivered_updates() {
    let (core, child) = pair();
    let child = Child::start(child, 1);
    let gate = Gate::new();
    child.plugin.set_hook({
        let gate = Arc::clone(&gate);
        move |call| {
            if call.opcode == vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32 {
                gate.wait();
            }
        }
    });
    let (_frames, sender, _ledger, responder) = respond(core, |_, _| Some(vosu_status::VOSU_OK));
    let entry = |pointer: &str| StateEntry {
        ns: "acme.other".into(),
        pointer: pointer.into(),
        value: Some(CborValue::Integer(1.into())),
        error: None,
    };
    let update = |pointer: &str| {
        Message::StateUpdate(StateUpdate {
            session: 5,
            values: vec![entry(pointer)],
            more: false,
        })
    };
    let (header, payload) = frame(&focus(1));
    sender.send(&header, &payload).unwrap();
    assert!(gate.wait_arrivals(1, BOUND));
    let (header, payload) = frame(&update("/old"));
    sender.send(&header, &payload).unwrap();
    let subscribe = Message::StateSubscribe(StateSubscribe {
        session: Some(5),
        selectors: vec![Selector {
            ns: "acme.other".into(),
            pointer: "/new".into(),
        }],
    });
    assert!(wait_until(BOUND, || child.relay.delivery().bytes().0 > 0));
    assert_eq!(child.send(&subscribe, 0), vosu_status::VOSU_OK);
    let (header, payload) = frame(&update("/new"));
    sender.send(&header, &payload).unwrap();
    gate.open();
    let calls = child.plugin.wait_calls(2, BOUND);
    assert_eq!(calls.len(), 2);
    assert_eq!(
        Message::decode(vosu_opcode::VOSU_OP_STATE_UPDATE, 0, &calls[1].payload).unwrap(),
        update("/new")
    );
    assert!(child.plugin.wait_calls(3, Duration::from_millis(100)).len() == 2);
    child.stop();
    drop(responder);
}

/// A reply naming an id nobody waits on is discarded and counted; the
/// channel stays healthy.
#[test]
fn relay_a_reply_for_an_unknown_id_is_discarded() {
    let (core, child) = pair();
    let child = Child::start(child, 1);
    let (_frames, sender, _ledger, responder) = respond(core, |_, _| Some(vosu_status::VOSU_OK));
    sender
        .send(
            &status_frame(4_000),
            &(vosu_status::VOSU_OK as u32).to_le_bytes(),
        )
        .unwrap();
    let (result, payload) = frame(&plan_result(9));
    sender.send_reply(4_001, &result, &payload).unwrap();
    assert!(wait_until(BOUND, || child.relay.waiters().discarded() == 2));
    assert_eq!(child.send(&timer_set(3), 0), vosu_status::VOSU_OK);
    assert!(
        child.plugin.calls().is_empty(),
        "the late result reached nobody"
    );
    child.stop();
    drop(responder);
}

/// `DESTROY`: what was queued is still delivered, then the workers end.
#[test]
fn relay_destroy_drains_queued_deliveries_then_stops_the_workers() {
    let (mut core, child) = pair();
    let Child {
        relay,
        plugin,
        fatal: _fatal,
        destroyed,
        threads,
    } = Child::start(child, 1);
    let gate = Gate::new();
    plugin.set_hook({
        let gate = Arc::clone(&gate);
        move |call| {
            if call.opcode == vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32 {
                gate.wait();
            }
        }
    });
    let (header, payload) = frame(&focus(1));
    core.sender.send(&header, &payload).unwrap();
    assert!(gate.wait_arrivals(1, BOUND));
    for invocation in 0..3 {
        let (header, payload) = frame(&invoke(invocation, 16));
        core.sender.send(&header, &payload).unwrap();
    }
    let destroy = FrameHeader {
        opcode: control::DESTROY,
        flags: 0,
        correlation: 0,
    };
    core.sender.send(&destroy, &[]).unwrap();
    assert!(wait_until(BOUND, || *destroyed.lock().unwrap()));
    gate.open();
    let mut threads = threads.into_iter();
    let reader = threads.next().unwrap();
    for worker in threads {
        worker.join().unwrap();
    }
    assert_eq!(plugin.calls().len(), 4);
    let _ = relay.closer().close();
    reader.join().unwrap();
    let mut buffer = PayloadBuffer::new();
    let _ = core.receiver.recv_until(&mut buffer, Some(Instant::now()));
}
