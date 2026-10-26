use super::*;

/// Helper-side swap and candidate supervision.
///
/// After authorizing a live owner's record, poll every 5 ms until its durable
/// handoff arrives. Missing records remain retryable while that owner lives;
/// truncated JSON and Windows sharing/delete-pending errors get a bounded
/// two-second retry window, reset only by a successfully loaded record.
pub fn run_helper<P: UpdatePlatform>(
    record_path: &Path,
    token: &str,
    platform: &P,
) -> Result<(), UpdateError> {
    let current = platform.current_identity()?;
    let mut last_candidate: Option<InstallRecord> = None;
    let mut record_retry_deadline = None;
    let mut record = loop {
        let candidate = match load_record(record_path) {
            Ok(candidate) => {
                record_retry_deadline = None;
                candidate
            }
            Err(UpdateError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                let Some(candidate) = last_candidate.as_ref() else {
                    return Err(UpdateError::Io(error));
                };
                if !platform.identity_is_alive(&candidate.owner)? {
                    return Err(UpdateError::HelperHandoff);
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(error) if retryable_handoff_record_error(&error) => {
                let Some(candidate) = last_candidate.as_ref() else {
                    return Err(error);
                };
                let deadline = *record_retry_deadline
                    .get_or_insert_with(|| Instant::now() + Duration::from_secs(2));
                if Instant::now() >= deadline {
                    return Err(error);
                }
                if !platform.identity_is_alive(&candidate.owner)? {
                    return Err(UpdateError::HelperHandoff);
                }
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(error) => return Err(error),
        };
        if !constant_time_eq(candidate.token.as_bytes(), token.as_bytes()) {
            return Err(UpdateError::ReadinessAuthorization);
        }
        match &candidate.helper {
            Some(helper) if *helper == current && candidate.phase == InstallPhase::HandedOff => {
                write_helper_ack(&candidate, &current)?;
                break candidate;
            }
            Some(_) => return Err(UpdateError::ReadinessAuthorization),
            None if platform.identity_is_alive(&candidate.owner)? => {
                last_candidate = Some(candidate);
                std::thread::sleep(Duration::from_millis(5));
            }
            None => return Err(UpdateError::HelperHandoff),
        }
    };
    if !wait_for_exit(&record.owner, Duration::from_secs(15), platform)? {
        platform.terminate_and_wait(&record.owner, Duration::from_secs(5))?;
    }
    if platform.identity_is_alive(&record.owner)? {
        return Err(UpdateError::LiveOwner);
    }
    if digest_file(&record.target)? != record.old_digest
        || record.new_digest.as_deref() != digest_file(&record.staging).ok().as_deref()
    {
        return quarantine(
            record_path,
            "pre-swap executable digest verification failed",
        );
    }
    platform.checkpoint(UpdateCut::BeforeSwap)?;
    platform.atomic_swap(&record.target, &record.staging, &record.backup)?;
    platform.checkpoint(UpdateCut::AfterSwap)?;
    if record.new_digest.as_deref() != digest_file(&record.target).ok().as_deref()
        || !backup_matches(&record)
    {
        return quarantine(
            record_path,
            "post-swap executable digest verification failed",
        );
    }
    persist_helper_phase(record_path, &mut record, InstallPhase::Swapped, platform)?;
    supervise_candidate_or_rollback(&mut record, record_path, platform)?;
    // The ready child cleans up after this helper exits.
    Ok(())
}

fn retryable_handoff_record_error(error: &UpdateError) -> bool {
    match error {
        UpdateError::Json(error) => error.is_eof(),
        // ERROR_ACCESS_DENIED includes a record marked for deletion while a
        // scanner still owns a handle. Sharing/lock violations are not mapped
        // to WouldBlock by std, so compare the native codes explicitly.
        #[cfg(windows)]
        UpdateError::Io(error) => matches!(error.raw_os_error(), Some(5 | 32 | 33 | 303)),
        _ => false,
    }
}
/// Ready-child cleanup. It waits for the exact helper identity, then retries
/// token-authorized removal for ten seconds to accommodate Windows scanners
/// and delayed executable unlocks.
pub fn finish_ready_cleanup<P: UpdatePlatform>(
    record_path: &Path,
    token: &str,
    platform: &P,
) -> Result<(), UpdateError> {
    let record = load_record(record_path)?;
    if record.phase != InstallPhase::Ready
        || !constant_time_eq(record.token.as_bytes(), token.as_bytes())
    {
        return Err(UpdateError::ReadinessAuthorization);
    }
    if let Some(helper) = &record.helper {
        wait_for_exit(helper, Duration::from_secs(10), platform)?;
        if platform.identity_is_alive(helper)? {
            return Err(UpdateError::LiveOwner);
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        platform.checkpoint(UpdateCut::BeforeCleanup)?;
        match cleanup_artifacts(&record, record_path, Some(token)) {
            Ok(()) => {
                platform.checkpoint(UpdateCut::AfterCleanup)?;
                return Ok(());
            }
            Err(UpdateError::Io(error))
                if Instant::now() < deadline
                    && matches!(
                        error.kind(),
                        io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
                    ) =>
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) struct PendingReadiness {
    stream: Option<std::net::TcpStream>,
    child: Option<Child>,
}

impl PendingReadiness {
    pub(super) fn acknowledge(mut self) -> Result<(), UpdateError> {
        if let Some(stream) = self.stream.as_mut()
            && let Err(error) = stream.write_all(&[1]).and_then(|()| stream.flush())
        {
            // The child has not received a committed-ready acknowledgement.
            // Stop and reap this exact handle before replacement/rollback can
            // touch the executable it is currently running.
            if let Some(mut child) = self.child.take() {
                kill_and_reap(&mut child);
            }
            return Err(error.into());
        }
        // Acknowledged candidates intentionally outlive this supervisor.
        drop(self.child.take());
        Ok(())
    }
}

impl Drop for PendingReadiness {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            kill_and_reap(&mut child);
        }
    }
}

pub(super) fn launch_and_wait_ready<P: UpdatePlatform>(
    record: &InstallRecord,
    record_path: &Path,
    platform: &P,
    rollback: bool,
) -> Result<PendingReadiness, UpdateError> {
    if let Some(result) = platform.readiness_override(rollback) {
        result?;
        return Ok(PendingReadiness {
            stream: None,
            child: None,
        });
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let readiness = hex(&platform.random_128()?);
    let role = if rollback { "rollback" } else { "candidate" };
    let mut args = record
        .original_args
        .iter()
        .map(EncodedOsString::restore)
        .collect::<io::Result<Vec<_>>>()?;
    args.extend([
        OsString::from(format!("--update-{role}")),
        OsString::from(format!("--update-ready={address}")),
        option_path("--update-record=", record_path),
        OsString::from(format!("--update-token={}", record.token)),
    ]);
    let cwd = PathBuf::from(record.working_directory.restore()?);
    let mut child = platform.spawn(&record.target, &args, &cwd)?;
    // Only the exact spawned child receives this one-use bearer secret. Never
    // place it in argv, environment, or a generally readable transaction file.
    let delivered = child
        .stdin
        .take()
        .ok_or(UpdateError::ReadinessAuthorization)
        .and_then(|mut input| {
            input
                .write_all(readiness.as_bytes())
                .map_err(UpdateError::from)
        });
    if let Err(error) = delivered {
        kill_and_reap(&mut child);
        return Err(error);
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return Err(UpdateError::CandidateFailed),
            Ok(None) => {}
            Err(error) => {
                kill_and_reap(&mut child);
                return Err(error.into());
            }
        }
        match listener.accept() {
            Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                let remaining = deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(2));
                let mut token = vec![0u8; readiness.len()];
                if let Err(error) = stream.set_read_timeout(Some(remaining)) {
                    kill_and_reap(&mut child);
                    return Err(error.into());
                }
                if stream.read_exact(&mut token).is_err() {
                    continue;
                }
                if let Err(error) = stream.set_read_timeout(Some(Duration::from_millis(10))) {
                    kill_and_reap(&mut child);
                    return Err(error.into());
                }
                let mut trailing = [0u8; 1];
                let has_trailing = matches!(stream.read(&mut trailing), Ok(count) if count != 0);
                if !has_trailing && constant_time_eq(&token, readiness.as_bytes()) {
                    return Ok(PendingReadiness {
                        stream: Some(stream),
                        child: Some(child),
                    });
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(error) => {
                kill_and_reap(&mut child);
                return Err(error.into());
            }
        }
    }
    kill_and_reap(&mut child);
    Err(UpdateError::CandidateFailed)
}

pub(super) fn cleanup_recovery_temps(program_dir: &Path) -> Result<(), UpdateError> {
    for entry in fs::read_dir(program_dir)?.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".vosu-update.recovery-") && name.ends_with(".tmp") {
            match fs::remove_file(entry.path()) {
                Ok(()) => sync_parent(&entry.path())?,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound
                            | io::ErrorKind::PermissionDenied
                            | io::ErrorKind::WouldBlock
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

/// Candidate-side readiness signal. The one-use secret arrives only through
/// the inherited stdin pipe, not through cross-account-visible command arguments.
pub fn signal_ready(
    record_path: &Path,
    transaction_token: &str,
    address: SocketAddr,
) -> Result<(), UpdateError> {
    let record = load_record(record_path)?;
    if !constant_time_eq(record.token.as_bytes(), transaction_token.as_bytes()) {
        return Err(UpdateError::ReadinessAuthorization);
    }
    let mut token = [0u8; 32];
    std::io::stdin().lock().read_exact(&mut token)?;
    signal_ready_with_token(record_path, transaction_token, &token, address)
}

pub(super) fn signal_ready_with_token(
    record_path: &Path,
    transaction_token: &str,
    readiness_token: &[u8],
    address: SocketAddr,
) -> Result<(), UpdateError> {
    let record = load_record(record_path)?;
    if !constant_time_eq(record.token.as_bytes(), transaction_token.as_bytes()) {
        return Err(UpdateError::ReadinessAuthorization);
    }
    if !address.ip().is_loopback() {
        return Err(UpdateError::ReadinessAuthorization);
    }
    let mut stream = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.write_all(readiness_token)?;
    stream.flush()?;
    let mut acknowledgement = [0u8; 1];
    stream.read_exact(&mut acknowledgement)?;
    if acknowledgement != [1] {
        return Err(UpdateError::ReadinessAuthorization);
    }
    Ok(())
}

pub(super) fn cleanup_artifacts(
    record: &InstallRecord,
    record_path: &Path,
    token: Option<&str>,
) -> Result<(), UpdateError> {
    validate_record_paths(record_path, record)?;
    if let Some(expected) = token {
        let current = load_record(record_path)?;
        if !constant_time_eq(expected.as_bytes(), current.token.as_bytes())
            || !constant_time_eq(record.token.as_bytes(), current.token.as_bytes())
        {
            return Err(UpdateError::ReadinessAuthorization);
        }
    }
    let record_temp = record_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(format!("{}-{}.tmp", INSTALL_RECORD_NAME, record.nonce));
    for artifact in [
        &record.work,
        &record.staging,
        &record.helper_path,
        &record.backup,
        &record_temp,
    ] {
        remove_any(artifact)?;
        sync_parent(artifact)?;
    }
    cleanup_recovery_temps(
        record_path
            .parent()
            .ok_or_else(|| UpdateError::Metadata("record has no parent".into()))?,
    )?;
    if record_path.exists() {
        fs::remove_file(record_path)?;
    }
    sync_parent(record_path)?;
    Ok(())
}
