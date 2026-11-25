//! Race-free creation of one `vosu plugin-host` process
//! (`docs/v2/plugin-abi.md` §7.4).
//!
//! 1. Create the strict [`PluginJob`] (kill-on-close, no breakaway,
//!    non-inheritable handle).
//! 2. Duplicate the four ring objects of §7.2 into inheritable handles with
//!    only the access the child needs, and name exactly those four in a
//!    `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. The job, process and thread
//!    handles, and the daemon's standard handles, are never on it.
//! 3. `CreateProcessW` with `CREATE_SUSPENDED`, no console and no standard
//!    handles, then close the inheritable duplicates at once.
//! 4. Assign the suspended process to the job and prove membership of
//!    *that* job: `IsProcessInJob` against the job's own handle, and the
//!    job's own process list holding exactly the child.
//! 5. Only then resume its initial thread.
//!
//! Any failure after creation terminates the suspended (or just-assigned)
//! process, waits for it within [`UNWIND_REAP_BOUND`], and closes every
//! handle; the child never runs a user-mode instruction unconfined.
//!
//! **Residual (§7.4 has no answer for it).** A handle list stops *this*
//! child from inheriting anything else, but the four duplicates are
//! inheritable for the duration of `CreateProcessW`. Another thread of this
//! process that creates a process with `bInheritHandles = TRUE` and no
//! handle list in that window (Rust's `std::process::Command`, and so
//! `tokio::process`, does exactly that) inherits copies of them. Every
//! plugin-host spawn here uses a handle list and so never picks up another
//! spawn's duplicates.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    DuplicateHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Memory::{SECTION_MAP_READ, SECTION_MAP_WRITE, SECTION_QUERY};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, CreateProcessW, DETACHED_PROCESS, DeleteProcThreadAttributeList,
    EVENT_MODIFY_STATE, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
    INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES,
    STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::channel::EndpointValues;
use super::job::PluginJob;
use crate::plugin_process::RingObjects;

/// `SYNCHRONIZE`: the standard right to wait on an object.
const SYNCHRONIZE: u32 = 0x0010_0000;

/// The access an inherited section handle keeps: map it read/write and
/// query its size, nothing else (no `WRITE_DAC`, no extend).
pub(super) const SECTION_ACCESS: u32 = SECTION_MAP_READ | SECTION_MAP_WRITE | SECTION_QUERY;
/// The access an inherited event handle keeps: wait on it and set it.
pub(super) const EVENT_ACCESS: u32 = SYNCHRONIZE | EVENT_MODIFY_STATE;

/// Exit code of a child terminated because its startup failed before it
/// was resumed (or as its resume failed).
pub const UNWIND_EXIT_CODE: u32 = 0x766F_7375;

/// Exit code of a plugin host [`PluginHostProcess::kill`] terminated.
pub const KILL_EXIT_CODE: u32 = 0x766F_736B;

/// How long a failed spawn waits for its terminated child to exit. A
/// suspended process ends as soon as it is terminated; this only bounds a
/// pathological wait.
pub const UNWIND_REAP_BOUND: Duration = Duration::from_secs(5);

/// One launched, confined, running plugin host.
///
/// Dropping it closes core's job handle, which terminates the plugin host
/// and every descendant still in its job (kill-on-close). Supervision keeps
/// it until the process has been reaped.
#[derive(Debug)]
pub struct PluginHostProcess {
    // Field order: the process handle closes before the job handle.
    process: OwnedHandle,
    pid: u32,
    job: PluginJob,
}

impl PluginHostProcess {
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The process handle, signaled when the plugin host exits. Exit
    /// watchers wait on it; identity is this handle, never the PID.
    pub fn process(&self) -> BorrowedHandle<'_> {
        self.process.as_handle()
    }

    /// The job confining the plugin host and its descendants.
    pub fn job(&self) -> &PluginJob {
        &self.job
    }

    /// Waits up to `timeout` (`None`: no limit) for the plugin host to exit
    /// and returns its exit code, or `None` if it is still running.
    pub fn wait(&self, timeout: Option<Duration>) -> io::Result<Option<u32>> {
        wait_exit(self.process.as_handle(), timeout)
    }

    /// Terminates the plugin host and every process in its job with
    /// [`KILL_EXIT_CODE`]; [`Self::wait`] then reaps it.
    pub fn kill(&self) -> io::Result<()> {
        self.job.terminate(KILL_EXIT_CODE)
    }
}

