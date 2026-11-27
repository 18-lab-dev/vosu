//! Platform-specific daemon launch, controlled shutdown, and orphan verification
//! for `release_smoke.rs` (Unix process groups + SIGTERM; Windows ADR 0024
//! console-less suspended spawn + Job Object assignment before resume).

use std::path::Path;

#[cfg(unix)]
use std::{
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use super::{HARNESS_CAPABILITY, HARNESS_CAPABILITY_ENV};

#[cfg(unix)]
use super::apply_isolated_env;

#[cfg(unix)]
pub(super) struct ChildGuard {
    child: Option<Child>,
}

#[cfg(unix)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let pgid = child.id() as libc::pid_t;
            // SAFETY: `libc::kill` with a negative process group id and
            // `SIGKILL` has no memory safety preconditions; the daemon was
            // spawned in its own process group via `process_group(0)`.
            let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(windows)]
pub(super) struct ChildGuard {
    process: Option<std::os::windows::io::OwnedHandle>,
    job: windows_process::SmokeJob,
}

#[cfg(windows)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            windows_process::terminate_and_reap(&process, 5_000);
        }
    }
}

#[cfg(unix)]
pub(super) fn spawn_release_daemon(binary: &Path, roots: &Path) -> ChildGuard {
    use std::os::unix::process::CommandExt;

    let mut command = Command::new(binary);
    command.arg("--update=false");
    apply_isolated_env(&mut command, roots);
    // A release build has no harness endpoint to mount, even when offered a
    // well-formed capability.
    command
        .env(HARNESS_CAPABILITY_ENV, HARNESS_CAPABILITY)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let spawned = command.spawn().expect("spawn unpacked release executable");
    ChildGuard {
        child: Some(spawned),
    }
}

#[cfg(windows)]
pub(super) fn spawn_release_daemon(binary: &Path, roots: &Path) -> ChildGuard {
    windows_process::spawn_consoleless_in_job(binary, roots)
        .expect("spawn console-less Windows release daemon inside job object")
}

