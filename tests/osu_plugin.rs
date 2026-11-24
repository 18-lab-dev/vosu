//! Real osu! ConcurrentPlugin over the ABI host, bus and injected process memory.
#![cfg(feature = "test-harness")]
#[path = "osu_plugin/support.rs"]
mod support;
#[path = "osu_plugin/tournament.rs"]
mod tournament;

use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use support::*;

#[test]
fn real_manifest_registers_its_namespace_schema() {
    let harness = Harness::new("vosu.osu", 0x6a00_0800, 1);
    harness
        .host
        .process_appeared(Harness::record(harness.pids[0], &[]));
    let handle = session_handle(&harness);
    eventually(|| namespace(&harness, handle, "vosu.osu").is_some());
    assert_dispatches_ok();
}

#[test]
fn stable_session_settings_and_exit() {
    let harness = Harness::new("vosu.osu", 0x6a00_0000, 1);
    let pid = harness.pids[0];
    harness.host.process_appeared(Harness::record(pid, &[]));
    let handle = session_handle(&harness);
    eventually(|| namespace(&harness, handle, "vosu.osu").is_some());
    assert!(
        harness
            .host
            .state()
            .store
            .current(vosu_core::state::SessionKey(handle))
            .is_some()
    );
    assert!(harness.memory.reads.load(Ordering::Relaxed) > 0);
    harness.dispatch(&harness.apply(273.0));
    eventually(|| {
        vosu_osu::test_support::session_settings(pid)
            .is_some_and(|settings| settings.poll_rate == 273.0)
    });
    harness.host.process_exited(pid);
    eventually(|| {
        harness
            .host
            .state()
            .store
            .current(vosu_core::state::SessionKey(handle))
            .is_none()
    });
    eventually(|| vosu_osu::test_support::live_sessions() == 0);
    harness.host.destroy_all(WAIT);
    assert_dispatches_ok();
}

#[test]
fn debug_clients_decline_without_publication() {
    let harness = Harness::new("vosu.osu", 0x6a00_0100, 2);
    harness
        .host
        .process_appeared(Harness::record(harness.pids[0], &["--debug-client-id=2"]));
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
    assert!(
        harness
            .host
            .api_state_source()
            .sessions()
            .unwrap()
            .is_empty()
    );
    harness.host.destroy_all(WAIT);
    assert_eq!(vosu_osu::test_support::live_sessions(), 0);
    assert_dispatches_ok();
}

