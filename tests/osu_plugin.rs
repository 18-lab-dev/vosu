//! Real osu! ConcurrentPlugin over the host, B1 and injected process memory.
#![cfg(feature = "test-harness")]
#[path = "osu_plugin/support.rs"]
mod support;

use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use support::*;
use vosu_osu::bridge_b1::B1Phase;

#[test]
fn real_manifest_registers_its_namespace_schema() {
    let harness = Harness::new("vosu.osu", 0x6a00_0800, 1);
    harness
        .host
        .process_appeared(Harness::record(harness.pids[0], &[]));
    harness
        .sink
        .until(WAIT, |events| !events.published.is_empty());
    assert_dispatches_ok();
}

#[test]
fn stable_session_b1_settings_and_exit() {
    let harness = Harness::new("test.osu.p6.stable", 0x6a00_0000, 1);
    let pid = harness.pids[0];
    harness.host.process_appeared(Harness::record(pid, &[]));
    harness.sink.until(WAIT, |events| {
        !events.published.is_empty()
            && events
                .phases
                .values()
                .any(|phases| phases.contains(&B1Phase::Reading))
    });
    let key = *harness
        .sink
        .events
        .lock()
        .unwrap()
        .published
        .keys()
        .next()
        .unwrap();
    {
        let events = harness.sink.events.lock().unwrap();
        assert!(events.attached.contains(&key));
        let phases = &events.phases[&key];
        assert_eq!(phases.first(), Some(&B1Phase::ResolvingOffsets));
        assert!(phases.contains(&B1Phase::Reading));
        let order = &events.order[&key];
        assert!(order.attached.unwrap() < order.resolving.unwrap());
        assert!(order.resolving.unwrap() < order.first_publish.unwrap());
        assert!(order.first_publish.unwrap() < order.reading.unwrap());
        assert!(
            order.starting.is_none(),
            "stable has no StartingReader phase"
        );
    }
    assert!(harness.memory.reads.load(Ordering::Relaxed) > 0);
    harness.dispatch(&harness.apply(273.0));
    eventually(|| {
        vosu_osu::test_support::session_settings(pid)
            .is_some_and(|settings| settings.poll_rate == 273.0)
    });
    harness.host.process_exited(pid);
    harness
        .sink
        .until(WAIT, |events| events.detached.contains(&key));
    eventually(|| vosu_osu::test_support::live_sessions() == 0);
    harness.host.destroy_all(WAIT);
    let events = harness.sink.events.lock().unwrap();
    assert!(events.attached.is_empty());
    assert!(!events.late_publish);
    assert_dispatches_ok();
}

#[test]
fn tournament_and_debug_clients_decline_without_b1() {
    let harness = Harness::new("test.osu.p6.decline", 0x6a00_0100, 2);
    harness
        .host
        .process_appeared(Harness::record(harness.pids[0], &["--tournament"]));
    harness
        .host
        .process_appeared(Harness::record(harness.pids[1], &["--debug-client-id=1"]));
    eventually(|| DECLINES.lock().unwrap().len() == 2);
    assert!(
        DECLINES
            .lock()
            .unwrap()
            .iter()
            .all(|reason| *reason == vosu_plugin_api::DetachReason::Declined)
    );
    harness.host.destroy_all(WAIT);
    let events = harness.sink.events.lock().unwrap();
    assert!(events.attached.is_empty());
    assert!(events.phases.is_empty());
    assert!(events.published.is_empty());
    assert!(events.detached.is_empty());
    assert_eq!(vosu_osu::test_support::live_sessions(), 0);
    assert_dispatches_ok();
}