#[cfg(unix)]
pub(super) async fn shutdown_and_verify_no_orphans(guard: &mut ChildGuard) {
    let pid = guard.child.as_ref().expect("running daemon").id() as libc::pid_t;
    // SAFETY: `libc::kill` with a valid pid and `SIGTERM` has no memory safety
    // preconditions; `pid` is this test's own spawned daemon process.
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(result, 0, "send release daemon SIGTERM");

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = guard.child.as_mut().unwrap().try_wait().unwrap() {
            assert!(
                status.success(),
                "release daemon exited unsuccessfully after SIGTERM: {status}"
            );
            guard.child.take();
            break;
        }
        assert!(
            Instant::now() < deadline,
            "release daemon failed to drain after SIGTERM"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let orphan_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        // SAFETY: signal 0 probes whether any process remains in the daemon's
        // dedicated process group without delivering a signal.
        let probe = unsafe { libc::kill(-pid, 0) };
        if probe == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return;
        }
        assert!(
            Instant::now() < orphan_deadline,
            "release daemon left orphan child process(es) in process group {pid}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(windows)]
pub(super) async fn shutdown_and_verify_no_orphans(guard: &mut ChildGuard) {
    // ADR 0024: the console-less daemon cannot rely on CTRL_BREAK console
    // attachment. Terminate the daemon process directly, wait for it to exit,
    // and verify via the Windows Job Object that zero child processes remain.
    let process = guard.process.take().expect("running Windows daemon");
    let exited = windows_process::terminate_and_reap(&process, 20_000);
    drop(process);
    assert!(
        exited,
        "Windows release daemon did not exit within 20s after controlled termination"
    );
    windows_process::assert_no_job_orphans(&guard.job).await;
}

#[cfg(windows)]
mod windows_process {
    use std::{
        collections::BTreeMap,
        ffi::OsString,
        io,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        path::Path,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_MORE_DATA, HANDLE, WAIT_OBJECT_0},
        System::{
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList,
                JobObjectExtendedLimitInformation, QueryInformationJobObject,
                SetInformationJobObject,
            },
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
                PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, TerminateProcess,
                WaitForSingleObject,
            },
        },
    };

    use super::{ChildGuard, HARNESS_CAPABILITY, HARNESS_CAPABILITY_ENV};

    const LISTED_IDS: usize = 64;

    #[repr(C)]
    struct IdList {
        assigned: u32,
        listed: u32,
        ids: [usize; LISTED_IDS],
    }

    pub(super) struct SmokeJob {
        handle: OwnedHandle,
    }

    impl SmokeJob {
        pub(super) fn create() -> io::Result<Self> {
            // SAFETY: null security attributes and unnamed job object have no
            // memory safety preconditions.
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `raw` is a fresh handle owned exclusively by this struct.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
            // SAFETY: all-zero `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is valid.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `handle` is live and `limits` is correctly sized.
            let ok = unsafe {
                SetInformationJobObject(
                    handle.as_raw_handle() as HANDLE,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { handle })
        }

        pub(super) fn assign(&self, process: &OwnedHandle) -> io::Result<()> {
            // SAFETY: both `self.handle` and `process` are live job/process
            // handles with full access returned by `CreateJobObjectW` and
            // `CreateProcessW`.
            let assigned = unsafe {
                AssignProcessToJobObject(
                    self.handle.as_raw_handle() as HANDLE,
                    process.as_raw_handle() as HANDLE,
                )
            };
            if assigned == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(super) fn assigned_processes(&self) -> io::Result<u32> {
            let mut list = IdList {
                assigned: 0,
                listed: 0,
                ids: [0; LISTED_IDS],
            };
            // SAFETY: `self.handle` is live and `list` is a writable
            // `JOBOBJECT_BASIC_PROCESS_ID_LIST`-compatible buffer.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.handle.as_raw_handle() as HANDLE,
                    JobObjectBasicProcessIdList,
                    (&raw mut list).cast(),
                    size_of::<IdList>() as u32,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_MORE_DATA as i32) {
                    return Err(error);
                }
            }
            Ok(list.assigned)
        }
    }

    fn build_environment_block(roots: &Path) -> Vec<u16> {
        let mut vars: BTreeMap<Vec<u16>, (OsString, OsString)> = BTreeMap::new();
        let sort_key = |name: &OsString| -> Vec<u16> {
            name.to_string_lossy()
                .to_uppercase()
                .encode_utf16()
                .collect()
        };
        for (key, value) in std::env::vars_os() {
            vars.insert(sort_key(&key), (key, value));
        }
        for removed in ["DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS"] {
            let key = OsString::from(removed);
            vars.remove(&sort_key(&key));
        }
        for (name, value) in [
            ("XDG_CONFIG_HOME", roots.join("config").into_os_string()),
            ("XDG_DATA_HOME", roots.join("data").into_os_string()),
            ("XDG_CACHE_HOME", roots.join("cache").into_os_string()),
            ("XDG_RUNTIME_DIR", roots.join("runtime").into_os_string()),
            ("HOME", roots.as_os_str().to_os_string()),
            ("LOCALAPPDATA", roots.join("local").into_os_string()),
            ("TEMP", roots.join("runtime-temp-sentinel").into_os_string()),
            ("OneDrive", roots.join("onedrive-sentinel").into_os_string()),
            (HARNESS_CAPABILITY_ENV, OsString::from(HARNESS_CAPABILITY)),
        ] {
            let key = OsString::from(name);
            vars.insert(sort_key(&key), (key, value));
        }
        let mut block = Vec::new();
        for (key, value) in vars.into_values() {
            block.extend(key.encode_wide());
            block.push(b'=' as u16);
            block.extend(value.encode_wide());
            block.push(0);
        }
        if block.is_empty() {
            block.push(0);
        }
        block.push(0);
        block
    }

    /// ADR 0024: spawn the packaged Windows daemon console-less and suspended
    /// (`CREATE_NO_WINDOW | CREATE_SUSPENDED`), attach it to `SmokeJob` while
    /// already guarded by `ChildGuard`, and only then resume its main thread.
    pub(super) fn spawn_consoleless_in_job(binary: &Path, roots: &Path) -> io::Result<ChildGuard> {
        let job = SmokeJob::create()?;
        let application: Vec<u16> = binary.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut command_line: Vec<u16> = std::iter::once(b'"' as u16)
            .chain(binary.as_os_str().encode_wide())
            .chain("\"".encode_utf16())
            .chain(" --update=false".encode_utf16())
            .chain(Some(0))
            .collect();
        let environment = build_environment_block(roots);

        // SAFETY: all-zero `STARTUPINFOW` and `PROCESS_INFORMATION` are valid.
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = size_of::<STARTUPINFOW>() as u32;
        // SAFETY: all-zero `PROCESS_INFORMATION` is valid.
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

        // SAFETY: `application` is NUL-terminated UTF-16, `command_line` is a
        // writable NUL-terminated UTF-16 buffer, `environment` is a double-NUL
        // terminated sorted UTF-16 block matching `CREATE_UNICODE_ENVIRONMENT`,
        // `startup` has `cb` initialized, and `info` is writable.
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                std::ptr::null(),
                &startup,
                &mut info,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: on success both `hProcess` and `hThread` are fresh handles
        // owned exclusively by the caller.
        let (process, thread) = unsafe {
            (
                OwnedHandle::from_raw_handle(info.hProcess.cast()),
                OwnedHandle::from_raw_handle(info.hThread.cast()),
            )
        };
        // Guard immediately before assigning to the job or resuming so any
        // error terminates and reaps the suspended process without leaking it.
        let guard = ChildGuard {
            process: Some(process),
            job,
        };
        guard.job.assign(guard.process.as_ref().unwrap())?;
        // SAFETY: `thread` is the live suspended primary thread handle returned
        // by `CreateProcessW` and already assigned to `guard.job`.
        let resumed = unsafe { ResumeThread(thread.as_raw_handle() as HANDLE) };
        if resumed == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        drop(thread);
        Ok(guard)
    }

    pub(super) fn terminate_and_reap(process: &OwnedHandle, timeout_ms: u32) -> bool {
        // SAFETY: `process` is a live handle with `PROCESS_TERMINATE` access.
        let _ = unsafe { TerminateProcess(process.as_raw_handle() as HANDLE, 0) };
        // SAFETY: `process` is a live synchronizable process handle.
        let waited = unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, timeout_ms) };
        waited == WAIT_OBJECT_0
    }

    pub(super) async fn assert_no_job_orphans(job: &SmokeJob) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = job
                .assigned_processes()
                .expect("query Windows smoke job process list");
            if remaining == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "release daemon left {remaining} orphan child process(es) in Windows job"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
