//! Core's Linux plugin-host spawn (`docs/v2/plugin-abi.md` §7.2, §7.3,
//! §7.6 "Launch", A5.8).
//!
//! Every plugin host is forked by one long-lived launcher thread
//! ([`PluginHostLauncher`]): `PR_SET_PDEATHSIG` fires when the *thread* that
//! created a child exits, so a transient pool thread would kill healthy
//! children. The thread lives until the launcher and every
//! [`PluginHostChild`] it produced are dropped, and a child is dropped only
//! after it has been reaped.
//!
//! Between `fork` and `execve` the child runs only async-signal-safe
//! syscalls on memory prepared before the fork:
//!
//! 1. arms `SIGKILL` as its parent-death signal and checks the parent is
//!    still core (core may have died before the arm);
//! 2. sets `no_new_privs`, so the `execve` grants no file capabilities and
//!    the plugin host never holds one, even before its own §7.3 drop (which
//!    still runs and still verifies);
//! 3. leaves core's process group, so terminal job-control signals aimed at
//!    core do not kill plugin hosts behind core's controlled teardown;
//! 4. clears the signal mask and restores `SIGPIPE`'s default;
//! 5. installs exactly the inherited descriptors: the null device on 0–2,
//!    the four endpoint objects on [`ENDPOINT_FDS`], and the close-on-exec
//!    error pipe on 7; every other descriptor is closed;
//! 6. `execve`s the requested executable with the exact §7.6 arguments.
//!
//! A failing child step writes its stage and `errno` to the error pipe, so
//! the parent reports it and reaps the child. End of file on that pipe means
//! the `execve` succeeded.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::super::RingObjects;
use super::channel::{ENDPOINT_ARGUMENT, ENDPOINT_FDS};

/// The child's argument vector before the endpoint values: `argv[0]` and
/// the exact §7.6 command.
const ARGV0: &str = "vosu";
const COMMAND: &str = "plugin-host";
/// Optional §7.6 argument, sent only for an approved opt-in.
pub const USER_NAMESPACES_ARGUMENT: &str = "--allow-user-namespaces";
/// Where the child keeps its error pipe until `execve` closes it.
const ERROR_FD: RawFd = 7;
/// Lowest descriptor the parent's temporary copies use, so moving the
/// copies onto 0–7 in the child never overwrites one not yet moved.
const SCRATCH_FLOOR: RawFd = ERROR_FD + 1;
/// Pending spawn requests the launcher thread holds; callers block beyond.
const LAUNCH_QUEUE: usize = 8;
/// The child's `_exit` status when a pre-exec step fails.
const PRE_EXEC_FAILURE: libc::c_int = 127;

/// One plugin host to start.
#[derive(Debug)]
pub struct PluginHostCommand<'a> {
    /// The executable to run. Core passes `/proc/self/exe`, its own exact
    /// generation (§7.6).
    pub executable: &'a Path,
    /// The compile-time build identity passed as the first argument after
    /// `plugin-host`.
    pub build_identity: &'a str,
    pub allow_user_namespaces: bool,
    pub host_to_plugin: &'a RingObjects,
    pub plugin_to_host: &'a RingObjects,
}

/// The pre-exec step that failed in the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildStage {
    ParentDeath,
    NoNewPrivs,
    ProcessGroup,
    Signals,
    Descriptors,
    Exec,
    /// The pipe carried something other than one stage report.
    Unknown,
}

impl ChildStage {
    fn code(self) -> u32 {
        match self {
            Self::ParentDeath => 1,
            Self::NoNewPrivs => 2,
            Self::ProcessGroup => 3,
            Self::Signals => 4,
            Self::Descriptors => 5,
            Self::Exec => 6,
            Self::Unknown => 0,
        }
    }

