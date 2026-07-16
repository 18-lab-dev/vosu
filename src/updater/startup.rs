use super::*;

pub fn current_version() -> Version {
    Version::parse(CURRENT_VERSION).expect("Cargo package version is valid semver")
}

pub fn parse_startup_update_args<I>(arguments: I) -> Result<StartupIntent, UpdateError>
where
    I: IntoIterator<Item = OsString>,
{
    let mut helper = false;
    let mut transaction_token = None;
    let mut candidate_token = None;
    let mut rollback_token = None;
    let mut ready_address = None;
    let mut record_path = None;
    let mut saw_internal = false;

    for argument in arguments {
        let argument = argument.as_os_str();
        if argument == OsStr::new("--update-helper") {
            if helper {
                return internal_argument_error("duplicate --update-helper");
            }
            helper = true;
            saw_internal = true;
            continue;
        }
        if let Some(value) = strip_os_prefix(argument, "--update-token=") {
            set_once(
                &mut transaction_token,
                parse_hex_token(value, "--update-token")?,
                "--update-token",
            )?;
            saw_internal = true;
            continue;
        }
        if let Some(value) = strip_os_prefix(argument, "--update-candidate=") {
            set_once(
                &mut candidate_token,
                parse_hex_token(value, "--update-candidate")?,
                "--update-candidate",
            )?;
            saw_internal = true;
            continue;
        }
        if let Some(value) = strip_os_prefix(argument, "--update-rollback=") {
            set_once(
                &mut rollback_token,
                parse_hex_token(value, "--update-rollback")?,
                "--update-rollback",
            )?;
            saw_internal = true;
            continue;
        }
        if let Some(value) = strip_os_prefix(argument, "--update-ready=") {
            let text = value.to_str().ok_or_else(|| {
                UpdateError::InternalArguments("--update-ready is not Unicode".into())
            })?;
            let address = text.parse::<SocketAddr>().map_err(|_| {
                UpdateError::InternalArguments("invalid --update-ready address".into())
            })?;
            set_once(&mut ready_address, address, "--update-ready")?;
            saw_internal = true;
            continue;
        }
        if let Some(value) = strip_os_prefix(argument, "--update-record=") {
            if value.is_empty() {
                return internal_argument_error("empty --update-record path");
            }
            set_once(&mut record_path, PathBuf::from(value), "--update-record")?;
            saw_internal = true;
            continue;
        }
        if [
            "--update-helper",
            "--update-token",
            "--update-candidate",
            "--update-rollback",
            "--update-ready",
            "--update-record",
        ]
        .iter()
        .any(|prefix| os_starts_with(argument, prefix))
        {
            return internal_argument_error("malformed internal updater option");
        }
    }

    if !saw_internal {
        return Ok(StartupIntent::Ordinary);
    }
    let record_path = record_path
        .ok_or_else(|| UpdateError::InternalArguments("missing --update-record".into()))?;
    let transaction_token = transaction_token
        .ok_or_else(|| UpdateError::InternalArguments("missing --update-token".into()))?;
    match (helper, candidate_token, rollback_token, ready_address) {
        (true, None, None, None) => Ok(StartupIntent::Helper {
            record_path,
            transaction_token,
        }),
        (false, Some(readiness_token), None, Some(address)) => {
            Ok(StartupIntent::Candidate(StartupReadiness {
                record_path,
                transaction_token,
                readiness_token,
                address,
            }))
        }
        (false, None, Some(readiness_token), Some(address)) => {
            Ok(StartupIntent::Rollback(StartupReadiness {
                record_path,
                transaction_token,
                readiness_token,
                address,
            }))
        }
        _ => internal_argument_error("partial or conflicting internal updater role"),
    }
}

/// Dispatch only token-bound internal updater roles. Ordinary startup is
/// deliberately side-effect free so archive/OneDrive guards can run before an
/// abandoned transaction mutates the installed executable.
pub fn dispatch_startup_role<I, P>(
    arguments: I,
    platform: &P,
) -> Result<StartupDispatch, UpdateError>
where
    I: IntoIterator<Item = OsString>,
    P: UpdatePlatform,
{
    match parse_startup_update_args(arguments)? {
        StartupIntent::Ordinary => Ok(StartupDispatch::Ordinary { recovery: None }),
        StartupIntent::Helper {
            record_path,
            transaction_token,
        } => {
            run_helper(&record_path, &transaction_token, platform)?;
            Ok(StartupDispatch::HelperCompleted)
        }
        StartupIntent::Candidate(context) => Ok(StartupDispatch::Candidate(context)),
        StartupIntent::Rollback(context) => Ok(StartupDispatch::Rollback(context)),
    }
}