/// Which spawn step failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnStep {
    /// Creating or reading back the job.
    CreateJob,
    /// Building the inherited-handle list or the command line.
    PrepareChild,
    /// `CreateProcessW`.
    CreateProcess,
    /// `AssignProcessToJobObject`.
    AssignJob,
    /// The job-specific membership readback after assignment.
    VerifyJob,
    /// `ResumeThread`.
    Resume,
}

impl SpawnStep {
    /// The §7.6 startup stage this step reports: `job` for creating,
    /// assigning or verifying the job, `spawn` otherwise.
    pub fn startup_stage(self) -> &'static str {
        match self {
            Self::CreateJob | Self::AssignJob | Self::VerifyJob => "job",
            Self::PrepareChild | Self::CreateProcess | Self::Resume => "spawn",
        }
    }
}

/// What happened to a process created before its spawn failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnwoundChild {
    pub pid: u32,
    /// Its exit code, once it was observed to exit within
    /// [`UNWIND_REAP_BOUND`]; `None` if it was not.
    pub exit_code: Option<u32>,
}

/// A failed spawn. Nothing it created remains open, and any process it
/// created was terminated before this was returned.
#[derive(Debug, thiserror::Error)]
#[error("plugin-host spawn failed at {step:?}: {source}")]
pub struct SpawnError {
    pub step: SpawnStep,
    #[source]
    pub source: io::Error,
    pub unwound: Option<UnwoundChild>,
}

/// Starts `program` with `leading_args` followed by the endpoint argument
/// (`channel::EndpointValues`) naming the child's copies of both ring
/// directions, confined to a fresh [`PluginJob`] before it runs.
///
/// `program` is the image path core captured at startup (§7.6). Called from
/// a dedicated launcher thread: it blocks for at most a few system calls
/// plus [`UNWIND_REAP_BOUND`] on failure.
pub fn spawn_plugin_host(
    program: &Path,
    leading_args: &[&OsStr],
    host_to_plugin: &RingObjects,
    plugin_to_host: &RingObjects,
) -> Result<PluginHostProcess, SpawnError> {
    spawn_with(
        program,
        leading_args,
        [host_to_plugin, plugin_to_host],
        &mut NoHooks,
    )
}

/// Observation and fault points between the spawn steps. Production uses
/// [`NoHooks`]; native tests use them to inspect the suspended child and to
/// inject each failure the unwind must survive.
pub(in crate::plugin_process) trait SpawnHooks {
    /// Extra inheritable handles to add to the list. Only negative
    /// controls (a deliberately leaked job handle) return any.
    fn extra_inherited(&mut self, _job: &PluginJob) -> io::Result<Vec<OwnedHandle>> {
        Ok(Vec::new())
    }

    /// After `CreateProcessW`, before assignment.
    fn created(&mut self, _process: BorrowedHandle<'_>) -> io::Result<()> {
        Ok(())
    }

    /// Whether to assign the job at all. Only the assignment-disabled
    /// negative control answers `false`; it also skips verification.
    fn assign(&mut self) -> bool {
        true
    }

    /// After assignment and verification, before `ResumeThread`.
    fn before_resume(&mut self, _job: &PluginJob, _process: BorrowedHandle<'_>) -> io::Result<()> {
        Ok(())
    }
}

pub(in crate::plugin_process) struct NoHooks;

impl SpawnHooks for NoHooks {}