    fn from_code(code: u32) -> Self {
        match code {
            1 => Self::ParentDeath,
            2 => Self::NoNewPrivs,
            3 => Self::ProcessGroup,
            4 => Self::Signals,
            5 => Self::Descriptors,
            6 => Self::Exec,
            _ => Self::Unknown,
        }
    }
}

/// Why a plugin host could not be started (startup stage `spawn`). The
/// child, if one was forked, has been reaped.
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("invalid plugin-host command: {0}")]
    InvalidCommand(&'static str),
    #[error("the plugin-host launcher thread has stopped")]
    LauncherStopped,
    #[error("{operation} failed: {source}")]
    Os {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("plugin-host child failed before exec at {stage:?}: errno {errno}")]
    Child { stage: ChildStage, errno: i32 },
}

fn os(operation: &'static str) -> impl FnOnce(io::Error) -> SpawnError {
    move |source| SpawnError::Os { operation, source }
}

/// The dedicated thread that forks every plugin host (A5.8).
#[derive(Debug)]
pub struct PluginHostLauncher {
    requests: SyncSender<Request>,
}

struct Request {
    prepared: Prepared,
    reply: SyncSender<Result<Spawned, SpawnError>>,
}

impl PluginHostLauncher {
    /// Starts the launcher thread. Its lifetime ends only when this value
    /// and every child it spawned are gone.
    pub fn start() -> io::Result<Self> {
        let (requests, queue) = sync_channel::<Request>(LAUNCH_QUEUE);
        std::thread::Builder::new()
            .name("vosu-plugin-launcher".to_owned())
            .spawn(move || {
                while let Ok(request) = queue.recv() {
                    // A caller that stopped waiting (it panicked) cannot own
                    // its child: kill and reap it here.
                    if let Err(unsent) = request.reply.send(spawn_here(request.prepared))
                        && let Ok(spawned) = unsent.0
                    {
                        kill_and_reap_pid(spawned.pid);
                    }
                }
            })?;
        Ok(Self { requests })
    }

    /// Starts one plugin host and returns once its `execve` succeeded, or
    /// fails after reaping it. Blocks the caller only for the fork and
    /// `execve`, never for the child's own startup.
    pub fn spawn(&self, command: &PluginHostCommand<'_>) -> Result<PluginHostChild, SpawnError> {
        self.spawn_with_env(command, &[])
    }

    /// [`spawn`](Self::spawn) with `extra_env` appended to the inherited
    /// environment.
    pub(crate) fn spawn_with_env(
        &self,
        command: &PluginHostCommand<'_>,
        extra_env: &[(&OsStr, &OsStr)],
    ) -> Result<PluginHostChild, SpawnError> {
        let prepared = Prepared::new(command, extra_env)?;
        let (reply, response) = sync_channel(1);
        self.requests
            .send(Request { prepared, reply })
            .map_err(|_| SpawnError::LauncherStopped)?;
        let spawned = response.recv().map_err(|_| SpawnError::LauncherStopped)??;
        Ok(PluginHostChild::new(spawned, Some(self.requests.clone())))
    }
}

/// Forks `command` from the calling thread, without the launcher. The
/// child's parent-death signal is then tied to the calling thread, so tests
/// use this to show why production spawns only from the launcher thread.
#[cfg(test)]
pub(super) fn spawn_from_current_thread(
    command: &PluginHostCommand<'_>,
    extra_env: &[(&OsStr, &OsStr)],
) -> Result<PluginHostChild, SpawnError> {
    let spawned = spawn_here(Prepared::new(command, extra_env)?)?;
    Ok(PluginHostChild::new(spawned, None))
}

/// Everything the child needs, built on the caller's thread before the
/// fork: no allocation happens between `fork` and `execve`.
struct Prepared {
    program: CString,
    argv: Vec<CString>,
    env: Vec<CString>,
    /// Close-on-exec copies at or above [`SCRATCH_FLOOR`], in
    /// [`ENDPOINT_FDS`] order.
    endpoints: [OwnedFd; 4],
    null: OwnedFd,
}