#[test]
fn concurrent_plugin_stress_four_workers_eight_threads() {
    let harness = Arc::new(Harness::new("vosu.osu", 0x6a00_0200, 32));
    let started = Instant::now();
    for &pid in &harness.pids {
        harness.host.process_appeared(Harness::record(pid, &[]));
    }
    eventually(|| HANDLES.lock().unwrap().len() == 32);
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
    eventually(|| vosu_osu::test_support::live_sessions() == 0);
    let destroy = Instant::now();
    assert!(
        harness
            .host
            .api_state_source()
            .sessions()
            .unwrap()
            .is_empty()
    );
    harness.host.destroy_all(WAIT);
    assert!(destroy.elapsed() < WAIT, "destroy exceeded ABI grace");
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
fn capture_publishes_without_root_control_inputs() {
    let harness = Harness::new("vosu.osu", 0x6a00_0a00, 1);
    harness
        .host
        .process_appeared(Harness::record(harness.pids[0], &[]));
    let handle = session_handle(&harness);
    eventually(|| namespace(&harness, handle, "vosu.osu").is_some());
    assert!(harness.memory.reads.load(Ordering::Relaxed) > 0);
    assert_dispatches_ok();
}

#[test]
fn precise_abi_demand_tracks_gameplay_and_releases_on_unsubscribe() {
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
            selectors: vec![
                Selector {
                    ns: "vosu.osu".into(),
                    pointer: "/play/hitErrors".into(),
                },
                Selector {
                    ns: "vosu.osu".into(),
                    pointer: "/session/playTime".into(),
                },
            ],
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
    // The bus-only regular selector is a positive acknowledgement that this
    // subscription's DemandChanged reached the plugin, not just registration.
    eventually(|| demand().is_some_and(|demand| demand.regular));
    assert!(
        !demand().unwrap().precise,
        "gameplay lane stays dormant in menu"
    );
    harness.memory.set_status(2);
    eventually(|| demand().is_some_and(|demand| demand.precise && demand.regular));
    harness.memory.set_status(0);
    eventually(|| demand().is_some_and(|demand| !demand.precise && demand.regular));
    commands
        .try_send(SubscribeCommand {
            session: Some(handle),
            selectors: vec![],
        })
        .unwrap();
    eventually(|| demand().is_some_and(|demand| !demand.precise && !demand.regular));
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

/// Core has an OS-foreground source only on Windows (`docs/v2/plugin-abi.md`
/// §6.2; `docs/v2/adapter-surfaces.md`: "Linux omits this wire field"). A
/// host built like production's reports the fact as absent elsewhere, never
/// as `false`: in `FOCUS_CHANGED`, in the identified roster and in
/// `vosu.osu` `/session/focused`. On Windows no poller runs here, so no
/// process owns the foreground and the fact is `false`.
#[test]
fn production_host_reports_foreground_only_where_core_has_a_source() {
    use vosu_plugin_api::Selector;
    use vosu_plugin_testkit::subscriber::{SubscribeCommand, SubscriberEvent};
    let expected = cfg!(windows).then_some(false);
    let harness = Harness::production("vosu.osu", 0x6a00_0e00, 1);
    let pid = harness.pids[0];
    let (events, commands) = harness.subscriber("test.osu.foreground");
    commands
        .try_send(SubscribeCommand {
            session: None,
            selectors: vec![Selector {
                ns: "vosu.sessions".into(),
                pointer: String::new(),
            }],
        })
        .unwrap();
    harness.host.process_appeared(Harness::record(pid, &[]));
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
                ns: "vosu.client".into(),
                pointer: "/focused".into(),
            }],
        })
        .unwrap();
    let focus = loop {
        if let SubscriberEvent::Focus(focus) = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
            && focus.handle == handle
        {
            break focus;
        }
    };
    assert_eq!(focus.foreground, expected, "FOCUS_CHANGED foreground");

    let focused =
        || namespace(&harness, handle, "vosu.osu").map(|value| value["session"]["focused"].clone());
    eventually(|| focused().is_some());
    assert_eq!(
        focused().unwrap(),
        expected.map_or(serde_json::Value::Null, serde_json::Value::Bool),
        "vosu.osu /session/focused"
    );

    let requester = harness
        .host
        .surface_requester(vosu_core::plugin::PluginInstanceId::new(1))
        .unwrap();
    let capture = requester
        .capture(vosu_surface::state::SessionTarget::Focused, &[])
        .unwrap();
    assert_eq!(capture.session.as_ref().unwrap().foreground, expected);
    assert_eq!(capture.roster.sessions.len(), 1);
    assert_eq!(capture.roster.sessions[0].foreground, expected, "roster");
    assert_dispatches_ok();
}

#[test]
fn bus_error_selection_controls_collection_without_extension_flags() {
    use vosu_plugin_api::Selector;
    use vosu_plugin_testkit::subscriber::{SubscribeCommand, SubscriberEvent};
    let harness = Harness::new("vosu.osu", 0x6a00_0d00, 1);
    let pid = harness.pids[0];
    let (events, commands) = harness.subscriber("test.osu.o6.collection");
    commands
        .try_send(SubscribeCommand {
            session: None,
            selectors: vec![Selector {
                ns: "vosu.sessions".into(),
                pointer: String::new(),
            }],
        })
        .unwrap();
    harness.host.process_appeared(Harness::record(pid, &[]));
    let deadline = Instant::now() + WAIT;
    let handle = loop {
        if let SubscriberEvent::Attached(attached) = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
        {
            break attached.handle;
        }
    };
    let select = |ns: &str, pointer: &str| {
        commands
            .try_send(SubscribeCommand {
                session: Some(handle),
                selectors: vec![Selector {
                    ns: ns.into(),
                    pointer: pointer.into(),
                }],
            })
            .unwrap();
    };
    let collecting = || vosu_osu::test_support::session_error_collection(pid);
    select("vosu.status", "/status");
    // A precise raise positively acknowledges delivery of this subscription.
    eventually(|| vosu_osu::test_support::session_demand(pid).is_some_and(|demand| demand.precise));
    assert_eq!(collecting(), Some(false));
    select("vosu.osu", "/play/hitOffsets");
    eventually(|| collecting() == Some(true));
    harness.memory.set_status(2);
    eventually(|| {
        namespace(&harness, handle, "vosu.osu")
            .is_some_and(|value| value["session"]["state"]["number"] == 2)
    });
    // The real producer retains accurate histories while the permanent adapter
    // views, not a root snapshot or flag, decide the frozen wire contract.
    assert_eq!(collecting(), Some(true));
    for pointer in ["/play", ""] {
        select("vosu.status", "/status");
        eventually(|| collecting() == Some(false));
        select("vosu.osu", pointer);
        eventually(|| collecting() == Some(true));
    }
    assert_dispatches_ok();
}
