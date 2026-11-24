//! Discovery parity map §7: real manifest matching and real-plugin lifecycle.
#![cfg(feature = "test-harness")]
#[expect(
    dead_code,
    reason = "shared harness also provides settings and stress-test helpers"
)]
#[path = "osu_plugin/support.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use support::*;
use vosu_core::plugin::{CandidateProcess, MatchPlatform, MatcherEngine, test_support};
fn ready(harness: &Harness, count: usize) -> Vec<vosu_core::plugin::DiagnosticSession> {
    eventually(|| {
        let sessions = harness.host.diagnostic_sessions();
        sessions.len() == count
            && sessions.iter().all(|s| {
                !s.client.is_empty()
                    && harness
                        .host
                        .state()
                        .store
                        .current(vosu_core::state::SessionKey(s.handle))
                        .is_some()
            })
    });
    harness.host.diagnostic_sessions()
}
use vosu_plugin_api::{DetachReason, PluginManifest};
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};

fn engine(platform: MatchPlatform) -> MatcherEngine {
    let manifest = PluginManifest::parse(vosu_osu::MANIFEST).unwrap();
    let mut engine = MatcherEngine::new(platform);
    engine
        .register(manifest.id, manifest.matcher.unwrap())
        .unwrap();
    engine
}

/// Rows 1 and 4: Windows exact, case-insensitive names; Linux DB-revised
/// `linux_name_contains`, not `comm_hints`, accepts Wine-prefixed names.
#[test]
fn manifest_matches_both_platform_name_tables() {
    for (platform, cases) in [
        (
            MatchPlatform::Windows,
            vec![
                ("OSULAZER.EXE", true),
                ("osu!.exe", true),
                ("other.exe", false),
                ("wine-osu!.exe", false),
                ("osu!", false),
            ],
        ),
        (
            MatchPlatform::Linux,
            vec![
                ("osu!", true),
                ("wine-osu!.exe", true),
                ("osulazer.exe", true),
                ("other.exe", false),
                ("OSULAZER.EXE", false),
            ],
        ),
    ] {
        let engine = engine(platform);
        for (name, expected) in cases {
            let result = engine.match_candidate(&CandidateProcess {
                name: name.into(),
                path: format!("/games/{name}"),
                command_line: name.into(),
            });
            assert_eq!(
                result.winner.as_deref(),
                expected.then_some("vosu.osu"),
                "{platform:?}: {name}"
            );
            assert!(result.shadowed.is_empty());
        }
    }
}

/// Row 1: Windows variants follow bitness and unrelated names never attach.
/// Check PID-to-variant association independently of concurrent delivery order.
#[cfg(windows)]
#[test]
fn windows_names_and_bitness() {
    let harness = Harness::new("vosu.osu", 0x7a00_0000, 3);
    let mut lazer = Harness::record(harness.pids[1], &[]);
    lazer.name = "OSULAZER.EXE".into();
    lazer.bitness = vosu_plugin_api::Bitness::X64;
    harness.host.process_appeared(lazer);
    harness
        .host
        .process_appeared(Harness::record(harness.pids[0], &[]));
    let mut other = Harness::record(harness.pids[2], &[]);
    other.name = "other.exe".into();
    harness.host.process_appeared(other);
    let sessions = ready(&harness, 2);
    assert_eq!(
        sessions
            .iter()
            .map(|s| (s.pid, s.client.as_str()))
            .collect::<Vec<_>>(),
        vec![(harness.pids[0], "stable"), (harness.pids[1], "lazer")]
    );
    harness.host.destroy_all(WAIT);
    assert!(harness.host.diagnostic_sessions().is_empty());
    assert_dispatches_ok();
}

/// Row 4: a named Wine sibling makes the Linux fallback stable. A separate
/// discovery with only the fallback identifies lazer after its settle timer.
#[cfg(target_os = "linux")]
#[test]
fn linux_named_sibling_and_lone_fallback_variants() {
    {
        let harness = Harness::new("vosu.osu", 0x7a00_0100, 2);
        let mut named = Harness::record(harness.pids[1], &[]);
        named.name = "wine-osu!.exe".into();
        harness.host.process_appeared(named);
        ready(&harness, 1);
        let mut fallback = Harness::record(harness.pids[0], &[]);
        fallback.name = "osu!".into();
        harness.host.process_appeared(fallback);
        let sessions = ready(&harness, 2);
        assert!(sessions.iter().all(|s| s.client == "stable"));
        assert_eq!(
            sessions.iter().map(|s| s.pid).collect::<Vec<_>>(),
            harness.pids
        );
        harness.host.destroy_all(WAIT);
        assert!(harness.host.diagnostic_sessions().is_empty());
    }
    let harness = Harness::new("vosu.osu", 0x7a00_0200, 1);
    let mut fallback = Harness::record(harness.pids[0], &[]);
    fallback.name = "osu!".into();
    harness.host.process_appeared(fallback);
    assert_eq!(ready(&harness, 1)[0].client, "lazer");
    harness.host.destroy_all(WAIT);
    assert_dispatches_ok();
}