impl Prepared {
    fn new(
        command: &PluginHostCommand<'_>,
        extra_env: &[(&OsStr, &OsStr)],
    ) -> Result<Self, SpawnError> {
        let program = CString::new(command.executable.as_os_str().as_bytes())
            .map_err(|_| SpawnError::InvalidCommand("executable path contains NUL"))?;
        if command.build_identity.is_empty() {
            return Err(SpawnError::InvalidCommand("empty build identity"));
        }
        let identity = CString::new(command.build_identity)
            .map_err(|_| SpawnError::InvalidCommand("build identity contains NUL"))?;
        // SAFETY: `getpid` has no preconditions.
        let core_pid = unsafe { libc::getpid() };
        let mut argv = Vec::with_capacity(5 + usize::from(command.allow_user_namespaces));
        argv.extend([
            CString::new(ARGV0).expect("constant has no NUL"),
            CString::new(COMMAND).expect("constant has no NUL"),
            identity,
            CString::new(core_pid.to_string()).expect("decimal has no NUL"),
        ]);
        if command.allow_user_namespaces {
            argv.push(CString::new(USER_NAMESPACES_ARGUMENT).expect("constant has no NUL"));
        }
        argv.push(CString::new(ENDPOINT_ARGUMENT).expect("constant has no NUL"));
        let mut env = Vec::new();
        for (key, value) in std::env::vars_os().chain(
            extra_env
                .iter()
                .map(|(key, value)| (key.to_os_string(), value.to_os_string())),
        ) {
            env.push(env_entry(&key, &value)?);
        }
        let endpoints = [
            command.host_to_plugin.mapping_fd(),
            command.host_to_plugin.doorbell_fd(),
            command.plugin_to_host.mapping_fd(),
            command.plugin_to_host.doorbell_fd(),
        ]
        .map(scratch_copy);
        let [a, b, c, d] = endpoints;
        let endpoints = [a?, b?, c?, d?];
        // SAFETY: opens a fixed NUL-terminated path, close-on-exec.
        let null = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if null < 0 {
            return Err(os("open /dev/null")(io::Error::last_os_error()));
        }
        // SAFETY: `open` returned a new descriptor this function owns.
        let null = scratch_copy(unsafe { OwnedFd::from_raw_fd(null) }.as_fd())?;
        Ok(Self {
            program,
            argv,
            env,
            endpoints,
            null,
        })
    }
}

fn env_entry(key: &OsString, value: &OsString) -> Result<CString, SpawnError> {
    let mut entry = Vec::with_capacity(key.len() + value.len() + 1);
    entry.extend_from_slice(key.as_bytes());
    entry.push(b'=');
    entry.extend_from_slice(value.as_bytes());
    CString::new(entry).map_err(|_| SpawnError::InvalidCommand("environment contains NUL"))
}

