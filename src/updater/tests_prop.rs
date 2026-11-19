// Model-based property test for update crash recovery (transaction.rs and
// friends). This drives `TransactionGuard`, `run_helper` and
// `recover_dead_transaction` exactly as the production lifecycle does, with
// an injected crash cut at a random point and, optionally, a second
// injected crash during recovery itself.
//
// It deliberately does not go through `prepare_update` (async, needs a real
// network download and a real archive): that function's extraction and
// verification logic has its own coverage in `tests_platform.rs` and
// `prepare.rs`'s own tests. Instead this model manufactures the staging
// file and digests `prepare_update` would have produced, so the crash-cut
// and recovery machinery in `transaction.rs`/`helper.rs` runs unmodified.
//
// `ModelPlatform::readiness_override` always succeeds without spawning a
// process, using the seam `launch_and_wait_ready` exposes exactly for this
// ("tests may complete or fail a launch without spawning a second copy of
// the test runner"). That means the rollback-launch-failure path
// (`BeforeRollback`/`AfterRollback`) is out of scope here; it is a
// different cause (a candidate that never becomes ready) from the crash
// cuts this test injects.

use proptest::prelude::*;

/// Every `UpdateCut` this model's forward pass and recovery pass can hit, in
/// the order a real run reaches them. Built once; used both to choose where
/// to inject a crash and, implicitly, to document the ordering.
fn injectable_cuts() -> Vec<UpdateCut> {
    vec![
        UpdateCut::BeforeRecordCreate,
        UpdateCut::AfterRecordCreate,
        UpdateCut::BeforePhase(InstallPhase::Metadata),
        UpdateCut::AfterPhase(InstallPhase::Metadata),
        UpdateCut::BeforePhase(InstallPhase::Downloading),
        UpdateCut::AfterPhase(InstallPhase::Downloading),
        UpdateCut::BeforePhase(InstallPhase::Verified),
        UpdateCut::AfterPhase(InstallPhase::Verified),
        UpdateCut::BeforePhase(InstallPhase::Prepared),
        UpdateCut::AfterPhase(InstallPhase::Prepared),
        UpdateCut::BeforeHandoff,
        UpdateCut::BeforePhase(InstallPhase::HandedOff),
        UpdateCut::AfterPhase(InstallPhase::HandedOff),
        UpdateCut::AfterHandoff,
        UpdateCut::BeforeSwap,
        UpdateCut::AfterSwap,
        UpdateCut::BeforePhase(InstallPhase::Swapped),
        UpdateCut::AfterPhase(InstallPhase::Swapped),
        UpdateCut::BeforePhase(InstallPhase::Ready),
        UpdateCut::AfterPhase(InstallPhase::Ready),
    ]
}

/// A platform whose owner and helper are always reported dead (this model
/// has no concurrent live owner to test) and whose readiness always
/// succeeds without spawning, via the seam built for exactly that. Crash
/// injection is one-shot: firing a cut clears it, so the "process" only
/// crashes once per `fail_cut` assignment, matching a real crash at one
/// exact point rather than a repeating fault.
struct ModelPlatform {
    identity: ProcessIdentity,
    nonce: AtomicU64,
    fail_cut: AtomicU64,
}

impl ModelPlatform {
    fn new() -> Self {
        Self {
            identity: ProcessIdentity { pid: 42, start: 7 },
            nonce: AtomicU64::new(1),
            fail_cut: AtomicU64::new(u64::MAX),
        }
    }

    fn set_fail_cut(&self, cut: Option<UpdateCut>) {
        self.fail_cut
            .store(cut.map(cut_code).unwrap_or(u64::MAX), Ordering::SeqCst);
    }
}

impl UpdatePlatform for ModelPlatform {
    fn current_identity(&self) -> io::Result<ProcessIdentity> {
        Ok(self.identity.clone())
    }
    fn identity_for_pid(&self, pid: u32) -> io::Result<ProcessIdentity> {
        Ok(ProcessIdentity { pid, start: self.identity.start })
    }
    fn identity_is_alive(&self, _: &ProcessIdentity) -> io::Result<bool> {
        Ok(false)
    }
    fn random_128(&self) -> io::Result<[u8; 16]> {
        let mut result = [0; 16];
        let value = self.nonce.fetch_add(1, Ordering::SeqCst);
        result[..8].copy_from_slice(&value.to_le_bytes());
        Ok(result)
    }
    fn atomic_swap(&self, target: &Path, staged: &Path, backup: &Path) -> io::Result<()> {
        atomic_swap(target, staged, backup)
    }
    fn restore_backup(&self, target: &Path, backup: &Path) -> io::Result<()> {
        restore_backup(target, backup)
    }
    fn spawn(&self, _: &Path, _: &[OsString], _: &Path) -> io::Result<Child> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "ModelPlatform never spawns; readiness_override always short-circuits first",
        ))
    }
    fn terminate_and_wait(&self, _: &ProcessIdentity, _: Duration) -> io::Result<()> {
        Ok(())
    }
    fn checkpoint(&self, cut: UpdateCut) -> io::Result<()> {
        if self.fail_cut.load(Ordering::SeqCst) == cut_code(cut) {
            self.fail_cut.store(u64::MAX, Ordering::SeqCst);
            Err(io::Error::new(io::ErrorKind::Interrupted, "injected crash cut"))
        } else {
            Ok(())
        }
    }
    fn readiness_override(&self, _: bool) -> Option<io::Result<()>> {
        Some(Ok(()))
    }
}