/// Row 5: debug-client presence/truthiness is preserved through host decline
/// delivery; declined generations stay dropped, while false/zero identify.
#[test]
fn command_filters_decline_and_suppress_reoffers() {
    let harness = Harness::new("vosu.osu", 0x7a00_0300, 6);
    let args = [
        "--debug-client-id=7",
        "--debug-client-id=1",
        "--debug-client-id=true",
        "--debug-client-id=false",
        "--debug-client-id=0",
        "--debug-client-id=",
    ];
    for (&pid, argument) in harness.pids.iter().zip(args) {
        harness
            .host
            .process_appeared(Harness::record(pid, &[argument]));
    }
    eventually(|| DECLINES.lock().unwrap().len() == 3);
    let sessions = ready(&harness, 3);
    for (&pid, argument) in harness.pids.iter().zip(args).take(3) {
        harness
            .host
            .process_appeared(Harness::record(pid, &[argument]));
    }
    harness.host.destroy_all(WAIT);
    let attaches = ATTACHES.lock().unwrap();
    assert_eq!(attaches.values().sum::<usize>(), 6);
    assert!(harness.pids.iter().all(|pid| attaches.get(pid) == Some(&1)));
    assert_eq!(
        DECLINES
            .lock()
            .unwrap()
            .iter()
            .filter(|&&reason| reason == DetachReason::Declined)
            .count(),
        3
    );
    assert_eq!(
        sessions.iter().map(|s| s.pid).collect::<Vec<_>>(),
        harness.pids[3..]
    );
    assert!(sessions.iter().all(|s| {
        harness
            .host
            .state()
            .store
            .current(vosu_core::state::SessionKey(s.handle))
            .is_none()
    }));
    assert_dispatches_ok();
}

/// M4a/T0 intentionally replaces the old tournament decline with an identified
/// stable priority manager. Rediscovery must still never attach its generation
/// twice; readiness comes from host lifecycle and committed state.
#[test]
fn tournament_manager_identifies_through_bus_and_suppresses_reoffers() {
    use vosu_core::state::{Pointer, SessionKey, StateValue};
    let harness = Harness::new("vosu.osu", 0x7a00_0600, 1);
    let pid = harness.pids[0];
    harness
        .host
        .process_appeared(Harness::record(pid, &["--tournament="]));
    let handle = ready(&harness, 1)[0].handle;
    let state = harness
        .host
        .state()
        .store
        .current(SessionKey(handle))
        .unwrap();
    assert!(
        matches!(state.get("vosu.client", &Pointer::parse("/variant").unwrap()), Some(StateValue::String(variant)) if variant.as_ref() == "stable")
    );
    assert!(DECLINES.lock().unwrap().is_empty());
    for _ in 0..3 {
        harness
            .host
            .process_appeared(Harness::record(pid, &["--tournament="]));
    }
    assert_eq!(ATTACHES.lock().unwrap().get(&pid), Some(&1));
    harness.host.destroy_all(WAIT);
    assert!(
        harness
            .host
            .state()
            .store
            .current(SessionKey(handle))
            .is_none()
    );
    assert_dispatches_ok();
}