#[test]
fn concurrent_plugin_stress_four_workers_eight_threads() {
    let harness = Arc::new(Harness::new("test.osu.p6.stress", 0x6a00_0200, 32));
    let started = Instant::now();
    for &pid in &harness.pids {
        harness.host.process_appeared(Harness::record(pid, &[]));
    }
    harness
        .sink
        .until(WAIT, |events| events.published.len() == 32);
    eventually(|| {
        harness
            .memories
            .iter()
            .all(|memory| memory.plans.load(Ordering::Relaxed) > 0)
    });
    let reads_before: Vec<_> = harness
        .memories
        .iter()
        .map(|memory| memory.plans.load(Ordering::Relaxed))
        .collect();
    // The anchor sessions never detach. After setup, distinct anchor gains
    // can therefore only come from the producers rotating foreground.
    let anchor_handles: Vec<_> = {
        let handles = HANDLES.lock().unwrap();
        harness.pids[..16].iter().map(|pid| handles[pid]).collect()
    };
    let focus_before = FOCUS_GAINS.lock().unwrap().clone();
    let (finished, completions) = mpsc::sync_channel(8);
    for worker in 0..8 {
        let harness = harness.clone();
        let finished = finished.clone();
        std::thread::spawn(move || {
            for iteration in 0..500 {
                // Half the pids stay live as read/focus anchors. Every producer
                // races the same other half, including immediate start-up exits.
                let pid = harness.pids[16 + (iteration * 13 + worker * 7) % 16];
                harness.host.process_appeared(Harness::record(pid, &[]));
                if iteration % 3 == 0 {
                    harness.host.process_exited(pid);
                }
                harness
                    .foreground
                    .set(Some(harness.pids[(iteration + worker) % 16]));
                harness.host.foreground_changed();
                harness.dispatch(&harness.apply(100.0 + (iteration % 4) as f64));
                if iteration % 3 != 0 {
                    harness.host.process_exited(pid);
                }
            }
            finished.send(()).unwrap();
        });
    }
    drop(finished);
    let deadline = started + Duration::from_secs(30);
    for _ in 0..8 {
        completions
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("stress worker completion");
    }
    let rotated_anchors = {
        let gains = FOCUS_GAINS.lock().unwrap();
        anchor_handles
            .iter()
            .filter(|&&handle| {
                gains.get(&handle).copied().unwrap_or(0)
                    > focus_before.get(&handle).copied().unwrap_or(0)
            })
            .count()
    };
    assert!(
        rotated_anchors > 1,
        "foreground rotation must focus multiple live anchors during the producer loop"
    );
    for &pid in &harness.pids[16..] {
        harness.host.process_appeared(Harness::record(pid, &[]));
    }
    eventually(|| {
        harness
            .memories
            .iter()
            .zip(&reads_before)
            .all(|(memory, before)| memory.plans.load(Ordering::Relaxed) > *before)
    });
    assert!(
        harness
            .memories
            .iter()
            .zip(reads_before)
            .all(|(memory, before)| memory.plans.load(Ordering::Relaxed) > before)
    );
    for &pid in &harness.pids {
        harness.host.process_exited(pid);
    }
    harness
        .sink
        .until(WAIT, |events| events.attached.is_empty());
    eventually(|| vosu_osu::test_support::live_sessions() == 0);
    let destroy = Instant::now();
    harness.host.destroy_all(WAIT);
    assert!(destroy.elapsed() < WAIT, "destroy exceeded ABI grace");
    assert!(!harness.sink.events.lock().unwrap().late_publish);
    assert_dispatches_ok();
    assert!(
        PEAK.load(Ordering::Relaxed) > 1,
        "dispatches never overlapped"
    );
    assert!(started.elapsed() < Duration::from_secs(45));
}

fn namespace(harness: &Harness, handle: u64, ns: &str) -> Option<serde_json::Value> {
    use vosu_core::state::{Audience, Pointer, SessionKey, project_select};
    let state = harness.host.state().store.current(SessionKey(handle))?;
    let registry = harness.host.state().registry.read().unwrap();
    project_select(
        &registry,
        &state,
        ns,
        &Pointer::parse("").unwrap(),
        Audience::Api,
    )
    .map(|value| {
        let mut bytes = Vec::new();
        value.write_json(&mut bytes);
        serde_json::from_slice(&bytes).unwrap()
    })
}

fn session_handle(harness: &Harness) -> u64 {
    eventually(|| HANDLES.lock().unwrap().contains_key(&harness.pids[0]));
    HANDLES.lock().unwrap()[&harness.pids[0]]
}

