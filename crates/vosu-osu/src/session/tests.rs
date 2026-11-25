//! `OsuSession` unit tests (P4): install-once, cancel-is-synchronous,
//! join-on-detach semantics, and the `GenerationIdentity` port. An offline
//! generator (always fails) exercises the resolver lifecycle without any
//! real lazer install or network access; cancellation preempting the
//! retry loop's own 30 s-to-300 s backoff is what keeps every join here
//! bounded (asserted directly below, not just hoped for).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use vosu_proptest::bounded;

use bytes::Bytes;
use tokio_util::sync::CancellationToken;

use vosu_osu_model::enums::ClientType;
use vosu_osu_model::launch::InstanceSettings;
use vosu_osu_reader::memory::lazer_loader::{GenerationFuture, LazerOffsetLoader, OffsetGenerator};
use vosu_plugin_api::memory::{
    MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest,
};

use super::*;

/// Always fails, immediately: proves the resolver lifecycle (install-once,
/// cancel, join) without any real lazer install, exactly like
/// `src/instance/tests.rs`'s own `OfflineGenerator` (1.x, deleted in C3).
struct OfflineGenerator;

impl OffsetGenerator for OfflineGenerator {
    fn generate<'a>(
        &'a self,
        _executable_root: &'a Path,
        _version: &'a str,
        _cancel: &'a CancellationToken,
    ) -> GenerationFuture<'a> {
        Box::pin(async { Err(io::Error::new(io::ErrorKind::NotFound, "offline")) })
    }
}

/// A `ProcessMemory` whose only interesting behavior is `is_alive`,
/// toggled by the test: every read method here is unused by these tests.
struct FakeMemory {
    alive: AtomicBool,
}

impl FakeMemory {
    fn new(alive: bool) -> Arc<Self> {
        Arc::new(Self {
            alive: AtomicBool::new(alive),
        })
    }
}

impl ProcessMemory for FakeMemory {
    fn read_exact(&self, _address: usize, _dst: &mut [u8]) -> Result<(), MemoryError> {
        Err(MemoryError::Backend {
            operation: "read_exact",
            message: "unused by OsuSession tests".to_owned(),
        })
    }

