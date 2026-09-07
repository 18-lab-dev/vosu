#[path = "updater_native/handoff.rs"]
mod handoff;
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::Child,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use vosu::{
    platform::atomic_replace,
    updater::{
        EncodedOsString, INSTALL_RECORD_NAME, InstallPhase, InstallRecord, ProcessIdentity,
        RecoveryAction, StartupIntent, StartupReadiness, TransactionGuard, UpdateCut, UpdateError,
        UpdatePlatform, finish_ready_cleanup, load_record, parse_startup_update_args,
        recover_dead_transaction, recover_startup_update, run_helper,
    },
};

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReadyBehavior {
    Succeed,
    FailCandidate,
    FailLaunchCandidate,
    FailAll,
}

struct NativePlatform {
    identity: ProcessIdentity,
    live_owner_polls: AtomicUsize,
    helper_alive_polls: AtomicUsize,
    fail_swap: AtomicBool,
    fail_cut: Mutex<Option<UpdateCut>>,
    seen: Mutex<Vec<UpdateCut>>,
    readiness: Mutex<ReadyBehavior>,
    terminated: AtomicBool,
}

impl NativePlatform {
    fn new() -> Self {
        Self {
            identity: ProcessIdentity {
                pid: 7001,
                start: 9001,
            },
            live_owner_polls: AtomicUsize::new(0),
            helper_alive_polls: AtomicUsize::new(0),
            fail_cut: Mutex::new(None),
            fail_swap: AtomicBool::new(false),
            seen: Mutex::new(Vec::new()),
            readiness: Mutex::new(ReadyBehavior::Succeed),
            terminated: AtomicBool::new(false),
        }
    }

    fn failing(cut: UpdateCut) -> Self {
        let platform = Self::new();
        *platform.fail_cut.lock() = Some(cut);
        platform
    }

    fn saw(&self, cut: UpdateCut) -> bool {
        self.seen.lock().contains(&cut)
    }
}

impl UpdatePlatform for NativePlatform {
    fn current_identity(&self) -> io::Result<ProcessIdentity> {
        Ok(self.identity.clone())
    }

    fn identity_for_pid(&self, pid: u32) -> io::Result<ProcessIdentity> {
        Ok(ProcessIdentity {
            pid,
            start: 123_456,
        })
    }

    fn identity_is_alive(&self, identity: &ProcessIdentity) -> io::Result<bool> {
        if identity.pid == 4242 && identity.start == 111 {
            return Ok(self
                .live_owner_polls
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                    left.checked_sub(1)
                })
                .is_ok());
        }
        if identity == &self.identity {
            return Ok(self
                .helper_alive_polls
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                    left.checked_sub(1)
                })
                .is_ok());
        }
        Ok(false)
    }

    fn random_128(&self) -> io::Result<[u8; 16]> {
        Ok([0x5a; 16])
    }

    fn atomic_swap(&self, target: &Path, staged: &Path, backup: &Path) -> io::Result<()> {
        if self.fail_swap.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected locked target",
            ));
        }
        fs::hard_link(target, backup)?;
        atomic_replace(staged, target)
    }

    fn restore_backup(&self, target: &Path, backup: &Path) -> io::Result<()> {
        if target.exists() {
            fs::remove_file(target)?;
        }
        fs::rename(backup, target)
    }

    fn spawn(&self, _: &Path, _: &[OsString], _: &Path) -> io::Result<Child> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native test uses readiness_override",
        ))
    }

    fn terminate_and_wait(&self, _: &ProcessIdentity, _: Duration) -> io::Result<()> {
        self.terminated.store(true, Ordering::Release);
        self.live_owner_polls.store(0, Ordering::Release);
        Ok(())
    }

    fn checkpoint(&self, cut: UpdateCut) -> io::Result<()> {
        self.seen.lock().push(cut);
        if self.fail_cut.lock().as_ref() == Some(&cut) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                format!("injected {cut:?}"),
            ));
        }
        Ok(())
    }

    fn readiness_override(&self, rollback: bool) -> Option<io::Result<()>> {
        let result = match *self.readiness.lock() {
            ReadyBehavior::Succeed => Ok(()),
            ReadyBehavior::FailCandidate | ReadyBehavior::FailLaunchCandidate if rollback => Ok(()),
            ReadyBehavior::FailLaunchCandidate => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "injected candidate launch failure",
            )),
            ReadyBehavior::FailCandidate | ReadyBehavior::FailAll => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "injected readiness failure",
            )),
        };
        Some(result)
    }
}

