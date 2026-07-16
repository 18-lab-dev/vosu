use super::*;

/// Download, verify and extract the one expected root executable. The caller
/// may then spawn `helper_path`, wait for its exact identity, and call
/// `TransactionGuard::hand_off` before shutting down the daemon.
pub async fn prepare_update<'a, P: UpdatePlatform>(
    mut guard: TransactionGuard<'a, P>,
    release: ValidatedRelease,
    client: &StreamingClient,
    cancel: &CancellationToken,
) -> Result<PreparedUpdate<'a, P>, UpdateError> {
    guard.set_metadata(release.digest)?;
    guard.persist_phase(InstallPhase::Downloading)?;
    guard.platform.checkpoint(UpdateCut::BeforePartDownload)?;
    client
        .download_atomic(&release.archive_url, USER_AGENT, &guard.record.part, cancel)
        .await?;
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled.into());
    }
    guard.platform.checkpoint(UpdateCut::AfterPartDownload)?;
    guard.platform.checkpoint(UpdateCut::BeforeVerification)?;
    if digest_file_bytes_cancelled(&guard.record.part, cancel)? != release.digest {
        return Err(UpdateError::DigestMismatch);
    }
    validate_archive_shape(&guard.record.part, &guard.record.target, cancel)?;
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled.into());
    }
    guard.platform.checkpoint(UpdateCut::AfterVerification)?;
    guard.persist_phase(InstallPhase::Verified)?;
    guard.platform.checkpoint(UpdateCut::BeforeStaging)?;
    extract_staged_binary(
        &guard.record.part,
        &guard.record.staging,
        &guard.record.target,
        cancel,
    )?;
    guard.record.new_digest = Some(hex(&digest_file_bytes_cancelled(
        &guard.record.staging,
        cancel,
    )?));
    persist_record(&guard.record_path, &guard.record)?;
    guard.platform.checkpoint(UpdateCut::AfterStaging)?;
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled.into());
    }
    guard.persist_phase(InstallPhase::Prepared)?;
    guard.platform.checkpoint(UpdateCut::BeforeHelperCopy)?;
    remove_any(&guard.record.helper_path)?;
    let mut source = File::open(&guard.record.target)?;
    let mut helper = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&guard.record.helper_path)?;
    copy_cancelled(&mut source, &mut helper, cancel)?;
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled.into());
    }
    set_executable(&guard.record.helper_path)?;
    helper.sync_all()?;
    sync_parent(&guard.record.helper_path)?;
    guard.platform.checkpoint(UpdateCut::AfterHelperCopy)?;
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled.into());
    }
    Ok(PreparedUpdate { guard, release })
}

impl<P: UpdatePlatform> PreparedUpdate<'_, P> {
    /// Start the helper, persist its exact process identity, and wait for the
    /// helper to durably acknowledge that identity before ownership is
    /// transferred and the daemon is allowed to shut down.
    pub fn spawn_helper(mut self) -> Result<ProcessIdentity, UpdateError> {
        let record = self.guard.record.clone();
        remove_any(&helper_ack_path(&record))?;
        let args = vec![
            OsString::from("--update-helper"),
            record.record_argument(),
            OsString::from(format!("--update-token={}", record.token)),
        ];
        let cwd = PathBuf::from(record.working_directory.restore()?);
        let mut child = self
            .guard
            .platform
            .spawn(&record.helper_path, &args, &cwd)?;
        let identity = match self.guard.platform.identity_for_pid(child.id()) {
            Ok(identity) => identity,
            Err(error) => {
                kill_and_reap(&mut child);
                return Err(error.into());
            }
        };
        if let Err(error) = self.guard.hand_off(identity.clone()) {
            kill_and_reap(&mut child);
            return Err(error);
        }
        if let Err(error) = wait_for_helper_ack(&record, &identity, &mut child) {
            kill_and_reap(&mut child);
            return Err(error);
        }
        self.guard.confirm_hand_off();
        drop(child);
        Ok(identity)
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct HelperHandoffAck {
    token: String,
    helper: ProcessIdentity,
}

pub(super) fn helper_ack_path(record: &InstallRecord) -> PathBuf {
    record.work.join("helper.ack")
}

pub(super) fn write_helper_ack(
    record: &InstallRecord,
    helper: &ProcessIdentity,
) -> Result<(), UpdateError> {
    let path = helper_ack_path(record);
    let temp = record
        .work
        .join(format!("helper.ack-{}-{}.tmp", helper.pid, helper.start));
    remove_any(&temp)?;
    let bytes = serde_json::to_vec(&HelperHandoffAck {
        token: record.token.clone(),
        helper: helper.clone(),
    })?;
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temp, &path)?;
    sync_parent(&path)?;
    Ok(())
}

