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
use vosu_osu_model::enums::ClientType;
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
    let harness = Harness::new("test.osu.p7.windows", 0x7a00_0000, 3);
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
    harness
        .sink
        .until(WAIT, |events| events.identified.len() == 2);
    harness.host.destroy_all(WAIT);
    let events = harness.sink.events.lock().unwrap();
    let retained: Vec<_> = events
        .identified
        .iter()
        .map(|(key, (client, _))| (key.pid, *client))
        .collect();
    assert_eq!(
        retained,
        vec![
            (harness.pids[0], ClientType::Stable),
            (harness.pids[1], ClientType::Lazer)
        ]
    );
    assert_dispatches_ok();
}

/// Row 4: a named Wine sibling makes the Linux fallback stable. A separate
/// discovery with only the fallback identifies lazer after its settle timer.
#[cfg(target_os = "linux")]
#[test]
fn linux_named_sibling_and_lone_fallback_variants() {
    {
        let harness = Harness::new("test.osu.p7.siblings", 0x7a00_0100, 2);
        let mut named = Harness::record(harness.pids[1], &[]);
        named.name = "wine-osu!.exe".into();
        harness.host.process_appeared(named);
        // Observe the named sibling before offering the fallback: this is not
        // an assertion about concurrent dispatch scheduling or timer speed.
        harness
            .sink
            .until(WAIT, |events| events.identified.len() == 1);
        let mut fallback = Harness::record(harness.pids[0], &[]);
        fallback.name = "osu!".into();
        harness.host.process_appeared(fallback);
        harness
            .sink
            .until(WAIT, |events| events.identified.len() == 2);
        harness.host.destroy_all(WAIT);
        let events = harness.sink.events.lock().unwrap();
        assert!(
            events
                .identified
                .values()
                .all(|(client, _)| *client == ClientType::Stable)
        );
        assert_eq!(
            events
                .identified
                .keys()
                .map(|key| key.pid)
                .collect::<Vec<_>>(),
            harness.pids
        );
    }
    let harness = Harness::new("test.osu.p7.fallback", 0x7a00_0200, 1);
    let mut fallback = Harness::record(harness.pids[0], &[]);
    fallback.name = "osu!".into();
    harness.host.process_appeared(fallback);
    harness
        .sink
        .until(WAIT, |events| events.identified.len() == 1);
    harness.host.destroy_all(WAIT);
    assert_eq!(
        harness
            .sink
            .events
            .lock()
            .unwrap()
            .identified
            .values()
            .next()
            .unwrap()
            .0,
        ClientType::Lazer
    );
    assert_dispatches_ok();
}

/// Row 5: presence/truthiness is preserved through host decline delivery;
/// declined generations stay dropped when rediscovered, false/zero identify.
#[test]
fn command_filters_decline_and_suppress_reoffers() {
    let harness = Harness::new("test.osu.p7.filters", 0x7a00_0300, 6);
    let args = [
        "--tournament=",
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
    harness
        .sink
        .until(WAIT, |events| events.identified.len() == 3);
    for (&pid, argument) in harness.pids.iter().zip(args).take(3) {
        harness
            .host
            .process_appeared(Harness::record(pid, &[argument]));
    }
    harness.host.destroy_all(WAIT);
    let reasons = DECLINES.lock().unwrap();
    assert_eq!(
        reasons
            .iter()
            .filter(|&&reason| reason == DetachReason::Declined)
            .count(),
        3
    );
    let events = harness.sink.events.lock().unwrap();
    assert_eq!(
        events
            .identified
            .keys()
            .map(|key| key.pid)
            .collect::<Vec<_>>(),
        harness.pids[3..]
    );
    assert!(!events.late_publish);
    assert_dispatches_ok();
}

/// Row 6: spectator/devserver arguments identify normally and reach B1 as
/// typed settings, rather than being mistaken for the tournament filter.
#[test]
fn spectator_devserver_settings_reach_b1() {
    let harness = Harness::new("test.osu.p7.spectator", 0x7a00_0400, 1);
    harness.host.process_appeared(Harness::record(
        harness.pids[0],
        &["--spectateclient=12", "--devserver=example.test"],
    ));
    harness
        .sink
        .until(WAIT, |events| events.identified.len() == 1);
    harness.host.destroy_all(WAIT);
    let events = harness.sink.events.lock().unwrap();
    let (_, settings) = events.identified.values().next().unwrap();
    assert!(settings.tournament_spectator);
    assert_eq!(settings.tournament_ipc_id, Some(12.0));
    assert_eq!(
        settings.devserver_endpoint.as_deref(),
        Some(std::ffi::OsStr::new("example.test"))
    );
    assert!(!DECLINES.lock().unwrap().contains(&DetachReason::Declined));
    assert_dispatches_ok();
}

/// Row 3: core discovery's already-unit-tested replacement is exit followed
/// by appearance. The real plugin must retire the old lifecycle generation.
#[test]
fn same_pid_replacement_cancels_old_plugin_generation() {
    let harness = Harness::new("test.osu.p7.generation", 0x7a00_0500, 1);
    let pid = harness.pids[0];
    harness.host.process_appeared(Harness::record(pid, &[]));
    harness
        .sink
        .until(WAIT, |events| !events.published.is_empty());
    let old = *harness
        .sink
        .events
        .lock()
        .unwrap()
        .attached
        .first()
        .unwrap();
    harness.host.process_exited(pid);
    harness
        .sink
        .until(WAIT, |events| events.detached.contains(&old));
    let mut replacement = Harness::record(pid, &[]);
    replacement.start_time += 1;
    harness.host.process_appeared(replacement);
    harness.sink.until(WAIT, |events| {
        events.identified.len() == 2 && events.published.len() == 2
    });
    harness.host.destroy_all(WAIT);
    let events = harness.sink.events.lock().unwrap();
    let keys: Vec<_> = events.identified.keys().copied().collect();
    assert_eq!(keys[0].pid, keys[1].pid);
    assert_ne!(keys[0].generation, keys[1].generation);
    assert_eq!(events.detached.len(), 2);
    assert!(!events.late_publish);
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
    let harness = Harness::new("test.osu.p7.idempotent", 0x7a00_0600, 1);
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
    harness
        .sink
        .until(WAIT, |events| !events.published.is_empty());
    test_support::clear_session_memory(pid);
    assert_eq!(
        drops.load(Ordering::SeqCst),
        0,
        "live session must retain its cached memory"
    );
    for _ in 0..2 {
        harness.host.process_exited(pid);
    }
    harness
        .sink
        .until(WAIT, |events| events.detached.len() == 1);
    eventually(|| drops.load(Ordering::SeqCst) == 1);
    harness.host.destroy_all(WAIT);
    let events = harness.sink.events.lock().unwrap();
    assert_eq!(events.identified.len(), 1);
    assert_eq!(events.detached.len(), 1);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(!events.late_publish);
    assert_dispatches_ok();
}