/// Runs one simulated update lifecycle: begin, metadata, download, verify,
/// prepare (staging written with `new_bytes`), hand off, and the helper-side
/// swap/ready sequence, with `main_cut` injected somewhere in that sequence.
/// If the record is still on disk afterward, recovers it (with `recovery_cut`
/// injected on the first attempt), then recovers again up to a few more
/// times until no record remains or a quarantine is reached.
fn run_crash_recovery_model(
    old_bytes: &[u8],
    new_bytes: &[u8],
    main_cut: Option<UpdateCut>,
    recovery_cut: Option<UpdateCut>,
) -> Result<(), TestCaseError> {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("vosu");
    fs::write(&target, old_bytes).unwrap();
    let platform = ModelPlatform::new();
    platform.set_fail_cut(main_cut);

    let maybe_guard = TransactionGuard::begin(temp.path(), &target, &platform);
    if let Ok(mut guard) = maybe_guard {
        let record_path = temp.path().join(INSTALL_RECORD_NAME);
        let reached_handoff = (|| -> Result<(), UpdateError> {
            let archive_digest: [u8; 32] = Sha256::digest(new_bytes).into();
            guard.set_metadata(archive_digest)?;
            guard.persist_phase(InstallPhase::Downloading)?;
            fs::write(&guard.record.part, b"partial-archive-bytes").unwrap();
            guard.persist_phase(InstallPhase::Verified)?;
            guard.record.new_digest = Some(hex(&Sha256::digest(new_bytes)));
            fs::write(&guard.record.staging, new_bytes).unwrap();
            guard.persist_phase(InstallPhase::Prepared)?;
            let helper_identity = platform.current_identity()?;
            guard.hand_off(helper_identity)?;
            Ok(())
        })();
        match reached_handoff {
            Ok(()) => {
                guard.confirm_hand_off();
                let token = guard.record.token.clone();
                drop(guard);
                let _ = run_helper(&record_path, &token, &platform);
            }
            Err(_) => {
                // A crash during setup, before handoff was confirmed: the
                // process really exited here, so its destructor never ran.
                std::mem::forget(guard);
            }
        }
    }

    let record_path = temp.path().join(INSTALL_RECORD_NAME);
    if record_path.exists() {
        if let Some(cut) = recovery_cut {
            platform.set_fail_cut(Some(cut));
            let _ = recover_dead_transaction(&record_path, &platform);
            platform.set_fail_cut(None);
        }
        for _ in 0..4 {
            if !record_path.exists() {
                break;
            }
            match recover_dead_transaction(&record_path, &platform) {
                Ok(_) => {}
                Err(UpdateError::Quarantined(reason)) => {
                    return Err(TestCaseError::fail(format!(
                        "unexpected quarantine in a model with no injected corruption: {reason}"
                    )));
                }
                Err(_) => break,
            }
        }
    }

    // Idempotency: once the transaction is apparently finished, recovering
    // again (if there is still anything to recover) must not change the
    // target and must not need a further attempt.
    if record_path.exists() {
        let before = fs::read(&target).ok();
        let _ = recover_dead_transaction(&record_path, &platform);
        let after = fs::read(&target).ok();
        prop_assert_eq!(before, after, "idempotent recovery changed the target");
    }

    prop_assert!(target.exists(), "target executable went missing");
    let final_bytes = fs::read(&target).unwrap();
    prop_assert!(
        final_bytes == old_bytes || final_bytes == new_bytes,
        "target is neither the old nor the new executable: {} bytes",
        final_bytes.len()
    );

    let mut leftover = Vec::new();
    for entry in fs::read_dir(temp.path()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "vosu" {
            continue;
        }
        if name == ".cache" {
            prop_assert!(
                fs::read_dir(entry.path()).unwrap().next().is_none(),
                ".cache retained a leftover helper copy"
            );
            continue;
        }
        leftover.push(name);
    }
    prop_assert!(
        leftover.is_empty(),
        "leftover update artifacts after recovery settled: {leftover:?}"
    );

    Ok(())
}

fn cut_strategy() -> impl Strategy<Value = Option<UpdateCut>> {
    let cuts = injectable_cuts();
    let len = cuts.len();
    prop::option::weighted(0.85, 0..len).prop_map(move |index| index.map(|index| cuts[index]))
}

proptest! {
    #![proptest_config(crate::proptest_support::vosu_proptest_config())]

    /// After a crash injected anywhere in a full update lifecycle (and,
    /// optionally, a second crash during the recovery that follows it),
    /// recovery always settles into a consistent state: the installed
    /// executable is exactly the old or exactly the new bytes, never
    /// missing or partial; no transaction artifacts remain outside an
    /// (unexpected, here) quarantine; and recovering again afterward is a
    /// no-op.
    #[test]
    fn prop_crash_recovery_settles_to_old_or_new_with_no_leftovers(
        old_bytes in prop::collection::vec(any::<u8>(), 1..48),
        new_bytes in prop::collection::vec(any::<u8>(), 1..48),
        main_cut in cut_strategy(),
        recovery_cut in cut_strategy(),
    ) {
        prop_assume!(old_bytes != new_bytes);
        run_crash_recovery_model(&old_bytes, &new_bytes, main_cut, recovery_cut)?;
    }
}