pub(super) fn wait_for_helper_ack(
    record: &InstallRecord,
    helper: &ProcessIdentity,
    child: &mut Child,
) -> Result<(), UpdateError> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let path = helper_ack_path(record);
    loop {
        match fs::read(&path) {
            Ok(bytes) => {
                let ack: HelperHandoffAck =
                    serde_json::from_slice(&bytes).map_err(|_| UpdateError::HelperHandoff)?;
                let record_path = record
                    .target
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(INSTALL_RECORD_NAME);
                let durable = load_record(&record_path)?;
                if constant_time_eq(ack.token.as_bytes(), record.token.as_bytes())
                    && ack.helper == *helper
                    && durable.phase == InstallPhase::HandedOff
                    && durable.helper.as_ref() == Some(helper)
                    && constant_time_eq(durable.token.as_bytes(), record.token.as_bytes())
                {
                    return Ok(());
                }
                return Err(UpdateError::HelperHandoff);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if child.try_wait()?.is_some() || Instant::now() >= deadline {
            return Err(UpdateError::HelperHandoff);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn validate_archive_shape(
    archive: &Path,
    target: &Path,
    cancel: &CancellationToken,
) -> Result<(), UpdateError> {
    let mut file = File::open(archive)?;
    preflight_zip_entries(&mut file, MAX_ARCHIVE_ENTRIES, cancel)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let expected = target
        .file_name()
        .ok_or_else(|| UpdateError::Metadata("target has no file name".into()))?;
    let expected_raw = os_name_bytes(expected)?;
    let mut executable_found = false;
    let mut license_found = false;
    let mut expanded = 0u64;
    for index in 0..zip.len() {
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled.into());
        }
        let entry = zip.by_index(index)?;
        let raw = entry.name_raw();
        if raw.is_empty() || raw.contains(&b'/') || raw.contains(&b'\\') {
            return Err(UpdateError::Metadata(
                "release archive must contain raw root files only".into(),
            ));
        }
        let is_executable = raw == expected_raw.as_slice();
        let is_license = raw == b"LICENSE";
        if !is_executable && !is_license {
            return Err(UpdateError::Metadata(format!(
                "unexpected archive member {}",
                entry.name()
            )));
        }
        if entry.is_dir()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(UpdateError::Metadata(
                "archive directory or symlink is forbidden".into(),
            ));
        }
        if (is_executable && executable_found) || (is_license && license_found) {
            return Err(UpdateError::Metadata("duplicate archive member".into()));
        }
        executable_found |= is_executable;
        license_found |= is_license;
        expanded = expanded
            .checked_add(entry.size())
            .filter(|total| *total <= MAX_EXPANDED_BYTES)
            .ok_or_else(|| UpdateError::Metadata("archive expands beyond limit".into()))?;
    }
    if zip.len() != 2 || !executable_found || !license_found {
        return Err(UpdateError::Metadata(
            "release archive must contain exactly the executable and LICENSE".into(),
        ));
    }
    Ok(())
}

pub(super) fn extract_staged_binary(
    archive: &Path,
    staging: &Path,
    target: &Path,
    cancel: &CancellationToken,
) -> Result<(), UpdateError> {
    extract_staged_binary_with_limit(archive, staging, target, cancel, MAX_EXPANDED_BYTES)
}

pub(super) fn extract_staged_binary_with_limit(
    archive: &Path,
    staging: &Path,
    target: &Path,
    cancel: &CancellationToken,
    max_expanded_bytes: u64,
) -> Result<(), UpdateError> {
    remove_any(staging)?;
    let mut file = File::open(archive)?;
    preflight_zip_entries(&mut file, MAX_ARCHIVE_ENTRIES, cancel)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let expected = target
        .file_name()
        .ok_or_else(|| UpdateError::Metadata("target has no file name".into()))?;
    let expected_raw = os_name_bytes(expected)?;
    let mut expanded = 0u64;
    let mut executable_found = false;
    let mut license_found = false;
    for index in 0..zip.len() {
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled.into());
        }
        let mut entry = zip.by_index(index)?;
        let raw = entry.name_raw();
        if raw.is_empty() || raw.contains(&b'/') || raw.contains(&b'\\') {
            return Err(UpdateError::Metadata(
                "release archive must contain raw root files only".into(),
            ));
        }
        let is_executable = raw == expected_raw.as_slice();
        let is_license = raw == b"LICENSE";
        if !is_executable && !is_license {
            return Err(UpdateError::Metadata(format!(
                "unexpected archive member {}",
                entry.name()
            )));
        }
        if entry.is_dir()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(UpdateError::Metadata(
                "archive directories and symlinks are forbidden".into(),
            ));
        }
        if (is_executable && executable_found) || (is_license && license_found) {
            return Err(UpdateError::Metadata(
                "duplicate release archive member".into(),
            ));
        }
        let declared = entry.size();
        let remaining = max_expanded_bytes
            .checked_sub(expanded)
            .ok_or_else(|| UpdateError::Metadata("archive expands beyond limit".into()))?;
        let mut limited = (&mut entry).take(remaining.saturating_add(1));
        let copied = if is_executable {
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(staging)?;
            let copied = copy_cancelled(&mut limited, &mut output, cancel)?;
            set_executable(staging)?;
            output.sync_all()?;
            executable_found = true;
            copied
        } else {
            license_found = true;
            copy_cancelled(&mut limited, &mut io::sink(), cancel)?
        };
        if copied > remaining {
            return Err(UpdateError::Metadata("archive expands beyond limit".into()));
        }
        if copied != declared {
            return Err(UpdateError::Metadata(
                "archive member size does not match decoded bytes".into(),
            ));
        }
        expanded += copied;
    }
    if !executable_found || !license_found || zip.len() != 2 {
        return Err(UpdateError::Metadata(
            "release archive must contain exactly the executable and LICENSE".into(),
        ));
    }
    sync_parent(staging)?;
    Ok(())
}

