use super::*;

/// A create-new transaction record. Before helper handoff, dropping this guard
/// removes every temporary artifact. A handed-off transaction deliberately
/// survives so another process can recover it.
pub struct TransactionGuard<'a, P: UpdatePlatform> {
    pub(super) platform: &'a P,
    pub(super) record_path: PathBuf,
    pub(super) record: InstallRecord,
    pub(super) disarmed: bool,
}

impl<'a, P: UpdatePlatform> TransactionGuard<'a, P> {
    pub fn begin(program_dir: &Path, target: &Path, platform: &'a P) -> Result<Self, UpdateError> {
        fs::create_dir_all(program_dir)?;
        let quarantined = fs::read_dir(program_dir)?
            .filter_map(Result::ok)
            .any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".vosu-update.quarantine-")
            });
        if quarantined {
            return Err(UpdateError::Quarantined(
                "a prior update transaction requires diagnostics".into(),
            ));
        }
        let record_path = program_dir.join(INSTALL_RECORD_NAME);
        if !record_path.exists() {
            for entry in fs::read_dir(program_dir)?.filter_map(Result::ok) {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(&format!("{INSTALL_RECORD_NAME}-")) && name.ends_with(".tmp") {
                    fs::remove_file(entry.path())?;
                    sync_parent(&entry.path())?;
                }
            }
        }
        recover_create_records(program_dir, &record_path, platform)?;
        let nonce = hex(&platform.random_128()?);
        let token = hex(&platform.random_128()?);
        let owner = platform.current_identity()?;
        let old_digest = digest_file(target)?;
        let cache = program_dir.join(".cache");
        ensure_real_directory(&cache)?;
        let executable_suffix = if cfg!(windows) { ".exe" } else { "" };
        let work = program_dir.join(format!(".vosu-update-work-{nonce}"));
        let record = InstallRecord {
            nonce: nonce.clone(),
            token,
            owner,
            helper: None,
            original_args: canonical_original_args(std::env::args_os().skip(1)),
            working_directory: EncodedOsString::capture(std::env::current_dir()?.into_os_string()),
            target: target.to_owned(),
            old_digest,
            archive_digest: None,
            new_digest: None,
            phase: InstallPhase::Created,
            part: work.join("archive.part"),
            staging: program_dir.join(format!(".vosu-stage-{nonce}{executable_suffix}")),
            backup: program_dir.join(format!(".vosu-backup-{nonce}{executable_suffix}")),
            helper_path: cache.join(format!(".vosu-update-helper-{nonce}{executable_suffix}")),
            work,
        };
        platform.checkpoint(UpdateCut::BeforeRecordCreate)?;
        let bytes = serde_json::to_vec(&record)?;
        let create_path = program_dir.join(format!(".vosu-update-create-{nonce}"));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&create_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        match publish_create_record(&create_path, &record_path) {
            Ok(()) => {
                sync_parent(&record_path)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&create_path);
                let existing = match load_record(&record_path) {
                    Ok(record) => record,
                    Err(_) => return quarantine(&record_path, "incomplete install record"),
                };
                if platform.identity_is_alive(&existing.owner)?
                    || existing
                        .helper
                        .as_ref()
                        .is_some_and(|helper| platform.identity_is_alive(helper).unwrap_or(true))
                {
                    return Err(UpdateError::AlreadyRunning);
                }
                return Err(UpdateError::Quarantined(
                    "dead transaction requires recovery before a new update".into(),
                ));
            }
            Err(error) => {
                let _ = fs::remove_file(&create_path);
                return Err(error.into());
            }
        }
        if let Err(error) = fs::create_dir(&record.work).and_then(|()| sync_parent(&record.work)) {
            let _ = fs::remove_file(&record_path);
            let _ = sync_parent(&record_path);
            return Err(error.into());
        }
        if let Err(error) = platform.checkpoint(UpdateCut::AfterRecordCreate) {
            let _ = cleanup_artifacts(&record, &record_path, Some(&record.token));
            return Err(error.into());
        }
        Ok(Self {
            platform,
            record_path,
            record,
            disarmed: false,
        })
    }

    pub fn record(&self) -> &InstallRecord {
        &self.record
    }

    pub fn persist_phase(&mut self, phase: InstallPhase) -> Result<(), UpdateError> {
        self.platform.checkpoint(UpdateCut::BeforePhase(phase))?;
        self.record.phase = phase;
        persist_record(&self.record_path, &self.record)?;
        self.platform.checkpoint(UpdateCut::AfterPhase(phase))?;
        Ok(())
    }

    pub fn set_metadata(&mut self, digest: [u8; 32]) -> Result<(), UpdateError> {
        self.record.archive_digest = Some(hex(&digest));
        self.persist_phase(InstallPhase::Metadata)
    }

    pub fn hand_off(&mut self, helper: ProcessIdentity) -> Result<(), UpdateError> {
        self.record.helper = Some(helper);
        self.platform.checkpoint(UpdateCut::BeforeHandoff)?;
        self.persist_phase(InstallPhase::HandedOff)?;
        self.platform.checkpoint(UpdateCut::AfterHandoff)?;
        Ok(())
    }

    pub(super) fn confirm_hand_off(&mut self) {
        self.disarmed = true;
    }

    pub fn cancel(mut self) -> Result<(), UpdateError> {
        self.platform.checkpoint(UpdateCut::BeforeCleanup)?;
        cleanup_artifacts(&self.record, &self.record_path, Some(&self.record.token))?;
        self.platform.checkpoint(UpdateCut::AfterCleanup)?;
        self.disarmed = true;
        Ok(())
    }
}