/// A close-on-exec duplicate of `fd` at or above [`SCRATCH_FLOOR`].
fn scratch_copy(fd: BorrowedFd<'_>) -> Result<OwnedFd, SpawnError> {
    // SAFETY: duplicates an open descriptor; no pointers involved.
    let raw = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, SCRATCH_FLOOR) };
    if raw < 0 {
        return Err(os("duplicate endpoint descriptor")(
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: `fcntl` returned a new descriptor this function owns.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// A forked, successfully `execve`d child.
struct Spawned {
    pid: libc::pid_t,
    pidfd: OwnedFd,
}

/// Forks and execs one child on the calling (launcher) thread.
fn spawn_here(prepared: Prepared) -> Result<Spawned, SpawnError> {
    let mut pipe = [0 as RawFd; 2];
    // SAFETY: writes two descriptors into the live local array.
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(os("create error pipe")(io::Error::last_os_error()));
    }
    // SAFETY: `pipe2` returned two new descriptors this function owns.
    let (read_end, pipe_write) =
        unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
    // Only the scratch copy may stay open: any other write end held here
    // would hide the end of file that reports a successful `execve`.
    let write_end = scratch_copy(pipe_write.as_fd())?;
    drop(pipe_write);

    let argv: Vec<*const libc::c_char> = prepared
        .argv
        .iter()
        .map(|arg| arg.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    let envp: Vec<*const libc::c_char> = prepared
        .env
        .iter()
        .map(|entry| entry.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();
    // SAFETY: `sigemptyset` initializes the live local set.
    let mut empty_mask = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    // SAFETY: as above.
    unsafe { libc::sigemptyset(&mut empty_mask) };
    let plan = ChildPlan {
        // SAFETY: `getpid` has no preconditions.
        parent: unsafe { libc::getpid() },
        program: prepared.program.as_ptr(),
        argv: argv.as_ptr(),
        envp: envp.as_ptr(),
        endpoints: prepared.endpoints.each_ref().map(AsRawFd::as_raw_fd),
        null: prepared.null.as_raw_fd(),
        error: write_end.as_raw_fd(),
        empty_mask,
    };

    // SAFETY: the child branch calls only `run_child`, which performs
    // async-signal-safe syscalls on `plan`'s memory (all built above and
    // alive in the child's copy of this address space) and never returns.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(os("fork")(io::Error::last_os_error()));
    }
    if pid == 0 {
        // SAFETY: this is the freshly forked child; see `run_child`.
        unsafe { run_child(&plan) }
    }
    drop(write_end);
    drop(prepared);

    // SAFETY: `pidfd_open` on our own unreaped child: the PID cannot be
    // reused before we reap it.
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if pidfd < 0 {
        let error = io::Error::last_os_error();
        kill_and_reap_pid(pid);
        return Err(os("pidfd_open")(error));
    }
    // SAFETY: `pidfd_open` returned a new descriptor this function owns.
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd as RawFd) };

    match read_report(&read_end) {
        Ok(None) => Ok(Spawned { pid, pidfd }),
        Ok(Some((stage, errno))) => {
            kill_and_reap_pid(pid);
            Err(SpawnError::Child { stage, errno })
        }
        Err(error) => {
            kill_and_reap_pid(pid);
            Err(os("read child error pipe")(error))
        }
    }
}

/// Reads the child's report: `None` at end of file (its `execve`
/// succeeded), or its failing stage and `errno`.
fn read_report(read_end: &OwnedFd) -> io::Result<Option<(ChildStage, i32)>> {
    let mut report = [0_u8; 8];
    let mut filled = 0;
    while filled < report.len() {
        // SAFETY: reads into the unfilled tail of the live local buffer.
        let read = unsafe {
            libc::read(
                read_end.as_raw_fd(),
                report[filled..].as_mut_ptr().cast(),
                report.len() - filled,
            )
        };
        if read < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if read == 0 {
            break;
        }
        filled += read as usize;
    }
    match filled {
        0 => Ok(None),
        8 => {
            let stage = u32::from_ne_bytes(report[..4].try_into().expect("four bytes"));
            let errno = i32::from_ne_bytes(report[4..].try_into().expect("four bytes"));
            Ok(Some((ChildStage::from_code(stage), errno)))
        }
        _ => Ok(Some((ChildStage::Unknown, 0))),
    }
}

/// Kills and reaps an unreaped child by PID (no pidfd yet). The PID stays
/// ours until `waitpid` returns.
fn kill_and_reap_pid(pid: libc::pid_t) {
    // SAFETY: `pid` is our unreaped child; signals and waits take integers.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        let mut status = 0;
        while libc::waitpid(pid, &mut status, 0) < 0
            && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
        {}
    }
}

/// Raw pointers and descriptors the child uses between `fork` and
/// `execve`.
struct ChildPlan {
    parent: libc::pid_t,
    program: *const libc::c_char,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
    endpoints: [RawFd; 4],
    null: RawFd,
    error: RawFd,
    empty_mask: libc::sigset_t,
}