    fn read_many(&self, _reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

fn test_settings() -> InstanceSettings {
    InstanceSettings {
        tournament_spectator: false,
        tournament_ipc_id: None,
        devserver_endpoint: None,
    }
}

fn session_with(client: ClientType, memory: Arc<FakeMemory>) -> Arc<OsuSession> {
    let process = ProcessHandle::new(memory, ProcessBitness::X64);
    OsuSession::new(
        1,
        // pid 0 never names a real process, so `executable_root()`'s live
        // `/proc`/module-enumeration read fails closed deterministically
        // (P3 review #5), instead of reading the real pid 100 on whatever
        // machine runs this test.
        0,
        1,
        client,
        process,
        PathBuf::new(),
        PathBuf::new(),
        test_settings(),
        None,
    )
}

/// Returns the loader alongside its `TempDir` guard (P3 review #6): the
/// caller must keep the guard bound for the test's duration, or its
/// directory is deleted while the loader still references it.
fn offline_loader() -> (Arc<LazerOffsetLoader<OfflineGenerator>>, tempfile::TempDir) {
    let cache_dir = tempfile::tempdir().expect("tempdir");
    let loader = Arc::new(LazerOffsetLoader::new(
        OfflineGenerator,
        cache_dir.path().to_owned(),
        Bytes::from_static(b"{}"),
    ));
    (loader, cache_dir)
}

fn noop_warn() -> LogWarn {
    Arc::new(|_message: String| {})
}

/// A [`LogWarn`] that also signals `attempted` the first (and every) time
/// it is invoked, so a test can wait for the resolver's first failed
/// attempt to actually be reported before exercising cancellation (P3
/// review #5), rather than racing `join_resolver` against a background
/// task that may not have run at all yet.
fn warn_signal() -> (LogWarn, Arc<tokio::sync::Notify>) {
    let attempted = Arc::new(tokio::sync::Notify::new());
    let signal = Arc::clone(&attempted);
    let warn: LogWarn = Arc::new(move |_message: String| signal.notify_one());
    (warn, attempted)
}

#[tokio::test]
async fn stable_session_never_starts_a_resolver() {
    let session = session_with(ClientType::Stable, FakeMemory::new(true));
    let (loader, _data_dir) = offline_loader();
    assert!(!bounded!(session.start_lazer_resolver(loader, noop_warn())));
    assert!(bounded!(session.resolver.lock()).is_none());
}

#[tokio::test]
async fn lazer_session_starts_at_most_one_resolver() {
    let session = session_with(ClientType::Lazer, FakeMemory::new(true));
    let (loader, _data_dir) = offline_loader();
    assert!(bounded!(
        session.start_lazer_resolver(Arc::clone(&loader), noop_warn())
    ));
    assert!(bounded!(session.resolver.lock()).is_some());

    // A second call while one is already running is a no-op
    // (install-once, ported from `Instance::start_lazer_resolver_inner`).
    assert!(!bounded!(session.start_lazer_resolver(loader, noop_warn())));
    bounded!(session.join_resolver());
}

#[tokio::test]
async fn join_resolver_cancels_and_joins_promptly() {
    let session = session_with(ClientType::Lazer, FakeMemory::new(true));
    let (loader, _data_dir) = offline_loader();
    let (warn, attempted) = warn_signal();
    assert!(bounded!(session.start_lazer_resolver(loader, warn)));
    // Wait for the resolver to actually report its first failed attempt
    // (pid 0's `executable_root()` fails immediately and
    // deterministically) before cancelling it, so the resolver really is
    // inside `retry_pending`'s backoff sleep rather than a background
    // task that has not run yet.
    tokio::time::timeout(Duration::from_secs(5), attempted.notified())
        .await
        .expect("the resolver must report its first failed attempt promptly");
    // A bounded timeout around `join_resolver` only passes if
    // cancellation actually preempts that sleep rather than waiting it
    // out.
    tokio::time::timeout(Duration::from_secs(5), session.join_resolver())
        .await
        .expect("cancellation must preempt the retry backoff, not wait it out");
    assert!(bounded!(session.resolver.lock()).is_none());
    assert!(session.lifecycle.is_cancelled());

    // A resolver cannot be restarted on a cancelled session: `join_resolver`
    // is also how `destroy`/detach shut a session down for good.
    let (loader, _data_dir) = offline_loader();
    assert!(!bounded!(session.start_lazer_resolver(loader, noop_warn())));
}

#[test]
fn install_lazer_offsets_is_refused_once_cancelled() {
    let session = session_with(ClientType::Lazer, FakeMemory::new(true));
    session.cancel();
    assert!(!session.install_lazer_offsets(Arc::new(LazerOffsets::embedded().unwrap())));
    assert!(session.lazer_offsets().is_none());
}

#[test]
fn install_lazer_offsets_is_refused_once_the_process_is_gone() {
    let session = session_with(ClientType::Lazer, FakeMemory::new(false));
    assert!(!session.install_lazer_offsets(Arc::new(LazerOffsets::embedded().unwrap())));
}

#[test]
fn install_lazer_offsets_is_refused_for_a_stable_session() {
    let session = session_with(ClientType::Stable, FakeMemory::new(true));
    assert!(!session.install_lazer_offsets(Arc::new(LazerOffsets::embedded().unwrap())));
}

#[test]
fn install_lazer_offsets_publishes_exactly_once() {
    let session = session_with(ClientType::Lazer, FakeMemory::new(true));
    let offsets = Arc::new(LazerOffsets::embedded().unwrap());
    assert!(session.install_lazer_offsets(Arc::clone(&offsets)));
    assert!(session.lazer_offsets().is_some());
    // A second install, even with a distinct (but equally valid) value,
    // is refused: the first installed value wins.
    assert!(!session.install_lazer_offsets(Arc::new(LazerOffsets::embedded().unwrap())));
}

mod capture;