impl<P: UpdatePlatform> Drop for TransactionGuard<'_, P> {
    fn drop(&mut self) {
        if !self.disarmed {
            let _ = cleanup_artifacts(&self.record, &self.record_path, Some(&self.record.token));
        }
        let _ = &self.platform;
    }
}

pub struct PreparedUpdate<'a, P: UpdatePlatform> {
    pub guard: TransactionGuard<'a, P>,
    pub release: ValidatedRelease,
}

// Keep the prepared transaction inline so callers retain the established public API.
#[allow(clippy::large_enum_variant)]
pub enum UpdateCheck<'a, P: UpdatePlatform> {
    Latest(Version),
    /// A newer stable release exists, but startup policy disabled installation.
    Available(Version),
    NoFiles,
    SuppressedForced,
    Prepared(PreparedUpdate<'a, P>),
}
impl InstallRecord {
    pub(super) fn record_argument(&self) -> OsString {
        option_path(
            "--update-record=",
            &self
                .target
                .parent()
                .unwrap_or(Path::new("."))
                .join(INSTALL_RECORD_NAME),
        )
    }
}

pub fn load_record(path: &Path) -> Result<InstallRecord, UpdateError> {
    let bytes = fs::read(path)?;
    let record: InstallRecord = serde_json::from_slice(&bytes)?;
    validate_record_paths(path, &record)?;
    Ok(record)
}

pub(super) fn validate_record_paths(
    record_path: &Path,
    record: &InstallRecord,
) -> Result<(), UpdateError> {
    let program = record_path
        .parent()
        .ok_or_else(|| UpdateError::Metadata("record has no program directory".into()))?;
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let cache = program.join(".cache");
    let work = program.join(format!(".vosu-update-work-{}", record.nonce));
    let expected = [
        (&record.work, work.clone()),
        (&record.part, work.join("archive.part")),
        (
            &record.staging,
            program.join(format!(".vosu-stage-{}{}", record.nonce, suffix)),
        ),
        (
            &record.helper_path,
            cache.join(format!(".vosu-update-helper-{}{}", record.nonce, suffix)),
        ),
        (
            &record.backup,
            program.join(format!(".vosu-backup-{}{}", record.nonce, suffix)),
        ),
    ];
    if record_path.file_name().and_then(|name| name.to_str()) != Some(INSTALL_RECORD_NAME)
        || record.target.parent() != Some(program)
        || expected.iter().any(|(actual, wanted)| *actual != wanted)
        || record.nonce.is_empty()
        || !record.nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(UpdateError::Quarantined(
            "install record contains paths outside its transaction root".into(),
        ));
    }
    Ok(())
}