/// The child between `fork` and `execve`.
///
/// # Safety
///
/// Call only in a freshly forked child of a possibly multi-threaded
/// process, with `plan` built before the fork. Performs only
/// async-signal-safe syscalls: no allocation, locking or unwinding.
unsafe fn run_child(plan: &ChildPlan) -> ! {
    let mut error_fd = plan.error;
    // SAFETY: every call below is a raw async-signal-safe
    // syscall wrapper; pointers come from `plan`, which outlives `execve`.
    unsafe {
        if libc::prctl(
            libc::PR_SET_PDEATHSIG,
            libc::SIGKILL as libc::c_ulong,
            0,
            0,
            0,
        ) != 0
        {
            child_fail(error_fd, ChildStage::ParentDeath);
        }
        if libc::getppid() != plan.parent {
            // Core is already gone: nobody reads the pipe.
            libc::_exit(PRE_EXEC_FAILURE);
        }
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            child_fail(error_fd, ChildStage::NoNewPrivs);
        }
        if libc::setpgid(0, 0) != 0 {
            child_fail(error_fd, ChildStage::ProcessGroup);
        }
        if libc::sigprocmask(libc::SIG_SETMASK, &plan.empty_mask, std::ptr::null_mut()) != 0
            || libc::signal(libc::SIGPIPE, libc::SIG_DFL) == libc::SIG_ERR
        {
            child_fail(error_fd, ChildStage::Signals);
        }
        // Every source is at or above `SCRATCH_FLOOR`, so no move below
        // overwrites a source still to be moved. `dup2` clears
        // close-on-exec on the target.
        for (&source, &target) in plan.endpoints.iter().zip(ENDPOINT_FDS.iter()) {
            if libc::dup2(source, target) != target {
                child_fail(error_fd, ChildStage::Descriptors);
            }
        }
        for target in 0..=2 {
            if libc::dup2(plan.null, target) != target {
                child_fail(error_fd, ChildStage::Descriptors);
            }
        }
        if libc::dup2(error_fd, ERROR_FD) != ERROR_FD {
            child_fail(error_fd, ChildStage::Descriptors);
        }
        error_fd = ERROR_FD;
        if libc::fcntl(ERROR_FD, libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
            child_fail(error_fd, ChildStage::Descriptors);
        }
        close_from(SCRATCH_FLOOR);
        libc::execve(plan.program, plan.argv, plan.envp);
        child_fail(error_fd, ChildStage::Exec);
    }
}

/// Closes every descriptor at or above `floor` in the child.
///
/// # Safety
///
/// Only for the forked child before `execve`: closes descriptors other
/// code in the parent's image believes it owns.
unsafe fn close_from(floor: RawFd) {
    // SAFETY: integer arguments only.
    let closed = unsafe { libc::syscall(libc::SYS_close_range, floor as u32, u32::MAX, 0_u32) };
    if closed == 0 {
        return;
    }
    // Kernels before 5.9: close up to the descriptor limit.
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: writes one `rlimit` into the live local.
    let end = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        RawFd::try_from(limit.rlim_cur).unwrap_or(RawFd::MAX)
    } else {
        1 << 20
    };
    for fd in floor..end {
        // SAFETY: closing a possibly unused descriptor number; EBADF is
        // harmless.
        unsafe { libc::close(fd) };
    }
}

/// Reports `stage` and the current `errno` on the error pipe, then exits.
///
/// # Safety
///
/// Only for the forked child before `execve`.
unsafe fn child_fail(error_fd: RawFd, stage: ChildStage) -> ! {
    let errno = io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let mut report = [0_u8; 8];
    report[..4].copy_from_slice(&stage.code().to_ne_bytes());
    report[4..].copy_from_slice(&errno.to_ne_bytes());
    // SAFETY: writes 8 bytes from a live local buffer, then exits without
    // running any Rust or libc teardown.
    unsafe {
        libc::write(error_fd, report.as_ptr().cast(), report.len());
        libc::_exit(PRE_EXEC_FAILURE)
    }
}