pub(in crate::plugin_process) fn spawn_with(
    program: &Path,
    leading_args: &[&OsStr],
    [host_to_plugin, plugin_to_host]: [&RingObjects; 2],
    hooks: &mut dyn SpawnHooks,
) -> Result<PluginHostProcess, SpawnError> {
    let fail = |step, source| SpawnError {
        step,
        source,
        unwound: None,
    };
    let job = PluginJob::create().map_err(|error| fail(SpawnStep::CreateJob, error))?;

    let created = {
        let prepare = |error| fail(SpawnStep::PrepareChild, error);
        // Inheritable duplicates; dropped (closed) right after creation.
        let mut inherited = vec![
            duplicate_inheritable(host_to_plugin.mapping_handle(), SECTION_ACCESS)
                .map_err(prepare)?,
            duplicate_inheritable(host_to_plugin.doorbell_handle(), EVENT_ACCESS)
                .map_err(prepare)?,
            duplicate_inheritable(plugin_to_host.mapping_handle(), SECTION_ACCESS)
                .map_err(prepare)?,
            duplicate_inheritable(plugin_to_host.doorbell_handle(), EVENT_ACCESS)
                .map_err(prepare)?,
        ];
        let values = EndpointValues {
            host_to_plugin: [raw_value(&inherited[0]), raw_value(&inherited[1])],
            plugin_to_host: [raw_value(&inherited[2]), raw_value(&inherited[3])],
        };
        inherited.extend(hooks.extra_inherited(&job).map_err(prepare)?);
        let application = wide_path(program).map_err(prepare)?;
        let mut command_line = command_line(program, leading_args, &values).map_err(prepare)?;
        let list: Vec<HANDLE> = inherited
            .iter()
            .map(|handle| handle.as_raw_handle() as HANDLE)
            .collect();
        let attributes = HandleListAttribute::new(&list).map_err(prepare)?;
        create_suspended(&application, &mut command_line, &attributes)
            .map_err(|error| fail(SpawnStep::CreateProcess, error))?
        // `attributes`, `list` and `inherited` drop here: the parent's
        // inheritable duplicates exist only around `CreateProcessW`.
    };

    let result = confine_and_resume(&job, &created, hooks);
    match result {
        Ok(()) => {
            let Suspended {
                process,
                thread,
                pid,
                ..
            } = created.disarm();
            drop(thread);
            Ok(PluginHostProcess { process, pid, job })
        }
        Err((step, source)) => {
            let unwound = created.unwind();
            Err(SpawnError {
                step,
                source,
                unwound: Some(unwound),
            })
        }
    }
}

fn confine_and_resume(
    job: &PluginJob,
    created: &Guard,
    hooks: &mut dyn SpawnHooks,
) -> Result<(), (SpawnStep, io::Error)> {
    let child = created.child();
    let process = child.process.as_handle();
    hooks
        .created(process)
        .map_err(|error| (SpawnStep::AssignJob, error))?;
    if hooks.assign() {
        job.assign(process)
            .map_err(|error| (SpawnStep::AssignJob, error))?;
        verify_membership(job, process, child.pid)
            .map_err(|error| (SpawnStep::VerifyJob, error))?;
    }
    hooks
        .before_resume(job, process)
        .map_err(|error| (SpawnStep::Resume, error))?;
    // SAFETY: the live initial-thread handle `CreateProcessW` returned.
    let previous = unsafe { ResumeThread(child.thread.as_raw_handle() as HANDLE) };
    if previous == u32::MAX {
        return Err((SpawnStep::Resume, io::Error::last_os_error()));
    }
    if previous != 1 {
        return Err((
            SpawnStep::Resume,
            io::Error::other(format!(
                "initial thread had suspend count {previous}, expected exactly 1"
            )),
        ));
    }
    Ok(())
}

/// Job-specific membership: this job's handle reports the process, and
/// this job's own list is exactly that one process.
fn verify_membership(job: &PluginJob, process: BorrowedHandle<'_>, pid: u32) -> io::Result<()> {
    if !job.contains(process)? {
        return Err(io::Error::other(
            "assigned process is not a member of its plugin-host job",
        ));
    }
    let ids = job.process_ids()?;
    if ids.assigned != 1 || ids.listed != [pid] {
        return Err(io::Error::other(format!(
            "plugin-host job lists {ids:?}, expected exactly process {pid}"
        )));
    }
    Ok(())
}

/// A created, still-suspended child.
#[derive(Debug)]
struct Suspended {
    process: OwnedHandle,
    thread: OwnedHandle,
    pid: u32,
}