/// Row 6: spectator/devserver arguments reach the public session projection,
/// rather than being mistaken for the tournament filter.
#[test]
fn spectator_devserver_settings_reach_bus() {
    use vosu_core::state::{Pointer, SessionKey};
    let harness = Harness::new("vosu.osu", 0x7a00_0400, 1);
    harness.host.process_appeared(Harness::record(
        harness.pids[0],
        &["--spectateclient=12", "--devserver=example.test"],
    ));
    let sessions = ready(&harness, 1);
    eventually(|| {
        harness
            .host
            .state()
            .store
            .current(SessionKey(sessions[0].handle))
            .is_some_and(|state| {
                state
                    .get("vosu.osu", &Pointer::parse("/session").unwrap())
                    .is_some()
            })
    });
    let state = harness
        .host
        .state()
        .store
        .current(SessionKey(sessions[0].handle))
        .unwrap();
    let mut bytes = Vec::new();
    state
        .get("vosu.osu", &Pointer::parse("/session").unwrap())
        .unwrap()
        .write_json(&mut bytes);
    let session: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(session["ipcId"], 12.0);
    assert_eq!(session["server"], "example.test");
    assert!(!DECLINES.lock().unwrap().contains(&DetachReason::Declined));
    harness.host.destroy_all(WAIT);
    assert_dispatches_ok();
}

/// Row 3: core discovery's already-unit-tested replacement is exit followed
/// by appearance. The real plugin must retire the old lifecycle generation.
#[test]
fn explicit_exit_then_appearance_advances_plugin_generation() {
    use vosu_core::state::SessionKey;
    let harness = Harness::new("vosu.osu", 0x7a00_0500, 1);
    let pid = harness.pids[0];
    harness.host.process_appeared(Harness::record(pid, &[]));
    let old = ready(&harness, 1).remove(0);
    eventually(|| harness.memory.plans.load(Ordering::Relaxed) > 0);
    let previous_plans = harness.memory.plans.load(Ordering::Relaxed);
    harness.host.process_exited(pid);
    eventually(|| {
        harness
            .host
            .state()
            .store
            .current(SessionKey(old.handle))
            .is_none()
    });
    harness.host.process_appeared(Harness::record(pid, &[]));
    let next = ready(&harness, 1).remove(0);
    eventually(|| harness.memory.plans.load(Ordering::Relaxed) > previous_plans);
    assert_eq!(old.pid, next.pid);
    assert_ne!(old.handle, next.handle);
    harness.host.destroy_all(WAIT);
    assert_eq!(ATTACHES.lock().unwrap().get(&pid), Some(&2));
    assert_eq!(DECLINES.lock().unwrap().len(), 1);
    assert!(
        harness
            .host
            .state()
            .store
            .current(SessionKey(old.handle))
            .is_none()
    );
    assert!(
        harness
            .host
            .state()
            .store
            .current(SessionKey(next.handle))
            .is_none()
    );
    assert_dispatches_ok();
}

struct CountedMemory {
    inner: Arc<Memory>,
    drops: Arc<AtomicUsize>,
}
impl Drop for CountedMemory {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl ProcessMemory for CountedMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.inner.read_exact(address, dst)
    }
    fn read_many(&self, requests: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.inner.read_many(requests)
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        self.inner.readable_regions()
    }
}

/// Row 7: duplicate discovery attaches once; duplicate removal releases the
/// host's cached memory exactly once, without needing host destruction.
#[test]
fn duplicate_discovery_and_removal_close_memory_once() {
    let harness = Harness::new("vosu.osu", 0x7a00_0600, 1);
    let pid = harness.pids[0];
    let drops = Arc::new(AtomicUsize::new(0));
    test_support::clear_session_memory(pid);
    test_support::set_session_memory(
        pid,
        Arc::new(CountedMemory {
            inner: harness.memory.clone(),
            drops: drops.clone(),
        }),
        ProcessBitness::X86,
    );
    for _ in 0..2 {
        harness.host.process_appeared(Harness::record(pid, &[]));
    }
    let session = ready(&harness, 1).remove(0);
    eventually(|| harness.memory.plans.load(Ordering::Relaxed) > 0);
    test_support::clear_session_memory(pid);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "live session must retain its cached memory"
    );
    for _ in 0..2 {
        harness.host.process_exited(pid);
    }
    eventually(|| drops.load(Ordering::SeqCst) == 1);
    // Cache eviction precedes asynchronous terminal delivery. Observe both
    // before destroying the instance, which may retire its queued deliveries.
    eventually(|| DECLINES.lock().unwrap().as_slice() == [DetachReason::Exited]);
    harness.host.destroy_all(WAIT);
    assert_eq!(ATTACHES.lock().unwrap().get(&pid), Some(&1));
    assert_eq!(DECLINES.lock().unwrap().len(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(
        harness
            .host
            .state()
            .store
            .current(vosu_core::state::SessionKey(session.handle))
            .is_none()
    );
    assert_dispatches_ok();
}
