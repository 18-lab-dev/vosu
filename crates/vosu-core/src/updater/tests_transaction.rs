    fn cut_code(cut: UpdateCut) -> u64 {
        match cut {
            UpdateCut::BeforePhase(phase) => 10 + phase as u64 * 2,
            UpdateCut::AfterPhase(phase) => 11 + phase as u64 * 2,
            UpdateCut::BeforeRecordCreate => 1,
            UpdateCut::AfterRecordCreate => 2,
            UpdateCut::BeforePartDownload => 30,
            UpdateCut::AfterPartDownload => 31,
            UpdateCut::BeforeVerification => 32,
            UpdateCut::AfterVerification => 33,
            UpdateCut::BeforeStaging => 34,
            UpdateCut::AfterStaging => 35,
            UpdateCut::BeforeHelperCopy => 36,
            UpdateCut::AfterHelperCopy => 37,
            UpdateCut::BeforeHandoff => 38,
            UpdateCut::AfterHandoff => 39,
            UpdateCut::BeforeSwap => 40,
            UpdateCut::AfterSwap => 41,
            UpdateCut::BeforeRollback => 42,
            UpdateCut::AfterRollback => 43,
            UpdateCut::BeforeCleanup => 44,
            UpdateCut::AfterCleanup => 45,
        }
    }

    struct FakePlatform {
        alive: AtomicBool,
        helper_alive: AtomicBool,
        identity: ProcessIdentity,
        nonce: AtomicU64,
        fail_cut: AtomicU64,
    }
    impl FakePlatform {
        fn new() -> Self {
            Self {
                alive: AtomicBool::new(false),
                helper_alive: AtomicBool::new(false),
                identity: ProcessIdentity { pid: 42, start: 7 },
                nonce: AtomicU64::new(1),
                fail_cut: AtomicU64::new(u64::MAX),
            }
        }
    }
    impl UpdatePlatform for FakePlatform {
        fn current_identity(&self) -> io::Result<ProcessIdentity> {
            Ok(self.identity.clone())
        }
        fn identity_for_pid(&self, pid: u32) -> io::Result<ProcessIdentity> {
            Ok(ProcessIdentity {
                pid,
                start: self.identity.start,
            })
        }
        fn identity_is_alive(&self, identity: &ProcessIdentity) -> io::Result<bool> {
            Ok(if identity.pid == 99 {
                self.helper_alive.load(Ordering::SeqCst)
            } else {
                self.alive.load(Ordering::SeqCst)
            })
        }
        fn random_128(&self) -> io::Result<[u8; 16]> {
            let mut result = [0; 16];
            result[..8].copy_from_slice(&self.nonce.fetch_add(1, Ordering::SeqCst).to_le_bytes());
            Ok(result)
        }
        fn atomic_swap(&self, target: &Path, staged: &Path, backup: &Path) -> io::Result<()> {
            atomic_swap(target, staged, backup)
        }
        fn restore_backup(&self, target: &Path, backup: &Path) -> io::Result<()> {
            restore_backup(target, backup)
        }
        fn spawn(&self, _: &Path, _: &[OsString], _: &Path) -> io::Result<Child> {
            Err(io::Error::new(io::ErrorKind::Unsupported, "test"))
        }
        fn terminate_and_wait(&self, identity: &ProcessIdentity, _: Duration) -> io::Result<()> {
            if identity.pid == 99 {
                self.helper_alive.store(false, Ordering::SeqCst);
            } else {
                self.alive.store(false, Ordering::SeqCst);
            }
            Ok(())
        }
        fn checkpoint(&self, cut: UpdateCut) -> io::Result<()> {
            if self.fail_cut.load(Ordering::SeqCst) == cut_code(cut) {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "injected crash cut",
                ))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn startup_dispatch_runs_ordinary_recovery_probe_and_binds_candidate_context() {
        let temp = tempfile::tempdir().unwrap();
        let platform = FakePlatform::new();
        assert_eq!(
            dispatch_startup_update(Vec::<OsString>::new(), temp.path(), &platform).unwrap(),
            StartupDispatch::Ordinary { recovery: None }
        );
        let dispatch = dispatch_startup_update(
            os_arguments(&[
                "--update-candidate",
                "--update-ready=127.0.0.1:24050",
                &format!("--update-token={TOKEN}"),
                "--update-record=/tmp/record",
            ]),
            temp.path(),
            &platform,
        )
        .unwrap();
        assert!(matches!(
            dispatch,
            StartupDispatch::Candidate(StartupReadiness {
                transaction_token,
                ..
            }) if transaction_token == TOKEN
        ));
    }

    fn record_fixture(
        temp: &tempfile::TempDir,
        phase: InstallPhase,
        target_bytes: &[u8],
    ) -> (PathBuf, InstallRecord) {
        let executable_suffix = if cfg!(windows) { ".exe" } else { "" };
        let target = temp.path().join(format!("vosu{executable_suffix}"));
        fs::write(&target, target_bytes).unwrap();
        let old = hex(&Sha256::digest(b"old"));
        let new = hex(&Sha256::digest(b"new"));
        let path = temp.path().join(INSTALL_RECORD_NAME);
        let cache = temp.path().join(".cache");
        fs::create_dir(&cache).unwrap();
        let work = temp.path().join(".vosu-update-work-aa");
        fs::create_dir(&work).unwrap();
        let record = InstallRecord {
            nonce: "aa".into(),
            token: "t".into(),
            owner: ProcessIdentity { pid: 1, start: 1 },
            helper: None,
            original_args: vec![],
            working_directory: EncodedOsString::capture(temp.path().as_os_str().to_owned()),
            target: target.clone(),
            old_digest: old,
            archive_digest: Some(hex(&Sha256::digest(b"archive"))),
            new_digest: Some(new),
            phase,
            part: work.join("archive.part"),
            staging: temp
                .path()
                .join(format!(".vosu-stage-aa{executable_suffix}")),
            backup: temp.path().join(format!(".vosu-backup-aa{executable_suffix}")),
            helper_path: cache.join(format!(".vosu-update-helper-aa{executable_suffix}")),
            work,
        };
        persist_record(&path, &record).unwrap();
        (path, record)
    }

    #[test]
    fn install_record_paths_outside_transaction_root_are_quarantined() {
        let temp = tempfile::tempdir().unwrap();
        let (record_path, record) = record_fixture(&temp, InstallPhase::Created, b"old");
        let outside = temp.path().with_extension("outside");

        let mut outside_target = record.clone();
        outside_target.target = outside.join(
            record
                .target
                .file_name()
                .expect("fixture target must have a file name"),
        );
        assert!(matches!(
            validate_record_paths(&record_path, &outside_target),
            Err(UpdateError::Quarantined(_))
        ));

        let mut outside_artifact = record;
        outside_artifact.part = outside.join("archive.part");
        assert!(matches!(
            validate_record_paths(&record_path, &outside_artifact),
            Err(UpdateError::Quarantined(_))
        ));
    }

    #[test]
    fn ordinary_role_dispatch_defers_recovery_until_guards_accept_startup() {
        let temp = tempfile::tempdir().unwrap();
        let (record_path, _) = record_fixture(&temp, InstallPhase::Created, b"old");
        let platform = FakePlatform::new();

        assert_eq!(
            dispatch_startup_role(Vec::<OsString>::new(), &platform).unwrap(),
            StartupDispatch::Ordinary { recovery: None }
        );
        assert!(
            record_path.exists(),
            "role dispatch must not mutate recovery artifacts"
        );

        assert_eq!(
            recover_startup_update(temp.path(), &platform).unwrap(),
            Some(RecoveryAction::CleanAndContinue)
        );
        assert!(!record_path.exists());
    }

    #[test]
    fn every_phase_write_has_deterministic_before_and_after_kill_cuts() {
        let phases = [
            InstallPhase::Created,
            InstallPhase::Metadata,
            InstallPhase::Downloading,
            InstallPhase::Verified,
            InstallPhase::Prepared,
            InstallPhase::HandedOff,
            InstallPhase::Swapped,
            InstallPhase::Rollback,
            InstallPhase::Ready,
        ];
        for phase in phases {
            for after in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let target = temp.path().join("vosu");
                fs::write(&target, b"old").unwrap();
                let platform = FakePlatform::new();
                let mut guard = TransactionGuard::begin(temp.path(), &target, &platform).unwrap();
                let cut = if after {
                    UpdateCut::AfterPhase(phase)
                } else {
                    UpdateCut::BeforePhase(phase)
                };
                platform.fail_cut.store(cut_code(cut), Ordering::SeqCst);
                assert!(guard.persist_phase(phase).is_err(), "{cut:?}");
                platform.fail_cut.store(u64::MAX, Ordering::SeqCst);
                let durable = load_record(&temp.path().join(INSTALL_RECORD_NAME)).unwrap();
                assert_eq!(
                    durable.phase,
                    if after { phase } else { InstallPhase::Created }
                );
                guard.cancel().unwrap();
            }
        }
    }

    #[test]
    fn all_mutation_boundaries_have_unique_injectable_cuts() {
        let cuts = [
            UpdateCut::BeforeRecordCreate,
            UpdateCut::AfterRecordCreate,
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
            UpdateCut::BeforeSwap,
            UpdateCut::AfterSwap,
            UpdateCut::BeforeRollback,
            UpdateCut::AfterRollback,
            UpdateCut::BeforeCleanup,
            UpdateCut::AfterCleanup,
        ];
        let mut codes = std::collections::BTreeSet::new();
        for cut in cuts {
            assert!(codes.insert(cut_code(cut)), "duplicate cut {cut:?}");
            let platform = FakePlatform::new();
            platform.fail_cut.store(cut_code(cut), Ordering::SeqCst);
            assert_eq!(
                platform.checkpoint(cut).unwrap_err().kind(),
                io::ErrorKind::Interrupted
            );
        }
    }

    #[test]
    fn recovery_table_covers_every_phase_and_swap_cut() {
        for phase in [
            InstallPhase::Created,
            InstallPhase::Metadata,
            InstallPhase::Downloading,
            InstallPhase::Verified,
            InstallPhase::Prepared,
            InstallPhase::HandedOff,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let (_, record) = record_fixture(&temp, phase, b"old");
            assert_eq!(
                classify_recovery(&record).unwrap(),
                RecoveryAction::CleanAndContinue,
                "{phase:?}"
            );
        }
        for phase in [
            InstallPhase::Prepared,
            InstallPhase::HandedOff,
            InstallPhase::Swapped,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let (_, record) = record_fixture(&temp, phase, b"new");
            assert_eq!(
                classify_recovery(&record).unwrap(),
                RecoveryAction::ResumeCandidate,
                "{phase:?}"
            );
        }
        let temp = tempfile::tempdir().unwrap();
        let (_, record) = record_fixture(&temp, InstallPhase::Rollback, b"new");
        fs::write(&record.backup, b"old").unwrap();
        assert_eq!(
            classify_recovery(&record).unwrap(),
            RecoveryAction::RestoreRollback
        );
        let temp = tempfile::tempdir().unwrap();
        let (_, record) = record_fixture(&temp, InstallPhase::Rollback, b"old");
        assert_eq!(
            classify_recovery(&record).unwrap(),
            RecoveryAction::RestoreRollback
        );
        let temp = tempfile::tempdir().unwrap();
        let (_, record) = record_fixture(&temp, InstallPhase::Ready, b"new");
        assert_eq!(
            classify_recovery(&record).unwrap(),
            RecoveryAction::FinishCleanup
        );
    }

    #[test]
    fn guard_is_single_flight_and_cleans_every_prehandoff_artifact() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("vosu");
        fs::write(&target, b"old").unwrap();
        let platform = FakePlatform::new();
        let mut guard = TransactionGuard::begin(temp.path(), &target, &platform).unwrap();
        fs::write(&guard.record.part, b"part").unwrap();
        fs::create_dir(&guard.record.staging).unwrap();
        fs::write(&guard.record.helper_path, b"helper").unwrap();
        platform.alive.store(true, Ordering::SeqCst);
        assert!(matches!(
            TransactionGuard::begin(temp.path(), &target, &platform),
            Err(UpdateError::AlreadyRunning)
        ));
        platform.alive.store(false, Ordering::SeqCst);
        assert!(matches!(
            TransactionGuard::begin(temp.path(), &target, &platform),
            Err(UpdateError::Quarantined(_))
        ));
        guard.persist_phase(InstallPhase::Downloading).unwrap();
        drop(guard);
        assert!(!temp.path().join(INSTALL_RECORD_NAME).exists());
        assert!(!temp.path().join(".cache").read_dir().unwrap().any(|_| true));
    }

    #[test]
    fn live_owner_blocks_and_unknown_target_is_quarantined() {
        let temp = tempfile::tempdir().unwrap();
        let (path, mut record) = record_fixture(&temp, InstallPhase::Prepared, b"old");
        let platform = FakePlatform::new();
        platform.alive.store(true, Ordering::SeqCst);
        assert!(matches!(
            recover_dead_transaction(&path, &platform),
            Err(UpdateError::LiveOwner)
        ));
        platform.alive.store(false, Ordering::SeqCst);
        fs::write(&record.target, b"foreign").unwrap();
        assert!(matches!(
            recover_dead_transaction(&path, &platform),
            Err(UpdateError::Quarantined(_))
        ));
        assert!(!path.exists());
        assert!(temp.path().read_dir().unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .contains("quarantine")
        }));
        assert!(matches!(
            TransactionGuard::begin(temp.path(), &record.target, &platform),
            Err(UpdateError::Quarantined(_))
        ));
        record.phase = InstallPhase::Created;
    }
    #[test]
    fn a_live_helper_blocks_recovery_even_after_the_owner_exits() {
        let temp = tempfile::tempdir().unwrap();
        let (path, mut record) = record_fixture(&temp, InstallPhase::HandedOff, b"old");
        record.helper = Some(ProcessIdentity { pid: 99, start: 3 });
        persist_record(&path, &record).unwrap();
        let platform = FakePlatform::new();
        platform.helper_alive.store(true, Ordering::SeqCst);
        assert!(matches!(
            recover_dead_transaction(&path, &platform),
            Err(UpdateError::LiveOwner)
        ));
        assert!(path.exists());
    }

    #[test]
    fn dead_atomic_create_record_is_adopted_and_fully_cleaned() {
        let temp = tempfile::tempdir().unwrap();
        let (canonical, record) = record_fixture(&temp, InstallPhase::Created, b"old");
        fs::write(&record.part, b"partial").unwrap();
        let create = temp.path().join(".vosu-update-create-aa");
        fs::rename(&canonical, &create).unwrap();
        let platform = FakePlatform::new();
        let guard = TransactionGuard::begin(temp.path(), &record.target, &platform).unwrap();
        assert!(!create.exists());
        assert!(!record.work.exists());
        guard.cancel().unwrap();
    }