#[test]
fn stable_fixture_session_publishes_every_namespace_within_5s() {
    let harness = Harness::new("vosu.osu", 0x6a00_0900, 1);
    let started = Instant::now();
    harness
        .host
        .process_appeared(Harness::record(harness.pids[0], &[]));
    let handle = session_handle(&harness);
    for ns in [
        "vosu.client",
        "vosu.status",
        "vosu.chart",
        "vosu.play",
        "vosu.skin",
        "vosu.osu",
    ] {
        eventually(|| namespace(&harness, handle, ns).is_some());
        assert!(namespace(&harness, handle, ns).unwrap().is_object(), "{ns}");
    }
    assert!(
        started.elapsed() < WAIT,
        "all namespaces must publish within five seconds"
    );
    let osu = namespace(&harness, handle, "vosu.osu").unwrap();
    assert!(osu.get("session").is_some());
    assert!(osu.get("play").is_some());
    // Full JSON Schema validation is X1's dev-dependency gate.
    assert_dispatches_ok();
}

#[test]
fn no_b1_sink_still_publishes_to_bus() {
    let mut harness = Harness::new("vosu.osu", 0x6a00_0a00, 1);
    harness.remove_b1();
    harness
        .host
        .process_appeared(Harness::record(harness.pids[0], &[]));
    let handle = session_handle(&harness);
    eventually(|| namespace(&harness, handle, "vosu.osu").is_some());
    assert!(harness.sink.events.lock().unwrap().published.is_empty());
    assert!(harness.memory.reads.load(Ordering::Relaxed) > 0);
    assert_dispatches_ok();
}

#[test]
fn bus_demand_unions_with_hub_demand() {
    use vosu_plugin_api::Selector;
    use vosu_plugin_testkit::subscriber::{SubscribeCommand, SubscriberEvent};
    let harness = Harness::new("vosu.osu", 0x6a00_0b00, 1);
    let (events, commands) = harness.subscriber("test.osu.o6.demand");
    commands
        .try_send(SubscribeCommand {
            session: None,
            selectors: vec![Selector {
                ns: "vosu.sessions".into(),
                pointer: String::new(),
            }],
        })
        .unwrap();
    harness
        .host
        .process_appeared(Harness::record(harness.pids[0], &[]));
    let deadline = Instant::now() + WAIT;
    let handle = loop {
        if let SubscriberEvent::Attached(attached) = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
        {
            break attached.handle;
        }
    };
    commands
        .try_send(SubscribeCommand {
            session: Some(handle),
            selectors: vec![Selector {
                ns: "vosu.osu".into(),
                pointer: "/play/hitErrors".into(),
            }],
        })
        .unwrap();
    loop {
        if let SubscriberEvent::SubscribeStatus(status) = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
        {
            assert_eq!(status, vosu_plugin_api::vosu_status::VOSU_OK);
            break;
        }
    }
    let demand = || vosu_osu::test_support::session_demand(harness.pids[0]);
    eventually(|| demand().is_some());
    assert!(
        !demand().unwrap().precise,
        "gameplay lane stays dormant in menu"
    );
    harness
        .sink
        .inputs
        .send_modify(|inputs| inputs.demand.regular = true);
    eventually(|| demand().is_some_and(|demand| demand.regular));
    harness.memory.set_status(2);
    eventually(|| demand().is_some_and(|demand| demand.precise && demand.regular));
    harness.memory.set_status(0);
    eventually(|| demand().is_some_and(|demand| !demand.precise && demand.regular));
    assert_dispatches_ok();
}

#[cfg(windows)]
#[test]
fn focus_foreground_sets_session_focused() {
    let harness = Harness::new("vosu.osu", 0x6a00_0c00, 1);
    let pid = harness.pids[0];
    harness.host.process_appeared(Harness::record(pid, &[]));
    let handle = session_handle(&harness);
    eventually(|| namespace(&harness, handle, "vosu.osu").is_some());
    harness.foreground.set(Some(pid));
    harness.host.foreground_changed();
    eventually(|| {
        namespace(&harness, handle, "vosu.osu")
            .is_some_and(|value| value["session"]["focused"] == true)
    });
    harness.foreground.set(None);
    harness.host.foreground_changed();
    eventually(|| {
        namespace(&harness, handle, "vosu.osu")
            .is_some_and(|value| value["session"]["focused"] == false)
    });
    assert_dispatches_ok();
}