/// Terminates and reaps the child on drop unless disarmed, so an early
/// return or a panic between creation and resume cannot leave a suspended
/// process behind.
struct Guard(Option<Suspended>);

impl Guard {
    fn child(&self) -> &Suspended {
        self.0.as_ref().expect("guard is armed until disarmed")
    }

    fn disarm(mut self) -> Suspended {
        self.0.take().expect("guard is armed until disarmed")
    }

    fn unwind(mut self) -> UnwoundChild {
        let child = self.0.take().expect("guard is armed until disarmed");
        unwind(child)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(child) = self.0.take() {
            unwind(child);
        }
    }
}

fn unwind(child: Suspended) -> UnwoundChild {
    // SAFETY: a live process handle with `PROCESS_TERMINATE` access. A
    // failure (the process already ended) is visible in the wait below.
    unsafe { TerminateProcess(child.process.as_raw_handle() as HANDLE, UNWIND_EXIT_CODE) };
    let exit_code = wait_exit(child.process.as_handle(), Some(UNWIND_REAP_BOUND))
        .ok()
        .flatten();
    // `child`'s process and thread handles close here.
    UnwoundChild {
        pid: child.pid,
        exit_code,
    }
}

fn create_suspended(
    application: &[u16],
    command_line: &mut [u16],
    attributes: &HandleListAttribute,
) -> io::Result<Guard> {
    // SAFETY: all-zero is a valid value of these plain C structs.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    // Standard handles are explicitly absent (null): the daemon's own are
    // never passed, and nothing beyond the four ring objects is inherited.
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.lpAttributeList = attributes.as_ptr();
    // SAFETY: all-zero is a valid value of this plain C struct.
    let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `application` and `command_line` are NUL-terminated UTF-16
    // buffers (the latter writable, as `CreateProcessW` requires);
    // `startup` is a STARTUPINFOEXW whose attribute list (a live, initialized
    // handle list over handles that outlive this call) matches
    // `EXTENDED_STARTUPINFO_PRESENT`; a null environment and directory
    // inherit the parent's; `information` is writable.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            CREATE_SUSPENDED | DETACHED_PROCESS | EXTENDED_STARTUPINFO_PRESENT,
            std::ptr::null(),
            std::ptr::null(),
            (&raw const startup).cast(),
            &mut information,
        )
    };
    if created == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success both handles are fresh and owned by the caller.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(information.hProcess.cast()),
            OwnedHandle::from_raw_handle(information.hThread.cast()),
        )
    };
    Ok(Guard(Some(Suspended {
        process,
        thread,
        pid: information.dwProcessId,
    })))
}

/// A `PROC_THREAD_ATTRIBUTE_LIST` holding one handle list. It points at
/// the caller's handle array, which must outlive it.
struct HandleListAttribute<'a> {
    buffer: Vec<u64>,
    _handles: std::marker::PhantomData<&'a [HANDLE]>,
}

impl<'a> HandleListAttribute<'a> {
    fn new(handles: &'a [HANDLE]) -> io::Result<Self> {
        let mut size = 0;
        // SAFETY: a null list with a writable size asks only for the size;
        // the expected `ERROR_INSUFFICIENT_BUFFER` failure is ignored and
        // `size` checked instead.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut list = Self {
            buffer: vec![0; size.div_ceil(size_of::<u64>())],
            _handles: std::marker::PhantomData,
        };
        // SAFETY: `buffer` is writable, 8-byte aligned and at least `size`
        // bytes, the size the system asked for one attribute.
        if unsafe { InitializeProcThreadAttributeList(list.as_ptr(), 1, 0, &mut size) } == 0 {
            // Not initialized: drop must not delete it.
            list.buffer = Vec::new();
            return Err(io::Error::last_os_error());
        }
        // SAFETY: an initialized one-attribute list; `handles` stays
        // borrowed (so alive and unmoved) for as long as the list exists.
        let updated = unsafe {
            UpdateProcThreadAttribute(
                list.as_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if updated == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(list)
    }

    fn as_ptr(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.buffer.as_ptr().cast_mut().cast()
    }
}

impl Drop for HandleListAttribute<'_> {
    fn drop(&mut self) {
        if !self.buffer.is_empty() {
            // SAFETY: the list was initialized and is deleted exactly once.
            unsafe { DeleteProcThreadAttributeList(self.as_ptr()) };
        }
    }
}

