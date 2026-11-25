//! M5 X0: ring pressure that must not become a loss
//! (`docs/v2/plugin-abi.md` §7.2 "Pressure, loss and restart", §7.6 "Child
//! delivery lanes"; M5 plan §7 "Bounded pressure").
//!
//! C0's `crash::crash_a_ring_stall_that_fills_the_ring_is_one_pressure_timeout`
//! is the positive case: a stopped child whose ring fills loses its channel
//! after the 2-second window. These are its two negative controls, on a
//! real `vosu plugin-host` child running X0's C fixture
//! (`sync/fixture.rs`):
//!
//! - a healthy idle child, sending and receiving nothing for well past the
//!   window, is never judged lost: silence is not a heartbeat failure;
//! - a child whose ordinary dispatch is blocked keeps draining its
//!   core → child ring into its delivery queue, so core writes more legal
//!   bytes than the ring holds without ever waiting out the window, and a
//!   worker thread's `SYNC` read still completes on its own thread before
//!   dispatch is released. Every queued frame is then dispatched, in order,
//!   byte for byte.

use std::time::{Duration, Instant};

use vosu_core::plugin::PluginInstanceId;
use vosu_core::plugin::process::supervisor::ProcessPhase;
use vosu_plugin_api::{
    ActionInvoke, Caller, CallerKind, CborValue, Message, vosu_opcode, vosu_status,
};

use super::support::host::{Identity, wait_until};
use super::sync::fixture::{self, Command};
use super::sync::{BOUND, MARKER, Scenario};

/// The loss window a legal full-ring write may wait (§7.2).
const WINDOW: Duration = Duration::from_secs(2);
/// The core → child ring's capacity (§7.2: 2 MiB per direction).
const RING: usize = 2 * 1024 * 1024;
/// Each flood frame's argument bytes.
const FRAME_ARGS: usize = 768 * 1024;
const FRAMES: u64 = 6;

/// Asserts instance 1 has been running, unharmed, for every check of
/// `period`, and that its child is still `identity`.
fn assert_healthy_for(scenario: &Scenario, identity: &Identity, period: Duration) {
    let until = Instant::now() + period;
    let mut checks = 0;
    loop {
        let status = scenario
            .host
            .host
            .process_status(PluginInstanceId::new(1))
            .expect("a supervised process instance");
        assert_eq!(
            (
                status.phase,
                status.restarts,
                status.crash_events,
                status.last_failure.clone()
            ),
            (ProcessPhase::Running, 0, 0, None),
            "the healthy child was judged lost after {checks} checks"
        );
        assert!(identity.alive(), "the healthy child died");
        checks += 1;
        if Instant::now() >= until {
            return;
        }
        // Time passing is what this observes: there is nothing to wait on.
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A child that is idle for 2.5 windows stays `Running` and unlost, and
/// then serves a SYNC read and its blocked-then-released dispatch.
#[test]
fn backpressure_an_idle_child_stays_healthy_past_the_pressure_window() {
    let scenario = Scenario::new("idle", "process", 0x7FF0_5301);
    let (child, _) = scenario.wait("attached");
    scenario.assert_ran_in_runtime(child);
    let identity = Identity::of(child);
    let idle = WINDOW * 5 / 2;
    assert_healthy_for(&scenario, &identity, idle);
    println!("RECORD backpressure idle child healthy for {idle:?}");

    scenario.command(Command::Hold);
    scenario.open("go");
    let (pid, sync) = scenario.wait("sync");
    assert_eq!(pid, child, "the idle child was replaced");
    assert_eq!(
        fixture::field(&sync, "status"),
        (vosu_status::VOSU_OK as u32).to_string()
    );
    assert_eq!(fixture::field(&sync, "value"), format!("{MARKER:016x}"));
    scenario.open("release");
    scenario.wait("unblocked");
}

fn flood(invocation: u64) -> Message {
    Message::ActionInvoke(ActionInvoke {
        invocation,
        action: "test.x0.blocked.flood".to_owned(),
        session: None,
        caller: Caller {
            kind: CallerKind::Tool,
            id: "x0-flood".to_owned(),
        },
        args: CborValue::Bytes(vec![(invocation % 251) as u8; FRAME_ARGS]),
    })
}

/// With ordinary dispatch blocked, core writes more than twice the ring's
/// capacity of legal frames to the child, none waiting out the window; a
/// worker's SYNC read then completes while dispatch is still blocked; the
/// child stays healthy past the window and finally dispatches every frame
/// in order.
#[test]
fn backpressure_a_blocked_dispatch_drains_the_ring_and_a_sync_read_still_completes() {
    let scenario = Scenario::new("blocked", "process", 0x7FF0_5302);
    scenario.command(Command::Hold);
    let (child, _) = scenario.wait("dispatch");
    scenario.assert_ran_in_runtime(child);
    let identity = Identity::of(child);

    // Real frames through the real ring while the dispatch is held.
    let messages: Vec<Message> = (1..=FRAMES).map(|n| 0x0A00 + n).map(flood).collect();
    let started = Instant::now();
    let writes: Vec<_> = {
        let _enter = scenario.host.enter();
        messages
            .iter()
            .map(|message| {
                scenario
                    .host
                    .host
                    .dispatch_to_instance(PluginInstanceId::new(1), message, 0)
                    .expect("the blocked instance is still live")
            })
            .collect()
    };
    for write in writes {
        assert_eq!(
            write.recv_timeout(BOUND).unwrap(),
            vosu_status::VOSU_OK,
            "a legal frame was refused"
        );
    }
    let written = started.elapsed();
    let bytes = FRAMES as usize * FRAME_ARGS;
    println!("RECORD backpressure {bytes} argument bytes written in {written:?}");
    assert!(bytes > 2 * RING, "the flood does not exceed the ring twice");
    assert!(
        written < WINDOW,
        "core waited {written:?} for ring space although the child drains it"
    );
    assert!(fixture::marker(&scenario.markers, "unblocked").is_none());

    // Past the window with the frames queued in the child: no loss.
    assert_healthy_for(&scenario, &identity, WINDOW + WINDOW / 4);

    // A SYNC read from a worker completes while dispatch is still blocked.
    scenario.open("go");
    let (_, sync) = scenario.wait("sync");
    assert_eq!(
        fixture::field(&sync, "status"),
        (vosu_status::VOSU_OK as u32).to_string()
    );
    assert_eq!(fixture::field(&sync, "value"), format!("{MARKER:016x}"));
    assert_eq!(
        fixture::field(&sync, "released"),
        "0",
        "the SYNC read waited for the blocked dispatch"
    );
    assert_eq!(
        fixture::field(&sync, "result_tid"),
        fixture::field(&sync, "submit_tid")
    );

    scenario.open("release");
    scenario.wait("unblocked");
    let opcode = vosu_opcode::VOSU_OP_ACTION_INVOKE as u32;
    assert!(
        wait_until(BOUND, || fixture::log(&scenario.markers, child)
            .iter()
            .filter(|(logged, _)| *logged == opcode)
            .count()
            == FRAMES as usize),
        "the queued frames were not all dispatched"
    );
    let delivered: Vec<Message> = fixture::log(&scenario.markers, child)
        .into_iter()
        .filter(|(logged, _)| *logged == opcode)
        .map(|(_, payload)| {
            Message::decode(vosu_opcode::VOSU_OP_ACTION_INVOKE, 0, &payload).unwrap()
        })
        .collect();
    assert_eq!(delivered, messages, "queued frames changed or reordered");
    assert_healthy_for(&scenario, &identity, Duration::ZERO);
}
