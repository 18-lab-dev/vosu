use super::*;

pub trait UpdatePlatform: Send + Sync {
    fn current_identity(&self) -> io::Result<ProcessIdentity>;
    fn identity_for_pid(&self, pid: u32) -> io::Result<ProcessIdentity>;
    fn identity_is_alive(&self, identity: &ProcessIdentity) -> io::Result<bool>;
    fn random_128(&self) -> io::Result<[u8; 16]>;
    fn atomic_swap(&self, target: &Path, staged: &Path, backup: &Path) -> io::Result<()>;
    fn restore_backup(&self, target: &Path, backup: &Path) -> io::Result<()>;
    fn spawn(&self, executable: &Path, args: &[OsString], cwd: &Path) -> io::Result<Child>;
    fn terminate_and_wait(&self, identity: &ProcessIdentity, timeout: Duration) -> io::Result<()>;
    /// Deterministic crash-cut hook used by the native kill/restart suite.
    fn checkpoint(&self, _: UpdateCut) -> io::Result<()> {
        Ok(())
    }
    /// Deterministic native-recovery seam. Production returns `None` and uses
    /// the private readiness pipe; tests may complete or fail a launch without
    /// spawning a second copy of the test runner.
    fn readiness_override(&self, _: bool) -> Option<io::Result<()>> {
        None
    }
}

#[derive(Debug, Default)]
pub struct NativePlatform;

impl UpdatePlatform for NativePlatform {
    fn current_identity(&self) -> io::Result<ProcessIdentity> {
        let pid = std::process::id();
        Ok(ProcessIdentity {
            pid,
            start: process_start_identity(pid)?,
        })
    }
    fn identity_for_pid(&self, pid: u32) -> io::Result<ProcessIdentity> {
        Ok(ProcessIdentity {
            pid,
            start: process_start_identity(pid)?,
        })
    }

    fn identity_is_alive(&self, identity: &ProcessIdentity) -> io::Result<bool> {
        match process_start_identity(identity.pid) {
            Ok(start) => Ok(start == identity.start),
            Err(error) if process_missing_error(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn random_128(&self) -> io::Result<[u8; 16]> {
        secure_random_128()
    }

    fn atomic_swap(&self, target: &Path, staged: &Path, backup: &Path) -> io::Result<()> {
        atomic_swap(target, staged, backup)
    }

    fn restore_backup(&self, target: &Path, backup: &Path) -> io::Result<()> {
        restore_backup(target, backup)
    }

    fn spawn(&self, executable: &Path, args: &[OsString], cwd: &Path) -> io::Result<Child> {
        Command::new(executable)
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }

    fn terminate_and_wait(&self, identity: &ProcessIdentity, timeout: Duration) -> io::Result<()> {
        match terminate_process(identity, timeout) {
            Err(error) if process_missing_error(&error) => Ok(()),
            result => result,
        }
    }
}
#[cfg(target_os = "linux")]
pub(super) fn process_missing_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

#[cfg(windows)]
pub(super) fn process_missing_error(error: &io::Error) -> bool {
    // OpenProcess reports ERROR_INVALID_PARAMETER for a PID with no process.
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(87)
}

pub(super) fn digest_file(path: &Path) -> io::Result<String> {
    Ok(hex(&digest_file_bytes(path)?))
}
pub(super) fn option_path(prefix: &str, path: &Path) -> OsString {
    let mut value = OsString::from(prefix);
    value.push(path.as_os_str());
    value
}

pub(super) fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub(super) fn digest_file_bytes(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().into())
}

pub(super) fn digest_file_bytes_cancelled(
    path: &Path,
    cancel: &CancellationToken,
) -> Result<[u8; 32], UpdateError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled.into());
        }
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher.finalize().into())
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub(super) fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
pub(super) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
pub(super) fn backup_matches(record: &InstallRecord) -> bool {
    digest_file(&record.backup).ok().as_deref() == Some(record.old_digest.as_str())
}
pub(super) fn ensure_real_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path is not a real directory",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
            sync_parent(path)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn remove_any(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
pub(super) fn wait_for_exit<P: UpdatePlatform>(
    identity: &ProcessIdentity,
    timeout: Duration,
    platform: &P,
) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !platform.identity_is_alive(identity)? {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(!platform.identity_is_alive(identity)?)
}
pub(super) fn sync_parent(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()?;
    }
    #[cfg(windows)]
    {
        let _ = parent;
    }
    Ok(())
}
#[cfg(unix)]
pub(super) fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}
#[cfg(windows)]
pub(super) fn set_executable(_: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn secure_random_128() -> io::Result<[u8; 16]> {
    let mut value = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut value)?;
    Ok(value)
}
#[cfg(windows)]
pub(super) fn secure_random_128() -> io::Result<[u8; 16]> {
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    type RtlGenRandom = unsafe extern "system" fn(*mut core::ffi::c_void, u32) -> u8;
    let library_name: Vec<u16> = "advapi32.dll\0".encode_utf16().collect();
    let library = unsafe { LoadLibraryW(library_name.as_ptr()) };
    if library.is_null() {
        return Err(io::Error::last_os_error());
    }
    let address = unsafe { GetProcAddress(library, c"SystemFunction036".as_ptr().cast()) };
    let Some(address) = address else {
        unsafe { FreeLibrary(library) };
        return Err(io::Error::last_os_error());
    };
    let function: RtlGenRandom = unsafe { std::mem::transmute(address) };
    let mut value = [0u8; 16];
    let ok = unsafe { function(value.as_mut_ptr().cast(), value.len() as u32) };
    unsafe { FreeLibrary(library) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

#[cfg(target_os = "linux")]
pub(super) fn process_start_identity(pid: u32) -> io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let after_name = stat
        .rsplit_once(") ")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid proc stat"))?
        .1;
    after_name
        .split_ascii_whitespace()
        .nth(19)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing process start time"))?
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid process start time"))
}

#[cfg(windows)]
pub(super) fn process_start_identity(pid: u32) -> io::Result<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut kernel = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut user = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }
}