struct Fixture {
    root: TempDir,
    record_path: PathBuf,
    record: InstallRecord,
}

impl Fixture {
    fn new(phase: InstallPhase, target_bytes: Option<&[u8]>) -> Self {
        let root = TempDir::new().unwrap();
        let program = root.path();
        let nonce = "ab".repeat(16);
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let target = program.join(format!("vosu{suffix}"));
        if let Some(bytes) = target_bytes {
            write_launchable(&target, bytes);
        }
        let work = program.join(format!(".vosu-update-work-{nonce}"));
        let cache = program.join(".cache");
        fs::create_dir_all(&work).unwrap();
        fs::create_dir_all(&cache).unwrap();
        let part = work.join("archive.part");
        let staging = program.join(format!(".vosu-stage-{nonce}{suffix}"));
        let backup = program.join(format!(".vosu-backup-{nonce}{suffix}"));
        let helper_path = cache.join(format!(".vosu-update-helper-{nonce}{suffix}"));
        fs::write(&part, b"archive").unwrap();
        write_launchable(&staging, b"new");
        write_launchable(&backup, b"old");
        write_launchable(&helper_path, b"helper");
        let record = InstallRecord {
            nonce,
            token: "11".repeat(16),
            owner: ProcessIdentity {
                pid: 4242,
                start: 111,
            },
            helper: None,
            original_args: vec![EncodedOsString::capture(OsString::from("--update=false"))],
            working_directory: EncodedOsString::capture(program.as_os_str().to_owned()),
            target,
            old_digest: digest(b"old"),
            archive_digest: Some(digest(b"archive")),
            new_digest: Some(digest(b"new")),
            phase,
            work,
            part,
            staging,
            backup,
            helper_path,
        };
        let record_path = program.join(INSTALL_RECORD_NAME);
        fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        Self {
            root,
            record_path,
            record,
        }
    }

    fn persist(&self) {
        let temp_path = self
            .root
            .path()
            .join(format!("{}-{}.tmp", INSTALL_RECORD_NAME, self.record.nonce));
        let bytes = serde_json::to_vec(&self.record).unwrap();
        let mut file = fs::File::create(&temp_path).unwrap();
        file.write_all(&bytes).unwrap();
        file.sync_all().unwrap();
        drop(file);
        atomic_replace(&temp_path, &self.record_path).unwrap();
    }

    fn assert_target(&self, expected: &[u8]) {
        assert_eq!(fs::read(&self.record.target).unwrap(), expected);
        assert!(
            self.record.target.is_file(),
            "target must remain present and launchable"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                fs::metadata(&self.record.target)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }
    }

    fn assert_clean(&self) {
        assert!(!self.record_path.exists(), "install record residue");
        assert!(!self.record.work.exists(), "work residue");
        assert!(!self.record.staging.exists(), "staging residue");
        assert!(!self.record.backup.exists(), "backup residue");
        assert!(!self.record.helper_path.exists(), "helper residue");
    }
}

