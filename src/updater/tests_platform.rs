    fn write_update_zip(path: &Path, executable_name: &str, members: &[(&str, &[u8])]) {
        use zip::write::SimpleFileOptions;
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        for (name, bytes) in members {
            let options = if *name == executable_name {
                SimpleFileOptions::default().unix_permissions(0o755)
            } else {
                SimpleFileOptions::default()
            };
            zip.start_file(*name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn truncated_unpublished_create_record_is_discarded_without_quarantine() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("vosu");
        fs::write(&target, b"old").unwrap();
        let create = temp.path().join(".vosu-update-create-dead");
        fs::write(&create, b"{").unwrap();
        let platform = FakePlatform::new();
        let guard = TransactionGuard::begin(temp.path(), &target, &platform).unwrap();
        assert!(!create.exists());
        assert!(!temp.path().join(".vosu-update.quarantine-0").exists());
        guard.cancel().unwrap();
    }

    #[test]
    fn extraction_requires_exact_root_members_and_hashes_executable_not_archive() {
        let temp = tempfile::tempdir().unwrap();
        let executable_name = if cfg!(windows) { "vosu.exe" } else { "vosu" };
        let archive = temp.path().join("update.zip");
        write_update_zip(
            &archive,
            executable_name,
            &[
                (executable_name, b"new executable"),
                ("LICENSE", b"license"),
            ],
        );
        let staging = temp.path().join(executable_name);
        let target = temp.path().join(executable_name);
        extract_staged_binary(&archive, &staging, &target, &CancellationToken::new()).unwrap();
        assert_eq!(fs::read(&staging).unwrap(), b"new executable");
        assert_ne!(
            digest_file(&archive).unwrap(),
            digest_file(&staging).unwrap()
        );

        let missing_license = temp.path().join("missing.zip");
        write_update_zip(
            &missing_license,
            executable_name,
            &[(executable_name, b"new executable")],
        );
        assert!(
            extract_staged_binary(
                &missing_license,
                &temp.path().join("missing-stage"),
                &target,
                &CancellationToken::new(),
            )
            .is_err()
        );

        let nested = temp.path().join("nested.zip");
        write_update_zip(
            &nested,
            executable_name,
            &[
                (&format!("dir/{executable_name}"), b"new executable"),
                ("LICENSE", b"license"),
            ],
        );
        assert!(
            extract_staged_binary(
                &nested,
                &temp.path().join("nested-stage"),
                &target,
                &CancellationToken::new(),
            )
            .is_err()
        );
        let bounded_stage = temp.path().join("bounded-stage");
        assert!(
            extract_staged_binary_with_limit(
                &archive,
                &bounded_stage,
                &target,
                &CancellationToken::new(),
                8,
            )
            .is_err()
        );
        assert!(fs::metadata(&bounded_stage).unwrap().len() <= 9);
    }
    #[test]
    fn zip_preflight_rejects_entry_count_before_archive_construction() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("many.zip");
        let mut end = [0u8; 22];
        end[..4].copy_from_slice(b"PK\x05\x06");
        end[8..10].copy_from_slice(&11u16.to_le_bytes());
        end[10..12].copy_from_slice(&11u16.to_le_bytes());
        fs::write(&archive, end).unwrap();
        let mut file = File::open(archive).unwrap();
        assert!(preflight_zip_entries(&mut file, 10, &CancellationToken::new()).is_err());
    }
    #[test]
    fn zip_preflight_ignores_payload_eocd_magic() {
        let temp = tempfile::tempdir().unwrap();
        let executable_name = if cfg!(windows) { "vosu.exe" } else { "vosu" };
        let archive = temp.path().join("payload-magic.zip");
        let payload = b"PK\x05\x06".repeat(MAX_ARCHIVE_ENTRIES + 1);
        write_update_zip(
            &archive,
            executable_name,
            &[(executable_name, &payload), ("LICENSE", b"license")],
        );
        let mut file = File::open(archive).unwrap();
        preflight_zip_entries(&mut file, MAX_ARCHIVE_ENTRIES, &CancellationToken::new()).unwrap();
    }

    #[cfg(target_os = "linux")]
    struct ReadyPlatform {
        fake: FakePlatform,
        spawned_pid: AtomicU64,
        rollback_launches: AtomicU64,
    }

    #[cfg(target_os = "linux")]
    impl ReadyPlatform {
        fn new() -> Self {
            Self {
                fake: FakePlatform::new(),
                spawned_pid: AtomicU64::new(0),
                rollback_launches: AtomicU64::new(0),
            }
        }

        fn reap_candidate(&self) {
            let pid = self.spawned_pid.load(Ordering::SeqCst) as libc::pid_t;
            if pid != 0 {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                    libc::waitpid(pid, std::ptr::null_mut(), 0);
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    impl UpdatePlatform for ReadyPlatform {
        fn current_identity(&self) -> io::Result<ProcessIdentity> {
            self.fake.current_identity()
        }
        fn identity_for_pid(&self, pid: u32) -> io::Result<ProcessIdentity> {
            self.fake.identity_for_pid(pid)
        }
        fn identity_is_alive(&self, identity: &ProcessIdentity) -> io::Result<bool> {
            self.fake.identity_is_alive(identity)
        }
        fn random_128(&self) -> io::Result<[u8; 16]> {
            self.fake.random_128()
        }
        fn atomic_swap(&self, target: &Path, staged: &Path, backup: &Path) -> io::Result<()> {
            self.fake.atomic_swap(target, staged, backup)
        }
        fn restore_backup(&self, target: &Path, backup: &Path) -> io::Result<()> {
            self.fake.restore_backup(target, backup)
        }
        fn spawn(&self, _: &Path, args: &[OsString], _: &Path) -> io::Result<Child> {
            let mut address = None;
            let mut transaction_token = None;
            let mut record_path = None;
            for argument in args {
                let text = argument.to_string_lossy();
                if let Some(value) = text.strip_prefix("--update-ready=") {
                    address = Some(value.parse::<SocketAddr>().unwrap());
                } else if let Some(value) = text.strip_prefix("--update-token=") {
                    transaction_token = Some(value.to_owned());
                } else if let Some(value) = text.strip_prefix("--update-record=") {
                    record_path = Some(PathBuf::from(value));
                } else if text == "--update-rollback" {
                    self.rollback_launches.fetch_add(1, Ordering::SeqCst);
                } else {
                    assert_eq!(text, "--update-candidate");
                }
            }
            let address = address.unwrap();
            let transaction_token = transaction_token.unwrap();
            let record_path = record_path.unwrap();
            let mut child = Command::new("/bin/sh")
                .args(["-c", "cat; exec sleep 30"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?;
            let mut input = child.stdout.take().unwrap();
            std::thread::spawn(move || {
                let mut readiness_token = [0u8; 32];
                input.read_exact(&mut readiness_token).unwrap();
                let mut partial = std::net::TcpStream::connect(address).unwrap();
                partial.write_all(&readiness_token[..5]).unwrap();
                drop(partial);
                let mut invalid = std::net::TcpStream::connect(address).unwrap();
                invalid.write_all(transaction_token.as_bytes()).unwrap();
                drop(invalid);
                signal_ready_with_token(&record_path, &transaction_token, &readiness_token, address).unwrap();
            });
            self.spawned_pid
                .store(u64::from(child.id()), Ordering::SeqCst);
            Ok(child)
        }
        fn terminate_and_wait(
            &self,
            identity: &ProcessIdentity,
            timeout: Duration,
        ) -> io::Result<()> {
            self.fake.terminate_and_wait(identity, timeout)
        }
        fn checkpoint(&self, cut: UpdateCut) -> io::Result<()> {
            self.fake.checkpoint(cut)
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn recovery_relaunches_swapped_candidate_and_old_rollback_target() {
        let platform = ReadyPlatform::new();
        let temp = tempfile::tempdir().unwrap();
        let (path, record) = record_fixture(&temp, InstallPhase::HandedOff, b"new");
        fs::write(&record.backup, b"old").unwrap();
        assert_eq!(
            recover_dead_transaction(&path, &platform).unwrap(),
            RecoveryAction::ResumeCandidate
        );
        assert_eq!(load_record(&path).unwrap().phase, InstallPhase::Ready);
        finish_ready_cleanup(&path, &record.token, &platform).unwrap();
        assert!(!path.exists());
        platform.reap_candidate();

        let temp = tempfile::tempdir().unwrap();
        let (path, record) = record_fixture(&temp, InstallPhase::Rollback, b"old");
        assert_eq!(
            recover_dead_transaction(&path, &platform).unwrap(),
            RecoveryAction::RestoreRollback
        );
        assert_eq!(platform.rollback_launches.load(Ordering::SeqCst), 1);
        assert_eq!(load_record(&path).unwrap().phase, InstallPhase::Ready);
        finish_ready_cleanup(&path, &record.token, &platform).unwrap();
        assert!(!path.exists());
        platform.reap_candidate();
    }

    #[test]
    fn dead_prehandoff_phase_cleanup_removes_every_artifact_and_allows_retry() {
        for phase in [
            InstallPhase::Created,
            InstallPhase::Metadata,
            InstallPhase::Downloading,
            InstallPhase::Verified,
            InstallPhase::Prepared,
            InstallPhase::HandedOff,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let (path, record) = record_fixture(&temp, phase, b"old");
            fs::write(&record.part, b"partial").unwrap();
            fs::write(&record.staging, b"staged").unwrap();
            fs::write(&record.helper_path, b"helper").unwrap();
            fs::write(&record.backup, b"backup").unwrap();
            let platform = FakePlatform::new();
            assert_eq!(
                recover_dead_transaction(&path, &platform).unwrap(),
                RecoveryAction::CleanAndContinue,
                "{phase:?}"
            );
            for artifact in [
                &record.work,
                &record.staging,
                &record.helper_path,
                &record.backup,
            ] {
                assert!(!artifact.exists(), "{phase:?}: {}", artifact.display());
            }
            let guard = TransactionGuard::begin(temp.path(), &record.target, &platform).unwrap();
            guard.cancel().unwrap();
        }
    }

    #[test]
    fn unix_swap_keeps_verified_backup_and_restore_is_atomic() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("vosu");
        let staged = temp.path().join("staged");
        let backup = temp.path().join("old");
        fs::write(&target, b"old").unwrap();
        fs::write(&staged, b"new").unwrap();
        atomic_swap(&target, &staged, &backup).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(fs::read(&backup).unwrap(), b"old");
        restore_backup(&target, &backup).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"old");
    }
    #[test]
    fn native_platform_uses_exact_current_process_identity_and_os_randomness() {
        let platform = NativePlatform;
        let current = platform.current_identity().unwrap();
        assert_eq!(current.pid, std::process::id());
        assert_eq!(platform.identity_for_pid(current.pid).unwrap(), current);
        assert!(platform.identity_is_alive(&current).unwrap());
        let first = platform.random_128().unwrap();
        let second = platform.random_128().unwrap();
        assert_ne!(first, second);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn native_termination_refuses_reused_identity_and_reaps_exact_process() {
        let platform = NativePlatform;
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let identity = platform.identity_for_pid(child.id()).unwrap();
        let wrong = ProcessIdentity {
            pid: identity.pid,
            start: identity.start.wrapping_add(1),
        };
        assert_eq!(
            platform
                .terminate_and_wait(&wrong, Duration::from_millis(100))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(child.try_wait().unwrap().is_none());
        platform
            .terminate_and_wait(&identity, Duration::from_secs(2))
            .unwrap();
        assert!(child.wait().unwrap().code().is_none());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn native_termination_accepts_an_already_exited_exact_owner() {
        let platform = NativePlatform;
        let mut child = Command::new("/bin/sleep").arg("0.05").spawn().unwrap();
        let identity = platform.identity_for_pid(child.id()).unwrap();
        child.wait().unwrap();
        platform
            .terminate_and_wait(&identity, Duration::from_millis(100))
            .unwrap();
    }