#[cfg(target_os = "linux")]
pub(super) fn terminate_process(identity: &ProcessIdentity, timeout: Duration) -> io::Result<()> {
    let fd = unsafe {
        libc::syscall(libc::SYS_pidfd_open, identity.pid as libc::pid_t, 0) as libc::c_int
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    match process_start_identity(identity.pid) {
        Ok(start) if start == identity.start => {}
        Err(error) if process_missing_error(&error) => {
            unsafe { libc::close(fd) };
            return Ok(());
        }
        _ => {
            unsafe { libc::close(fd) };
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "process identity changed",
            ));
        }
    }
    let sent = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd,
            libc::SIGKILL,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    if sent < 0 {
        let error = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error);
    }
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    let waited = unsafe { libc::poll(&mut pollfd, 1, millis) };
    unsafe { libc::close(fd) };
    if waited > 0 {
        Ok(())
    } else if waited == 0 {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "process did not exit",
        ))
    } else {
        Err(io::Error::last_os_error())
    }
}
#[cfg(windows)]
pub(super) fn terminate_process(identity: &ProcessIdentity, timeout: Duration) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };
    let handle = unsafe {
        OpenProcess(
            PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            identity.pid,
        )
    };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut kernel = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut user = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let times =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    let start = (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
    if times == 0 || start != identity.start {
        unsafe { CloseHandle(handle) };
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "process identity changed",
        ));
    }
    if unsafe { TerminateProcess(handle, 75) } == 0 {
        let error = io::Error::last_os_error();
        unsafe { CloseHandle(handle) };
        return Err(error);
    }
    let waited =
        unsafe { WaitForSingleObject(handle, timeout.as_millis().min(u32::MAX as u128) as u32) };
    unsafe { CloseHandle(handle) };
    if waited == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "process did not exit",
        ))
    }
}

#[cfg(unix)]
pub(super) fn atomic_swap(target: &Path, staged: &Path, backup: &Path) -> io::Result<()> {
    remove_any(backup)?;
    fs::hard_link(target, backup)?;
    File::open(backup)?.sync_all()?;
    sync_parent(target)?;
    fs::rename(staged, target)?;
    sync_parent(target)
}
#[cfg(unix)]
pub(super) fn restore_backup(target: &Path, backup: &Path) -> io::Result<()> {
    fs::rename(backup, target)?;
    File::open(target)?.sync_all()?;
    sync_parent(target)
}

#[cfg(windows)]
pub(super) fn atomic_swap(target: &Path, staged: &Path, backup: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW};
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let target = wide(target);
    let staged = wide(staged);
    let backup = wide(backup);
    let ok = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            staged.as_ptr(),
            backup.as_ptr(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[cfg(windows)]
pub(super) fn restore_backup(target: &Path, backup: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let target = wide(target);
    let backup = wide(backup);
    let ok = unsafe {
        MoveFileExW(
            backup.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