pub(super) fn copy_cancelled(
    reader: &mut impl Read,
    writer: &mut impl Write,
    cancel: &CancellationToken,
) -> Result<u64, UpdateError> {
    let mut copied = 0u64;
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled.into());
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(copied);
        }
        writer.write_all(&buffer[..count])?;
        copied += count as u64;
    }
}

pub(super) fn os_name_bytes(name: &std::ffi::OsStr) -> Result<Vec<u8>, UpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(name.as_bytes().to_vec())
    }
    #[cfg(windows)]
    {
        name.to_str()
            .map(|value| value.as_bytes().to_vec())
            .ok_or_else(|| UpdateError::Metadata("Windows executable name is not Unicode".into()))
    }
}

pub(super) fn preflight_zip_entries(
    file: &mut File,
    maximum: usize,
    cancel: &CancellationToken,
) -> Result<(), UpdateError> {
    let length = file.metadata()?.len();
    if length < 22 {
        return Err(UpdateError::Metadata("missing ZIP end record".into()));
    }
    const CHUNK: usize = 1024 * 1024;
    let mut offset = 0u64;
    let mut overlap = Vec::new();
    let mut chunk = vec![0u8; CHUNK];
    let mut found = false;
    let mut candidates = 0usize;
    while offset < length {
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled.into());
        }
        file.seek(SeekFrom::Start(offset))?;
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        let overlap_len = overlap.len();
        let mut scan = overlap;
        scan.extend_from_slice(&chunk[..count]);
        let base = offset.saturating_sub(overlap_len as u64);
        for index in 0..scan.len().saturating_sub(3) {
            if &scan[index..index + 4] != b"PK\x05\x06" {
                continue;
            }
            if let Some(entries) = zip_candidate_entries(file, base + index as u64, length)? {
                candidates += 1;
                if candidates > MAX_ARCHIVE_ENTRIES {
                    return Err(UpdateError::Metadata("too many ZIP end records".into()));
                }
                found = true;
                if entries > maximum as u64 {
                    return Err(UpdateError::Metadata("archive has too many entries".into()));
                }
            }
        }
        overlap = scan[scan.len().saturating_sub(3)..].to_vec();
        offset += count as u64;
    }
    if !found {
        return Err(UpdateError::Metadata("missing ZIP end record".into()));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}

pub(super) fn zip_candidate_entries(
    file: &mut File,
    position: u64,
    length: u64,
) -> Result<Option<u64>, UpdateError> {
    if position.checked_add(22).is_none_or(|end| end > length) {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(position))?;
    let mut end = [0u8; 22];
    file.read_exact(&mut end)?;
    if &end[..4] != b"PK\x05\x06" {
        return Ok(None);
    }
    let comment = u16::from_le_bytes([end[20], end[21]]) as u64;
    if position.checked_add(22 + comment) != Some(length) {
        return Ok(None);
    }
    if position >= 20 {
        file.seek(SeekFrom::Start(position - 20))?;
        let mut locator = [0u8; 20];
        file.read_exact(&mut locator)?;
        if &locator[..4] == b"PK\x06\x07" {
            let offset = u64::from_le_bytes(locator[8..16].try_into().unwrap());
            if offset.checked_add(56).is_none_or(|end| end > length) {
                return Err(UpdateError::Metadata("invalid ZIP64 end record".into()));
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut record = [0u8; 56];
            file.read_exact(&mut record)?;
            if &record[..4] != b"PK\x06\x06" {
                return Err(UpdateError::Metadata("invalid ZIP64 end record".into()));
            }
            return Ok(Some(u64::from_le_bytes(record[32..40].try_into().unwrap())));
        }
    }
    Ok(Some(u16::from_le_bytes([end[10], end[11]]) as u64))
}