/// Recover an abandoned ordinary-startup transaction only after startup path
/// guards have accepted the executable location.
pub fn recover_startup_update<P: UpdatePlatform>(
    program_dir: &Path,
    platform: &P,
) -> Result<Option<RecoveryAction>, UpdateError> {
    let record = program_dir.join(INSTALL_RECORD_NAME);
    if record.exists() {
        recover_dead_transaction(&record, platform).map(Some)
    } else {
        Ok(None)
    }
}

/// Compatibility convenience for callers that have already applied startup
/// path guards. New startup composition should use [`dispatch_startup_role`]
/// before the guards and [`recover_startup_update`] after they pass.
pub fn dispatch_startup_update<I, P>(
    arguments: I,
    program_dir: &Path,
    platform: &P,
) -> Result<StartupDispatch, UpdateError>
where
    I: IntoIterator<Item = OsString>,
    P: UpdatePlatform,
{
    match dispatch_startup_role(arguments, platform)? {
        StartupDispatch::Ordinary { .. } => Ok(StartupDispatch::Ordinary {
            recovery: recover_startup_update(program_dir, platform)?,
        }),
        dispatch => Ok(dispatch),
    }
}

pub(super) fn set_once<T>(slot: &mut Option<T>, value: T, name: &str) -> Result<(), UpdateError> {
    if slot.replace(value).is_some() {
        internal_argument_error(&format!("duplicate {name}"))
    } else {
        Ok(())
    }
}

pub(super) fn parse_hex_token(value: OsString, name: &str) -> Result<String, UpdateError> {
    let value = value
        .into_string()
        .map_err(|_| UpdateError::InternalArguments(format!("{name} is not Unicode")))?;
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return internal_argument_error(&format!("invalid {name} token"));
    }
    Ok(value)
}

pub(super) fn internal_argument_error<T>(message: &str) -> Result<T, UpdateError> {
    Err(UpdateError::InternalArguments(message.into()))
}

#[cfg(unix)]
pub(super) fn strip_os_prefix(value: &OsStr, prefix: &str) -> Option<OsString> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    value
        .as_bytes()
        .strip_prefix(prefix.as_bytes())
        .map(|tail| OsString::from_vec(tail.to_vec()))
}

#[cfg(windows)]
pub(super) fn strip_os_prefix(value: &OsStr, prefix: &str) -> Option<OsString> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    let value = value.encode_wide().collect::<Vec<_>>();
    let prefix = prefix.encode_utf16().collect::<Vec<_>>();
    value
        .strip_prefix(prefix.as_slice())
        .map(OsString::from_wide)
}

pub(super) fn os_starts_with(value: &OsStr, prefix: &str) -> bool {
    strip_os_prefix(value, prefix).is_some()
}

pub(super) fn canonical_original_args<I>(arguments: I) -> Vec<EncodedOsString>
where
    I: IntoIterator<Item = OsString>,
{
    const VALUE_OPTIONS: [&str; 5] = [
        "--update-token",
        "--update-candidate",
        "--update-rollback",
        "--update-ready",
        "--update-record",
    ];
    let mut output = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let argument_ref = argument.as_os_str();
        if argument_ref == OsStr::new("--update-helper") {
            continue;
        }
        if VALUE_OPTIONS
            .iter()
            .any(|option| argument_ref == OsStr::new(option))
        {
            // The following OsString is an opaque internal value, including
            // a potentially non-Unicode record path. It belongs to the
            // internal option and must not be replayed as a user argument.
            let _ = arguments.next();
            continue;
        }
        if VALUE_OPTIONS
            .iter()
            .any(|option| strip_os_prefix(argument_ref, &format!("{option}=")).is_some())
        {
            continue;
        }
        output.push(EncodedOsString::capture(argument));
    }
    output
}