/// One spawned plugin host, owned by core until reaped.
///
/// Dropping it kills (`SIGKILL` through the pidfd) and reaps the process if
/// that has not happened yet, so no plugin host outlives its handle. The
/// pidfd is pollable (readable once the process exits) for an event-driven
/// exit watcher, and signals through it cannot reach a reused PID.
#[derive(Debug)]
pub struct PluginHostChild {
    pid: libc::pid_t,
    pidfd: OwnedFd,
    status: Mutex<Option<ExitStatus>>,
    /// Keeps the launcher thread, the child's parent-death "parent",
    /// alive while this child may still run. `None` only for test spawns
    /// from the calling thread.
    _launcher: Option<SyncSender<Request>>,
}

impl PluginHostChild {
    fn new(spawned: Spawned, launcher: Option<SyncSender<Request>>) -> Self {
        Self {
            pid: spawned.pid,
            pidfd: spawned.pidfd,
            status: Mutex::new(None),
            _launcher: launcher,
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid.unsigned_abs()
    }

    /// The process's pidfd: readable once it has exited.
    pub fn exit_fd(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }

    /// Sends `SIGKILL`. Succeeds if the process already exited.
    pub fn kill(&self) -> io::Result<()> {
        // SAFETY: integer arguments and a null `siginfo`.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0_u32,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(error)
    }

    /// Reaps the process if it has exited, without waiting.
    pub fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(status) = *status {
            return Ok(Some(status));
        }
        // SAFETY: `waitid` writes one `siginfo_t` into the zeroed local.
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        loop {
            // SAFETY: waits on our own pidfd, writing into `info`.
            let result = unsafe {
                libc::waitid(
                    libc::P_PIDFD,
                    self.pidfd.as_raw_fd() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG,
                )
            };
            if result == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        // SAFETY: `waitid` succeeded; for `WEXITED` the `sigchld` fields are
        // valid, and `si_pid` is 0 when nothing has exited yet.
        let (pid, code, value) = unsafe { (info.si_pid(), info.si_code, info.si_status()) };
        if pid == 0 {
            return Ok(None);
        }
        let raw = match code {
            libc::CLD_EXITED => (value & 0xff) << 8,
            libc::CLD_DUMPED => (value & 0x7f) | 0x80,
            _ => value & 0x7f,
        };
        let exited = ExitStatus::from_raw(raw);
        *status = Some(exited);
        Ok(Some(exited))
    }

    /// Waits up to `timeout` for the process to exit, reaping it.
    pub fn wait_timeout(&self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            let remaining = match deadline {
                None => None,
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Ok(None);
                    }
                    Some(remaining)
                }
            };
            self.poll_exit(remaining)?;
        }
    }

    /// Waits for the process to exit, reaping it.
    pub fn wait(&self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            self.poll_exit(None)?;
        }
    }

    /// One `poll` on the pidfd. Interruption and timeout both return; the
    /// caller rechecks.
    fn poll_exit(&self, timeout: Option<Duration>) -> io::Result<()> {
        let millis = timeout.map_or(-1, |timeout| {
            libc::c_int::try_from(timeout.as_nanos().div_ceil(1_000_000))
                .unwrap_or(libc::c_int::MAX)
        });
        let mut poll = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid `pollfd` on a live descriptor.
        if unsafe { libc::poll(&mut poll, 1, millis) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        Ok(())
    }
}

impl Drop for PluginHostChild {
    fn drop(&mut self) {
        if matches!(self.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Err(error) = self.kill() {
            tracing::warn!(%error, pid = self.pid, "failed to kill a dropped plugin host");
        }
        if let Err(error) = self.wait() {
            tracing::warn!(%error, pid = self.pid, "failed to reap a dropped plugin host");
        }
    }
}