fn duplicate_inheritable(source: BorrowedHandle<'_>, access: u32) -> io::Result<OwnedHandle> {
    let mut target: HANDLE = std::ptr::null_mut();
    // SAFETY: duplicates a live handle within this process (pseudo handle
    // on both sides) into a new inheritable handle with `access`.
    let ok = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            source.as_raw_handle() as HANDLE,
            GetCurrentProcess(),
            &mut target,
            access,
            1,
            0,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a fresh handle this function now owns exclusively.
    Ok(unsafe { OwnedHandle::from_raw_handle(target.cast()) })
}

fn raw_value(handle: &OwnedHandle) -> usize {
    handle.as_raw_handle() as usize
}

pub(in crate::plugin_process) fn wait_exit(
    process: BorrowedHandle<'_>,
    timeout: Option<Duration>,
) -> io::Result<Option<u32>> {
    let millis = match timeout {
        None => INFINITE,
        Some(timeout) => u32::try_from(timeout.as_millis())
            .unwrap_or(INFINITE - 1)
            .min(INFINITE - 1),
    };
    // SAFETY: a live process handle with `SYNCHRONIZE` access.
    match unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, millis) } {
        WAIT_OBJECT_0 => {}
        WAIT_TIMEOUT => return Ok(None),
        WAIT_FAILED => return Err(io::Error::last_os_error()),
        other => {
            return Err(io::Error::other(format!(
                "unexpected process wait result {other:#x}"
            )));
        }
    }
    let mut code = 0;
    // SAFETY: a live, signaled process handle and a writable u32.
    if unsafe { GetExitCodeProcess(process.as_raw_handle() as HANDLE, &mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(code))
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program path contains NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

/// The NUL-terminated command line: the quoted program, `leading_args`,
/// then the endpoint argument, each quoted so the C runtime's (and Rust's)
/// argument parser recovers it exactly.
pub(in crate::plugin_process) fn command_line(
    program: &Path,
    leading_args: &[&OsStr],
    endpoints: &EndpointValues,
) -> io::Result<Vec<u16>> {
    let invalid = |what| io::Error::new(io::ErrorKind::InvalidInput, what);
    let mut line = Vec::new();
    // argv[0] is parsed without escapes: quote it whole, so it must not
    // contain a quote (Windows paths cannot).
    let program: Vec<u16> = program.as_os_str().encode_wide().collect();
    if program
        .iter()
        .any(|&unit| unit == 0 || unit == u16::from(b'"'))
    {
        return Err(invalid("program path contains NUL or a quote"));
    }
    line.push(u16::from(b'"'));
    line.extend_from_slice(&program);
    line.push(u16::from(b'"'));
    let endpoint = endpoints.to_arg();
    for arg in leading_args
        .iter()
        .copied()
        .chain(std::iter::once(OsStr::new(&endpoint)))
    {
        let arg: Vec<u16> = arg.encode_wide().collect();
        if arg.contains(&0) {
            return Err(invalid("argument contains NUL"));
        }
        line.push(u16::from(b' '));
        push_quoted(&mut line, &arg);
    }
    line.push(0);
    Ok(line)
}

/// Appends one argument quoted for `CommandLineToArgvW`/the C runtime:
/// backslashes are literal except before a quote, where they are doubled.
fn push_quoted(line: &mut Vec<u16>, arg: &[u16]) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    let plain = !arg.is_empty()
        && !arg
            .iter()
            .any(|&unit| unit == QUOTE || unit == u16::from(b' ') || unit == u16::from(b'\t'));
    if plain {
        line.extend_from_slice(arg);
        return;
    }
    line.push(QUOTE);
    let mut backslashes = 0;
    for &unit in arg {
        if unit == BACKSLASH {
            backslashes += 1;
            continue;
        }
        let escaped = if unit == QUOTE {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        line.extend(std::iter::repeat_n(BACKSLASH, escaped));
        line.push(unit);
        backslashes = 0;
    }
    line.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2));
    line.push(QUOTE);
}