fn write_launchable(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[test]
fn record_creation_and_every_phase_fsync_cut_are_cleanup_safe_and_reentrant() {
    for cut in [UpdateCut::BeforeRecordCreate, UpdateCut::AfterRecordCreate] {
        let root = TempDir::new().unwrap();
        let target = root
            .path()
            .join(if cfg!(windows) { "vosu.exe" } else { "vosu" });
        write_launchable(&target, b"old");
        let platform = NativePlatform::failing(cut);
        assert!(
            TransactionGuard::begin(root.path(), &target, &platform).is_err(),
            "{cut:?}"
        );
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert!(
            recover_startup_update(root.path(), &platform)
                .unwrap()
                .is_none()
        );
        assert!(
            recover_startup_update(root.path(), &platform)
                .unwrap()
                .is_none()
        );
        assert_no_update_residue(root.path());
    }

    for phase in [
        InstallPhase::Metadata,
        InstallPhase::Downloading,
        InstallPhase::Verified,
        InstallPhase::Prepared,
        InstallPhase::HandedOff,
        InstallPhase::Swapped,
        InstallPhase::Rollback,
        InstallPhase::Ready,
    ] {
        for cut in [UpdateCut::BeforePhase(phase), UpdateCut::AfterPhase(phase)] {
            let root = TempDir::new().unwrap();
            let target = root
                .path()
                .join(if cfg!(windows) { "vosu.exe" } else { "vosu" });
            write_launchable(&target, b"old");
            let platform = NativePlatform::failing(cut);
            let mut guard = TransactionGuard::begin(root.path(), &target, &platform).unwrap();
            assert!(guard.persist_phase(phase).is_err(), "{cut:?}");
            drop(guard);
            assert_eq!(fs::read(&target).unwrap(), b"old");
            assert!(
                recover_startup_update(root.path(), &platform)
                    .unwrap()
                    .is_none()
            );
            assert!(
                recover_startup_update(root.path(), &platform)
                    .unwrap()
                    .is_none()
            );
            assert_no_update_residue(root.path());
        }
    }
}

#[test]
fn durable_pre_handoff_states_cleanup_idempotently_and_allow_second_call() {
    for phase in [
        InstallPhase::Created,
        InstallPhase::Metadata,
        InstallPhase::Downloading,
        InstallPhase::Verified,
        InstallPhase::Prepared,
        InstallPhase::HandedOff,
    ] {
        let fixture = Fixture::new(phase, Some(b"old"));
        let platform = NativePlatform::new();
        assert_eq!(
            recover_dead_transaction(&fixture.record_path, &platform).unwrap(),
            RecoveryAction::CleanAndContinue,
            "{phase:?}"
        );
        fixture.assert_target(b"old");
        fixture.assert_clean();
        assert!(
            recover_startup_update(fixture.root.path(), &platform)
                .unwrap()
                .is_none()
        );
        assert!(
            recover_startup_update(fixture.root.path(), &platform)
                .unwrap()
                .is_none()
        );

        let second =
            TransactionGuard::begin(fixture.root.path(), &fixture.record.target, &platform)
                .unwrap();
        drop(second);
        assert_no_update_residue(fixture.root.path());
    }
}
#[test]
fn latest_no_files_and_pre_handoff_errors_release_single_flight_for_second_calls() {
    for terminal in ["latest", "noFiles"] {
        let root = TempDir::new().unwrap();
        let target = root
            .path()
            .join(if cfg!(windows) { "vosu.exe" } else { "vosu" });
        write_launchable(&target, b"old");
        let platform = NativePlatform::new();
        TransactionGuard::begin(root.path(), &target, &platform)
            .unwrap()
            .cancel()
            .unwrap();
        let second = TransactionGuard::begin(root.path(), &target, &platform)
            .unwrap_or_else(|error| panic!("second call after {terminal} failed: {error}"));
        drop(second);
        assert_no_update_residue(root.path());
    }

    for cut in [
        UpdateCut::BeforeRecordCreate,
        UpdateCut::AfterRecordCreate,
        UpdateCut::BeforePhase(InstallPhase::Metadata),
        UpdateCut::AfterPhase(InstallPhase::Metadata),
        UpdateCut::BeforePhase(InstallPhase::Downloading),
        UpdateCut::AfterPhase(InstallPhase::Downloading),
        UpdateCut::BeforePartDownload,
        UpdateCut::AfterPartDownload,
        UpdateCut::BeforeVerification,
        UpdateCut::AfterVerification,
        UpdateCut::BeforePhase(InstallPhase::Verified),
        UpdateCut::AfterPhase(InstallPhase::Verified),
        UpdateCut::BeforeStaging,
        UpdateCut::AfterStaging,
        UpdateCut::BeforePhase(InstallPhase::Prepared),
        UpdateCut::AfterPhase(InstallPhase::Prepared),
        UpdateCut::BeforeHelperCopy,
        UpdateCut::AfterHelperCopy,
        UpdateCut::BeforeHandoff,
    ] {
        let root = TempDir::new().unwrap();
        let target = root
            .path()
            .join(if cfg!(windows) { "vosu.exe" } else { "vosu" });
        write_launchable(&target, b"old");
        let crashing = NativePlatform::failing(cut);
        assert!(crashing.checkpoint(cut).is_err());
        let healthy = NativePlatform::new();
        let first = TransactionGuard::begin(root.path(), &target, &healthy).unwrap();
        drop(first);
        let second = TransactionGuard::begin(root.path(), &target, &healthy)
            .unwrap_or_else(|error| panic!("second call after {cut:?} failed: {error}"));
        drop(second);
        assert_no_update_residue(root.path());
    }
}

#[test]
fn concurrent_call_is_rejected_without_stealing_live_owner() {
    let root = TempDir::new().unwrap();
    let target = root
        .path()
        .join(if cfg!(windows) { "vosu.exe" } else { "vosu" });
    write_launchable(&target, b"old");
    let platform = Arc::new(NativePlatform::new());
    platform
        .helper_alive_polls
        .store(usize::MAX, Ordering::Release);
    let first = TransactionGuard::begin(root.path(), &target, platform.as_ref()).unwrap();
    std::thread::scope(|scope| {
        let contender = scope.spawn(|| {
            match TransactionGuard::begin(root.path(), &target, platform.as_ref()) {
                Ok(_) => panic!("concurrent transaction unexpectedly acquired ownership"),
                Err(error) => error,
            }
        });
        let error = contender.join().unwrap();
        assert!(matches!(error, UpdateError::AlreadyRunning));
    });
    drop(first);
    assert_no_update_residue(root.path());
}

#[test]
fn exact_old_identity_exit_then_swap_candidate_ready_and_delayed_helper_cleanup() {
    let mut fixture = Fixture::new(InstallPhase::HandedOff, Some(b"old"));
    fs::remove_file(&fixture.record.backup).unwrap();
    fixture.record.helper = Some(ProcessIdentity {
        pid: 7001,
        start: 9001,
    });
    fixture.persist();
    let platform = NativePlatform::new();
    platform.live_owner_polls.store(2, Ordering::Release);

    run_helper(&fixture.record_path, &fixture.record.token, &platform).unwrap();
    assert!(
        !platform.terminated.load(Ordering::Acquire),
        "exact old identity exited without termination"
    );
    assert!(platform.saw(UpdateCut::BeforeSwap));
    assert!(platform.saw(UpdateCut::AfterSwap));
    assert_eq!(
        load_record(&fixture.record_path).unwrap().phase,
        InstallPhase::Ready
    );
    fixture.assert_target(b"new");

    platform.helper_alive_polls.store(2, Ordering::Release);
    finish_ready_cleanup(&fixture.record_path, &fixture.record.token, &platform).unwrap();
    fixture.assert_clean();
    assert!(
        recover_startup_update(fixture.root.path(), &platform)
            .unwrap()
            .is_none()
    );
}

#[test]
fn candidate_failure_rolls_back_and_readiness_failure_never_removes_target() {
    for behavior in [
        ReadyBehavior::FailLaunchCandidate,
        ReadyBehavior::FailCandidate,
    ] {
        let fixture = Fixture::new(InstallPhase::Swapped, Some(b"new"));
        let platform = NativePlatform::new();
        *platform.readiness.lock() = behavior;
        assert_eq!(
            recover_dead_transaction(&fixture.record_path, &platform).unwrap(),
            RecoveryAction::ResumeCandidate
        );
        assert_eq!(
            load_record(&fixture.record_path).unwrap().phase,
            InstallPhase::Ready
        );
        fixture.assert_target(b"old");
        finish_ready_cleanup(&fixture.record_path, &fixture.record.token, &platform).unwrap();
        fixture.assert_clean();
    }

    let fixture = Fixture::new(InstallPhase::Swapped, Some(b"new"));
    let platform = NativePlatform::new();
    *platform.readiness.lock() = ReadyBehavior::FailAll;
    assert!(recover_dead_transaction(&fixture.record_path, &platform).is_err());
    assert!(fixture.record.target.exists());
    fixture.assert_target(b"old");

    let recovery = NativePlatform::new();
    assert_eq!(
        recover_dead_transaction(&fixture.record_path, &recovery).unwrap(),
        RecoveryAction::RestoreRollback,
    );
    finish_ready_cleanup(&fixture.record_path, &fixture.record.token, &recovery).unwrap();
    fixture.assert_clean();
    assert!(
        recover_startup_update(fixture.root.path(), &recovery)
            .unwrap()
            .is_none()
    );
}

#[test]
fn injected_native_swap_failure_keeps_old_target_and_recovers_idempotently() {
    let mut fixture = Fixture::new(InstallPhase::HandedOff, Some(b"old"));
    fs::remove_file(&fixture.record.backup).unwrap();
    fixture.record.helper = Some(ProcessIdentity {
        pid: 7001,
        start: 9001,
    });
    fixture.persist();
    let crashing = NativePlatform::new();
    crashing.fail_swap.store(true, Ordering::Release);
    assert!(run_helper(&fixture.record_path, &fixture.record.token, &crashing).is_err());
    fixture.assert_target(b"old");

    let recovery = NativePlatform::new();
    assert_eq!(
        recover_dead_transaction(&fixture.record_path, &recovery).unwrap(),
        RecoveryAction::CleanAndContinue,
    );
    fixture.assert_clean();
    assert!(
        recover_startup_update(fixture.root.path(), &recovery)
            .unwrap()
            .is_none()
    );
    assert!(
        recover_startup_update(fixture.root.path(), &recovery)
            .unwrap()
            .is_none()
    );
}

#[test]
fn swap_and_rollback_fault_cuts_recover_through_the_same_entrypoint() {
    for cut in [
        UpdateCut::BeforeSwap,
        UpdateCut::AfterSwap,
        UpdateCut::BeforePhase(InstallPhase::Swapped),
        UpdateCut::AfterPhase(InstallPhase::Swapped),
        UpdateCut::BeforeRollback,
        UpdateCut::AfterRollback,
        UpdateCut::BeforePhase(InstallPhase::Rollback),
        UpdateCut::AfterPhase(InstallPhase::Rollback),
        UpdateCut::BeforePhase(InstallPhase::Ready),
        UpdateCut::AfterPhase(InstallPhase::Ready),
    ] {
        let mut fixture = Fixture::new(InstallPhase::HandedOff, Some(b"old"));
        fs::remove_file(&fixture.record.backup).unwrap();
        fixture.record.helper = Some(ProcessIdentity {
            pid: 7001,
            start: 9001,
        });
        fixture.persist();
        let crashing = NativePlatform::failing(cut);
        *crashing.readiness.lock() = ReadyBehavior::FailCandidate;
        let _ = run_helper(&fixture.record_path, &fixture.record.token, &crashing);
        assert!(fixture.record.target.exists(), "target missing at {cut:?}");

        let recovery = NativePlatform::new();
        *recovery.readiness.lock() = ReadyBehavior::FailCandidate;
        let _ = recover_startup_update(fixture.root.path(), &recovery);
        if fixture.record_path.exists() {
            let persisted = load_record(&fixture.record_path).unwrap();
            if persisted.phase == InstallPhase::Ready {
                finish_ready_cleanup(&fixture.record_path, &persisted.token, &recovery).unwrap();
            } else {
                let _ = recover_startup_update(fixture.root.path(), &recovery);
            }
        }
        assert!(
            fixture.record.target.exists(),
            "recovery lost target at {cut:?}"
        );
        assert!(
            recover_startup_update(fixture.root.path(), &recovery)
                .unwrap()
                .is_none(),
            "second recovery at {cut:?}"
        );
        assert!(
            recover_startup_update(fixture.root.path(), &recovery)
                .unwrap()
                .is_none(),
            "idempotent recovery at {cut:?}"
        );
        assert_no_update_residue(fixture.root.path());
    }
}

#[test]
fn transaction_and_readiness_tokens_are_authoritative() {
    let mut fixture = Fixture::new(InstallPhase::HandedOff, Some(b"old"));
    fixture.record.helper = Some(ProcessIdentity {
        pid: 7001,
        start: 9001,
    });
    fixture.persist();
    let platform = NativePlatform::new();
    assert!(matches!(
        run_helper(&fixture.record_path, &"00".repeat(16), &platform),
        Err(UpdateError::ReadinessAuthorization)
    ));
    fixture.assert_target(b"old");

    let readiness = StartupReadiness {
        record_path: fixture.record_path.clone(),
        transaction_token: "00".repeat(16),
        readiness_token: "22".repeat(16),
        address: "127.0.0.1:9".parse().unwrap(),
    };
    assert!(matches!(
        readiness.signal_ready(),
        Err(UpdateError::ReadinessAuthorization)
    ));

    for role in ["candidate", "rollback"] {
        let args = [
            OsString::from(format!("--update-{role}={}", "22".repeat(16))),
            OsString::from(format!("--update-record={}", fixture.record_path.display())),
            OsString::from(format!("--update-token={}", fixture.record.token)),
            OsString::from("--update-ready=127.0.0.1:9"),
        ];
        let intent = parse_startup_update_args(args).unwrap();
        assert!(
            matches!(
                (role, intent),
                ("candidate", StartupIntent::Candidate(_))
                    | ("rollback", StartupIntent::Rollback(_))
            ),
            "role-specific token was not isolated for {role}",
        );
    }
    assert!(
        parse_startup_update_args([
            OsString::from(format!("--update-candidate={}", "22".repeat(16))),
            OsString::from(format!("--update-rollback={}", "33".repeat(16))),
            OsString::from(format!("--update-record={}", fixture.record_path.display())),
            OsString::from(format!("--update-token={}", fixture.record.token)),
            OsString::from("--update-ready=127.0.0.1:9"),
        ])
        .is_err()
    );

    fixture.record.phase = InstallPhase::Ready;
    fixture.persist();
    assert!(matches!(
        finish_ready_cleanup(&fixture.record_path, &"00".repeat(16), &platform),
        Err(UpdateError::ReadinessAuthorization)
    ));
    fixture.assert_target(b"old");
}

#[test]
fn part_archive_staging_helper_and_cleanup_cut_residue_is_removed() {
    for cut in [
        UpdateCut::BeforePartDownload,
        UpdateCut::AfterPartDownload,
        UpdateCut::BeforeVerification,
        UpdateCut::AfterVerification,
        UpdateCut::BeforeStaging,
        UpdateCut::AfterStaging,
        UpdateCut::BeforeHelperCopy,
        UpdateCut::AfterHelperCopy,
        UpdateCut::BeforeHandoff,
        UpdateCut::AfterHandoff,
    ] {
        let phase = match cut {
            UpdateCut::BeforePartDownload | UpdateCut::AfterPartDownload => {
                InstallPhase::Downloading
            }
            UpdateCut::BeforeVerification | UpdateCut::AfterVerification => {
                InstallPhase::Downloading
            }
            UpdateCut::BeforeStaging | UpdateCut::AfterStaging => InstallPhase::Verified,
            UpdateCut::BeforeHelperCopy | UpdateCut::AfterHelperCopy => InstallPhase::Prepared,
            UpdateCut::BeforeHandoff => InstallPhase::Prepared,
            UpdateCut::AfterHandoff => InstallPhase::HandedOff,
            _ => unreachable!(),
        };
        let fixture = Fixture::new(phase, Some(b"old"));
        let platform = NativePlatform::failing(cut);
        assert!(
            platform.checkpoint(cut).is_err(),
            "fault hook did not fire at {cut:?}"
        );
        *platform.fail_cut.lock() = None;
        assert_eq!(
            recover_dead_transaction(&fixture.record_path, &platform).unwrap(),
            RecoveryAction::CleanAndContinue,
            "{cut:?}"
        );
        fixture.assert_target(b"old");
        fixture.assert_clean();
        assert!(
            recover_startup_update(fixture.root.path(), &platform)
                .unwrap()
                .is_none()
        );
        assert!(
            recover_startup_update(fixture.root.path(), &platform)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn cleanup_fsync_cuts_retry_without_artifact_or_helper_leaks() {
    for cut in [UpdateCut::BeforeCleanup, UpdateCut::AfterCleanup] {
        let fixture = Fixture::new(InstallPhase::Ready, Some(b"new"));
        let crashing = NativePlatform::failing(cut);
        assert!(
            finish_ready_cleanup(&fixture.record_path, &fixture.record.token, &crashing,).is_err()
        );
        assert!(fixture.record.target.exists(), "{cut:?}");

        let recovery = NativePlatform::new();
        if fixture.record_path.exists() {
            assert_eq!(
                recover_dead_transaction(&fixture.record_path, &recovery).unwrap(),
                RecoveryAction::FinishCleanup,
            );
        }
        fixture.assert_target(b"new");
        fixture.assert_clean();
        assert!(
            recover_startup_update(fixture.root.path(), &recovery)
                .unwrap()
                .is_none()
        );
        assert!(
            recover_startup_update(fixture.root.path(), &recovery)
                .unwrap()
                .is_none()
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_swap_uses_native_hard_link_backup_and_atomic_rename() {
    let root = TempDir::new().unwrap();
    let target = root.path().join("vosu");
    let staged = root.path().join("stage");
    let backup = root.path().join("backup");
    write_launchable(&target, b"old");
    write_launchable(&staged, b"new");
    vosu::updater::NativePlatform
        .atomic_swap(&target, &staged, &backup)
        .unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(fs::read(&backup).unwrap(), b"old");
    assert!(!staged.exists());
}

#[cfg(windows)]
#[test]
fn windows_locked_target_preserves_old_executable_until_lock_release() {
    use std::fs::OpenOptions;
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    let root = TempDir::new().unwrap();
    let target = root.path().join("vosu.exe");
    let staged = root.path().join("stage.exe");
    let backup = root.path().join("backup.exe");
    write_launchable(&target, b"old");
    write_launchable(&staged, b"new");
    let lock = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&target)
        .unwrap();
    let platform = vosu::updater::NativePlatform;
    assert!(platform.atomic_swap(&target, &staged, &backup).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"old");
    drop(lock);
    platform.atomic_swap(&target, &staged, &backup).unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(fs::read(&backup).unwrap(), b"old");
}

fn assert_no_update_residue(root: &Path) {
    let mut pending = vec![root.to_owned()];
    let mut residue = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path.clone());
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(".vosu-update")
                || name.starts_with(".vosu-stage")
                || name.starts_with(".vosu-backup-")
                || name.starts_with(".vosu-update-helper-")
            {
                residue.push(path);
            }
        }
    }
    assert!(residue.is_empty(), "updater residue: {residue:?}");
}
