//! Process identities for death proofs (`docs/testing/verification.md`,
//! "Process identity"): a Linux pidfd or a Windows process handle, opened
//! while the process is known alive and checked against the start time the
//! process itself reported (or read at once for our own unreaped child).
//! Death is read from that pidfd or handle, never from PID absence.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One tracked process.
pub struct Tracked {
    pub pid: u32,
    /// Linux: `/proc/<pid>/stat` start time; Windows: creation `FILETIME`.
    pub start: u64,
    #[cfg(target_os = "linux")]
    pidfd: std::os::fd::OwnedFd,
    #[cfg(windows)]
    handle: std::os::windows::io::OwnedHandle,
}

impl std::fmt::Debug for Tracked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "process {} (start {})", self.pid, self.start)
    }
}

impl Tracked {
    /// Opens process `pid` and checks it is the process that reported
    /// `start` (`None`: our own unreaped child, whose PID cannot be reused).
    pub fn open(pid: u32, start: Option<u64>) -> Self {
        let tracked = Self::open_os(pid);
        if let Some(start) = start {
            assert_eq!(tracked.start, start, "process {pid} is the reported one");
        }
        assert!(!tracked.exited(), "process {pid} is alive when opened");
        tracked
    }

    /// Waits up to `bound` for the process to end; returns how long it took.
    pub fn wait_exit(&self, bound: Duration) -> Option<Duration> {
        let started = Instant::now();
        self.wait_os(bound).then(|| started.elapsed())
    }

    pub fn exited(&self) -> bool {
        self.wait_os(Duration::ZERO)
    }
}

/// The start-time token of the calling process, as [`Tracked::start`].
pub fn own_start() -> u64 {
    #[cfg(target_os = "linux")]
    {
        linux_start(std::process::id()).unwrap()
    }
    #[cfg(windows)]
    {
        // SAFETY: the current-process pseudo handle is always valid.
        windows_start(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcess() })
    }
}

/// The parent PID of a live process, read from the OS.
pub fn parent_of(pid: u32) -> Option<u32> {
    #[cfg(target_os = "linux")]
    {
        stat_field(pid, 1)?.parse().ok()
    }
    #[cfg(windows)]
    {
        windows_processes()
            .into_iter()
            .find(|(child, _)| *child == pid)
            .map(|(_, parent)| parent)
    }
}

/// The executable image of a live process.
pub fn image_of(tracked: &Tracked) -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{}/exe", tracked.pid)).unwrap()
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::System::Threading::{
            PROCESS_NAME_WIN32, QueryFullProcessImageNameW,
        };
        let mut buffer = vec![0_u16; 32_768];
        let mut len = buffer.len() as u32;
        // SAFETY: a live handle with query access and a writable buffer of
        // `len` UTF-16 units.
        let ok = unsafe {
            QueryFullProcessImageNameW(
                tracked.handle.as_raw_handle(),
                PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(),
                &mut len,
            )
        };
        assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
        use std::os::windows::ffi::OsStringExt as _;
        PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len as usize]))
    }
}

/// Whether two paths name the same file.
pub fn same_file(left: &Path, right: &Path) -> bool {
    std::fs::canonicalize(left).unwrap() == std::fs::canonicalize(right).unwrap()
}

#[cfg(target_os = "linux")]
fn stat_field(pid: u32, index: usize) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = &stat[stat.rfind(')')? + 2..];
    after_name.split_whitespace().nth(index).map(str::to_owned)
}

#[cfg(target_os = "linux")]
fn linux_start(pid: u32) -> Option<u64> {
    stat_field(pid, 19)?.parse().ok()
}

/// The children of `pid` (every thread's), live or zombie.
#[cfg(target_os = "linux")]
pub fn children_of(pid: u32) -> Vec<u32> {
    let mut children = Vec::new();
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return children;
    };
    for task in tasks.flatten() {
        let text = std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
        children.extend(
            text.split_whitespace()
                .filter_map(|pid| pid.parse::<u32>().ok()),
        );
    }
    children
}

/// `argv[1]` of a live process.
#[cfg(target_os = "linux")]
pub fn argv1_of(pid: u32) -> Option<String> {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    cmdline
        .split(|byte| *byte == 0)
        .nth(1)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
}

#[cfg(target_os = "linux")]
impl Tracked {
    fn open_os(pid: u32) -> Self {
        Self::try_open(pid)
            .unwrap_or_else(|| panic!("pidfd_open({pid}): {}", std::io::Error::last_os_error()))
    }

