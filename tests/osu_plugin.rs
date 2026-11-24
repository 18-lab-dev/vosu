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
    let started = Instant::now();
    let harness = Arc::new(Harness::new("test.osu.p6.stress", 0x6a00_0200, 32));
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
    assert!(
        FOCUS_EVENTS.load(Ordering::Relaxed) > 32,
        "foreground rotations delivered no focus changes"
    );
    assert!(started.elapsed() < Duration::from_secs(45));
}