pub(super) fn persist_record(path: &Path, record: &InstallRecord) -> Result<(), UpdateError> {
    validate_record_paths(path, record)?;
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "record has no parent"))?;
    let temp = parent.join(format!("{}-{}.tmp", INSTALL_RECORD_NAME, record.nonce));
    remove_any(&temp)?;
    let bytes = serde_json::to_vec(record)?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    crate::platform::atomic_replace(&temp, path)?;
    sync_parent(path)?;
    Ok(())
}

pub(super) fn recover_create_records<P: UpdatePlatform>(
    program_dir: &Path,
    canonical_record: &Path,
    platform: &P,
) -> Result<(), UpdateError> {
    let entries = fs::read_dir(program_dir)?.collect::<Result<Vec<_>, _>>()?;
    for entry in entries {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(".vosu-update-create-") {
            continue;
        }
        let create_path = entry.path();
        let bytes = match fs::read(&create_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let record: InstallRecord = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(_) => {
                fs::remove_file(&create_path)?;
                sync_parent(&create_path)?;
                continue;
            }
        };
        if validate_record_paths(canonical_record, &record).is_err() {
            let destination =
                program_dir.join(format!(".vosu-update.quarantine-{}", epoch_millis()));
            fs::rename(&create_path, &destination)?;
            sync_parent(&destination)?;
            return Err(UpdateError::Quarantined(format!(
                "unsafe create record retained at {}",
                destination.display()
            )));
        }
        if platform.identity_is_alive(&record.owner)?
            || record
                .helper
                .as_ref()
                .is_some_and(|helper| platform.identity_is_alive(helper).unwrap_or(true))
        {
            return Err(UpdateError::AlreadyRunning);
        }
        if canonical_record.exists() {
            fs::remove_file(&create_path)?;
            sync_parent(&create_path)?;
            continue;
        }
        remove_any(&record.work)?;
        remove_any(&record.staging)?;
        remove_any(&record.helper_path)?;
        remove_any(&record.backup)?;
        fs::remove_file(&create_path)?;
        sync_parent(&create_path)?;
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn publish_create_record(create_path: &Path, record_path: &Path) -> io::Result<()> {
    fs::hard_link(create_path, record_path)?;
    fs::remove_file(create_path)?;
    Ok(())
}

#[cfg(windows)]
pub(super) fn publish_create_record(create_path: &Path, record_path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let create = wide(create_path);
    let record = wide(record_path);
    let result = unsafe { MoveFileExW(create.as_ptr(), record.as_ptr(), MOVEFILE_WRITE_THROUGH) };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Determine recovery without mutating anything. Digest checks make a phase
/// fsync that was lost immediately after replacement distinguishable from an
/// ordinary unswapped handoff.
pub fn classify_recovery(record: &InstallRecord) -> Result<RecoveryAction, UpdateError> {
    let target_digest = digest_file(&record.target).ok();
    let old = Some(record.old_digest.as_str());
    let new = record.new_digest.as_deref();
    let target = target_digest.as_deref();
    let action = match record.phase {
        InstallPhase::Created
        | InstallPhase::Metadata
        | InstallPhase::Downloading
        | InstallPhase::Verified => {
            if target == old {
                RecoveryAction::CleanAndContinue
            } else {
                RecoveryAction::Quarantine
            }
        }
        InstallPhase::Prepared | InstallPhase::HandedOff => {
            if target == old {
                RecoveryAction::CleanAndContinue
            } else if new.is_some() && target == new {
                RecoveryAction::ResumeCandidate
            } else if target.is_none() && backup_matches(record) {
                RecoveryAction::RestoreRollback
            } else {
                RecoveryAction::Quarantine
            }
        }
        InstallPhase::Swapped => {
            if new.is_some() && target == new {
                RecoveryAction::ResumeCandidate
            } else if target == old {
                RecoveryAction::CleanAndContinue
            } else if target.is_none() && backup_matches(record) {
                RecoveryAction::RestoreRollback
            } else {
                RecoveryAction::Quarantine
            }
        }
        InstallPhase::Rollback => {
            if target == old || target.is_none() || (new.is_some() && target == new) {
                RecoveryAction::RestoreRollback
            } else {
                RecoveryAction::Quarantine
            }
        }
        InstallPhase::Ready => {
            if target == old || (new.is_some() && target == new) {
                RecoveryAction::FinishCleanup
            } else if target.is_none() && backup_matches(record) {
                RecoveryAction::RestoreRollback
            } else {
                RecoveryAction::Quarantine
            }
        }
    };
    Ok(action)
}

pub(super) struct RecoveryClaim {
    path: PathBuf,
}

impl RecoveryClaim {
    fn acquire<P: UpdatePlatform>(record_path: &Path, platform: &P) -> Result<Self, UpdateError> {
        cleanup_recovery_temps(
            record_path
                .parent()
                .ok_or_else(|| UpdateError::Metadata("record has no parent".into()))?,
        )?;
        let path = record_path.with_extension("recovery");
        loop {
            let identity = platform.current_identity()?;
            let temp = record_path
                .with_extension(format!("recovery-{}-{}.tmp", identity.pid, identity.start));
            remove_any(&temp)?;
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            file.write_all(&serde_json::to_vec(&identity)?)?;
            file.sync_all()?;
            drop(file);
            match publish_create_record(&temp, &path) {
                Ok(()) => {
                    sync_parent(&path)?;
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let _ = fs::remove_file(&temp);
                    let existing = fs::read(&path)
                        .ok()
                        .and_then(|bytes| serde_json::from_slice::<ProcessIdentity>(&bytes).ok());
                    if existing
                        .as_ref()
                        .is_some_and(|owner| platform.identity_is_alive(owner).unwrap_or(true))
                    {
                        return Err(UpdateError::LiveOwner);
                    }
                    match fs::remove_file(&path) {
                        Ok(()) => sync_parent(&path)?,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) => {
                    let _ = fs::remove_file(&temp);
                    return Err(error.into());
                }
            }
        }
    }
}

impl Drop for RecoveryClaim {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = sync_parent(&self.path);
    }
}

pub fn recover_dead_transaction<P: UpdatePlatform>(
    record_path: &Path,
    platform: &P,
) -> Result<RecoveryAction, UpdateError> {
    let _claim = RecoveryClaim::acquire(record_path, platform)?;
    let mut record = load_record(record_path)?;
    if platform.identity_is_alive(&record.owner)?
        || record
            .helper
            .as_ref()
            .is_some_and(|helper| platform.identity_is_alive(helper).unwrap_or(true))
    {
        return Err(UpdateError::LiveOwner);
    }
    let action = classify_recovery(&record)?;
    if matches!(
        action,
        RecoveryAction::ResumeCandidate | RecoveryAction::RestoreRollback
    ) {
        record.helper = Some(platform.current_identity()?);
        persist_record(record_path, &record)?;
    }
    match action {
        RecoveryAction::CleanAndContinue | RecoveryAction::FinishCleanup => {
            cleanup_artifacts(&record, record_path, Some(&record.token))?;
        }
        RecoveryAction::RestoreRollback => {
            let target_is_old = digest_file(&record.target)
                .ok()
                .is_some_and(|digest| digest == record.old_digest);
            persist_helper_phase(record_path, &mut record, InstallPhase::Rollback, platform)?;
            if !target_is_old {
                if !backup_matches(&record) {
                    return quarantine(
                        record_path,
                        "backup is missing or has an unexpected digest",
                    );
                }
                platform.restore_backup(&record.target, &record.backup)?;
            }
            if digest_file(&record.target)? != record.old_digest {
                return quarantine(record_path, "restored target digest is invalid");
            }
            let ready = launch_and_wait_ready(&record, record_path, platform, true)?;
            commit_ready_with_rollback(record_path, &mut record, ready, platform)?;
        }
        RecoveryAction::ResumeCandidate => {
            if record.phase != InstallPhase::Swapped {
                persist_helper_phase(record_path, &mut record, InstallPhase::Swapped, platform)?;
            }
            supervise_candidate_or_rollback(&mut record, record_path, platform)?;
        }
        RecoveryAction::Quarantine => {
            return quarantine(
                record_path,
                "target was modified outside the update transaction",
            );
        }
    }
    Ok(action)
}

pub(super) fn supervise_candidate_or_rollback<P: UpdatePlatform>(
    record: &mut InstallRecord,
    record_path: &Path,
    platform: &P,
) -> Result<(), UpdateError> {
    let ready = match launch_and_wait_ready(record, record_path, platform, false) {
        Ok(ready) => ready,
        Err(_) => {
            platform.checkpoint(UpdateCut::BeforeRollback)?;
            persist_helper_phase(record_path, record, InstallPhase::Rollback, platform)?;
            if !backup_matches(record) {
                return quarantine(record_path, "rollback backup digest is invalid");
            }
            platform.restore_backup(&record.target, &record.backup)?;
            platform.checkpoint(UpdateCut::AfterRollback)?;
            if digest_file(&record.target)? != record.old_digest {
                return quarantine(record_path, "rollback digest verification failed");
            }
            launch_and_wait_ready(record, record_path, platform, true)?
        }
    };
    commit_ready_with_rollback(record_path, record, ready, platform)?;
    Ok(())
}

pub(super) fn commit_ready_with_rollback<P: UpdatePlatform>(
    record_path: &Path,
    record: &mut InstallRecord,
    ready: PendingReadiness,
    platform: &P,
) -> Result<(), UpdateError> {
    if commit_ready(record_path, record, ready, platform).is_ok() {
        return Ok(());
    }
    platform.checkpoint(UpdateCut::BeforeRollback)?;
    let target_is_old = digest_file(&record.target)
        .ok()
        .is_some_and(|digest| digest == record.old_digest);
    if !target_is_old {
        if !backup_matches(record) {
            return quarantine(record_path, "rollback backup digest is invalid");
        }
        platform.restore_backup(&record.target, &record.backup)?;
    }
    platform.checkpoint(UpdateCut::AfterRollback)?;
    if digest_file(&record.target)? != record.old_digest {
        return quarantine(record_path, "rollback digest verification failed");
    }
    let rollback = launch_and_wait_ready(record, record_path, platform, true)?;
    commit_ready(record_path, record, rollback, platform)
}

pub(super) fn commit_ready<P: UpdatePlatform>(
    record_path: &Path,
    record: &mut InstallRecord,
    ready: PendingReadiness,
    platform: &P,
) -> Result<(), UpdateError> {
    persist_helper_phase(record_path, record, InstallPhase::Ready, platform)?;
    if let Err(error) = ready.acknowledge() {
        persist_helper_phase(record_path, record, InstallPhase::Rollback, platform)?;
        return Err(error);
    }
    Ok(())
}

pub(super) fn persist_helper_phase<P: UpdatePlatform>(
    record_path: &Path,
    record: &mut InstallRecord,
    phase: InstallPhase,
    platform: &P,
) -> Result<(), UpdateError> {
    platform.checkpoint(UpdateCut::BeforePhase(phase))?;
    record.phase = phase;
    persist_record(record_path, record)?;
    platform.checkpoint(UpdateCut::AfterPhase(phase))?;
    Ok(())
}
pub(super) fn quarantine<T>(record_path: &Path, reason: &str) -> Result<T, UpdateError> {
    let destination = record_path.with_extension(format!("quarantine-{}", epoch_millis()));
    fs::rename(record_path, &destination)?;
    sync_parent(&destination)?;
    Err(UpdateError::Quarantined(format!(
        "{reason}; record retained at {}",
        destination.display()
    )))
}