    /// Opens a pidfd for `pid` if it still exists (a zombie included).
    pub fn try_open(pid: u32) -> Option<Self> {
        use std::os::fd::FromRawFd as _;
        // SAFETY: plain syscall; the result is checked before ownership.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if fd < 0 {
            return None;
        }
        // SAFETY: a fresh descriptor this value now owns.
        let pidfd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) };
        // Read after the pidfd exists: the pidfd names this exact process.
        let start = linux_start(pid)?;
        Some(Self { pid, start, pidfd })
    }

    /// The raw pidfd, for syscalls that take one.
    pub fn pidfd(&self) -> i32 {
        use std::os::fd::AsRawFd as _;
        self.pidfd.as_raw_fd()
    }

    fn wait_os(&self, bound: Duration) -> bool {
        use std::os::fd::AsRawFd as _;
        let deadline = Instant::now() + bound;
        loop {
            let mut poll = libc::pollfd {
                fd: self.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let remaining = deadline.saturating_duration_since(Instant::now());
            // SAFETY: one valid pollfd for the duration of the call.
            let ready = unsafe { libc::poll(&mut poll, 1, remaining.as_millis() as i32) };
            if ready > 0 {
                return true;
            }
            let interrupted = ready < 0
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
            if !interrupted || Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// Sends `signal` through the pidfd (never to a reused PID).
    pub fn signal(&self, signal: i32) {
        use std::os::fd::AsRawFd as _;
        // SAFETY: plain syscall on a live pidfd; no siginfo.
        let sent = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        let error = std::io::Error::last_os_error();
        assert!(
            sent == 0 || error.raw_os_error() == Some(libc::ESRCH),
            "pidfd_send_signal: {error}"
        );
    }

    pub fn kill(&self) {
        self.signal(libc::SIGKILL);
    }

    pub fn suspend(&self) {
        self.signal(libc::SIGSTOP);
    }

    /// Whether `/proc` no longer shows this exact process: it was reaped.
    pub fn reaped(&self) -> bool {
        linux_start(self.pid) != Some(self.start)
    }

    /// Linux exposes no exit status to a non-parent.
    pub fn exit_code(&self) -> Option<u32> {
        None
    }
}

#[cfg(windows)]
fn windows_start(handle: windows_sys::Win32::Foundation::HANDLE) -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetProcessTimes;
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: a live process handle with query access; four writable
    // FILETIMEs.
    let ok = unsafe { GetProcessTimes(handle, created, exited, kernel, user) };
    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
    u64::from(times[0].dwHighDateTime) << 32 | u64::from(times[0].dwLowDateTime)
}

/// Every process as `(pid, parent pid)`.
#[cfg(windows)]
fn windows_processes() -> Vec<(u32, u32)> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    // SAFETY: a process snapshot; the handle is closed below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    assert_ne!(snapshot, INVALID_HANDLE_VALUE);
    // SAFETY: PROCESSENTRY32W is plain data, for which all-zero is valid.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut found = Vec::new();
    // SAFETY: `entry` is a writable PROCESSENTRY32W with `dwSize` set.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        found.push((entry.th32ProcessID, entry.th32ParentProcessID));
        // SAFETY: as for `Process32FirstW`.
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    // SAFETY: closes the snapshot handle opened above.
    unsafe { CloseHandle(snapshot) };
    found
}

#[cfg(windows)]
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtSuspendProcess(process: windows_sys::Win32::Foundation::HANDLE) -> i32;
}

#[cfg(windows)]
impl Tracked {
    fn open_os(pid: u32) -> Self {
        use std::os::windows::io::FromRawHandle as _;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SUSPEND_RESUME, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        };
        // SAFETY: plain call; a null result is checked before ownership.
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_INFORMATION
                    | PROCESS_QUERY_LIMITED_INFORMATION
                    | PROCESS_SYNCHRONIZE
                    | PROCESS_TERMINATE
                    | PROCESS_SUSPEND_RESUME,
                0,
                pid,
            )
        };
        assert!(
            !raw.is_null(),
            "OpenProcess({pid}): {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: a fresh handle this value now owns.
        let handle = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) };
        let start = windows_start(raw);
        Self { pid, start, handle }
    }

    fn wait_os(&self, bound: Duration) -> bool {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        let millis = u32::try_from(bound.as_millis()).unwrap_or(u32::MAX - 1);
        // SAFETY: waits on a live owned handle.
        unsafe { WaitForSingleObject(self.handle.as_raw_handle(), millis) == WAIT_OBJECT_0 }
    }

    pub fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle as _;
        self.handle.as_raw_handle()
    }

    pub fn kill(&self) {
        use windows_sys::Win32::System::Threading::TerminateProcess;
        // SAFETY: a live handle with terminate access; failure means the
        // process already ended.
        unsafe { TerminateProcess(self.raw(), 1) };
    }

    pub fn suspend(&self) {
        // SAFETY: a live handle with suspend access.
        let status = unsafe { NtSuspendProcess(self.raw()) };
        assert!(status >= 0, "NtSuspendProcess: {status:#x}");
    }

    /// The exit code, once the process ended.
    pub fn exit_code(&self) -> Option<u32> {
        use windows_sys::Win32::System::Threading::GetExitCodeProcess;
        if !self.exited() {
            return None;
        }
        let mut code = 0;
        // SAFETY: a live handle with query access and a writable u32.
        let ok = unsafe { GetExitCodeProcess(self.raw(), &mut code) };
        (ok != 0).then_some(code)
    }
}
