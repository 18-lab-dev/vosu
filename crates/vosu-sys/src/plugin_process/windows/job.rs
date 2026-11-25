//! The strict job object that confines one `vosu plugin-host` process and
//! its descendants (`docs/v2/plugin-abi.md` §7.4).
//!
//! A [`PluginJob`] has exactly the `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
//! limit: no breakaway limit of either kind, so neither the plugin host nor
//! anything it starts can leave the job. Its handle is created
//! non-inheritable and is never placed on a child's inherited-handle list,
//! so core's handle is the only one: closing it, including through an
//! unclean core exit, terminates every process in the job.
//!
//! Membership evidence is always specific to one job: [`PluginJob::contains`]
//! asks about *this* job's handle, and [`PluginJob::process_ids`] reads this
//! job's own process list. A generic `IsProcessInJob(process, NULL)` answer
//! only says the process is in *some* job (a scheduler, a test runner or a
//! terminal routinely supplies one) and is never used as evidence here.

use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};

use windows_sys::Win32::Foundation::{
    ERROR_MORE_DATA, GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOBOBJECTINFOCLASS, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

/// The exact limit flags of a plugin-host job: kill-on-close and nothing
/// else. In particular neither breakaway flag.
pub const PLUGIN_JOB_LIMIT_FLAGS: u32 = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

const _: () = assert!(
    PLUGIN_JOB_LIMIT_FLAGS & (JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK)
        == 0,
    "a plugin-host job never allows breakaway"
);

/// Most process IDs one job-list read returns. A plugin host is the only
/// process in its job at assignment; later reads are diagnostics, and a
/// longer list reports [`JobProcessIds::assigned`] with the first entries.
const LISTED_IDS: usize = 64;

/// One kill-on-close job for one plugin-host process. Dropping it closes
/// core's only handle, which terminates every process still in the job.
#[derive(Debug)]
pub struct PluginJob {
    handle: OwnedHandle,
}

impl PluginJob {
    /// Creates an unnamed, non-inheritable job with exactly
    /// [`PLUGIN_JOB_LIMIT_FLAGS`], and reads both properties back.
    pub fn create() -> io::Result<Self> {
        // SAFETY: default (non-inheritable) security and no name.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh job handle this function now owns exclusively.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        let job = Self { handle };
        // SAFETY: an all-zero JOBOBJECT_EXTENDED_LIMIT_INFORMATION is a valid
        // "no limits" value of this plain C struct.
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = PLUGIN_JOB_LIMIT_FLAGS;
        // SAFETY: `limits` is a live, correctly sized structure of the class
        // named, read only for the duration of the call.
        let set = unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = job.limit_flags()?;
        if flags != PLUGIN_JOB_LIMIT_FLAGS {
            return Err(io::Error::other(format!(
                "job limit flags read back as {flags:#x}, expected exactly {PLUGIN_JOB_LIMIT_FLAGS:#x}"
            )));
        }
        if is_inheritable(job.handle.as_handle())? {
            return Err(io::Error::other("job handle is inheritable"));
        }
        Ok(job)
    }

    /// Assigns `process` to this job. A nested assignment (the process is
    /// already in its parent's job) is expected; only this call's own
    /// result, then [`Self::contains`], count.
    pub fn assign(&self, process: BorrowedHandle<'_>) -> io::Result<()> {
        // SAFETY: both are live handles; `process` has the
        // `PROCESS_SET_QUOTA | PROCESS_TERMINATE` access assignment needs.
        if unsafe { AssignProcessToJobObject(self.raw(), process.as_raw_handle() as HANDLE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Whether `process` is in *this* job (not merely in some job).
    pub fn contains(&self, process: BorrowedHandle<'_>) -> io::Result<bool> {
        let mut result = 0;
        // SAFETY: both are live handles and `result` is a writable BOOL.
        if unsafe { IsProcessInJob(process.as_raw_handle() as HANDLE, self.raw(), &mut result) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(result != 0)
    }

    /// This job's own list of member process IDs.
    pub fn process_ids(&self) -> io::Result<JobProcessIds> {
        query_process_ids(self.raw())
    }

    /// This job's limit flags, read back from the job.
    pub fn limit_flags(&self) -> io::Result<u32> {
        query_limit_flags(self.raw())
    }

    /// Terminates every process in the job with `exit_code`.
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: a live job handle with `JOB_OBJECT_TERMINATE` access.
        if unsafe { TerminateJobObject(self.raw(), exit_code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The job handle, for independent identity checks. It must never be
    /// duplicated into, or inherited by, a plugin host.
    pub fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }
}

/// The member process IDs a job reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobProcessIds {
    /// Every process currently in the job (nested jobs included).
    pub assigned: u32,
    /// The IDs returned, at most [`LISTED_IDS`] of them.
    pub listed: Vec<u32>,
}

/// What the calling process can read about its own immediate job, without
/// holding any job handle: the child-side confinement check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentJob {
    pub limit_flags: u32,
    pub process_ids: JobProcessIds,
}

impl CurrentJob {
    /// Whether the immediate job has exactly the plugin-host limits:
    /// kill-on-close and neither breakaway flag.
    pub fn is_plugin_job(&self) -> bool {
        self.limit_flags == PLUGIN_JOB_LIMIT_FLAGS
    }
}

/// Reads the calling process's immediate job (the innermost one when jobs
/// are nested). `None` when the process is in no job at all.
pub fn current_job() -> io::Result<Option<CurrentJob>> {
    let mut in_job = 0;
    // SAFETY: the current-process pseudo handle, a null job meaning "any
    // job" (used only to decide whether a job exists to query), and a
    // writable BOOL.
    if unsafe { IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut in_job) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if in_job == 0 {
        return Ok(None);
    }
    // A null job handle names the calling process's immediate job.
    let job = std::ptr::null_mut();
    Ok(Some(CurrentJob {
        limit_flags: query_limit_flags(job)?,
        process_ids: query_process_ids(job)?,
    }))
}

/// Whether `handle` would be inherited by a child created with handle
/// inheritance.
pub(super) fn is_inheritable(handle: BorrowedHandle<'_>) -> io::Result<bool> {
    let mut flags = 0;
    // SAFETY: a live handle and a writable u32.
    if unsafe { GetHandleInformation(handle.as_raw_handle() as HANDLE, &mut flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags & HANDLE_FLAG_INHERIT != 0)
}

fn query_limit_flags(job: HANDLE) -> io::Result<u32> {
    // SAFETY: all-zero is a valid value of this plain C struct.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    query(
        job,
        JobObjectExtendedLimitInformation,
        (&raw mut limits).cast(),
        size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>(),
    )?;
    Ok(limits.BasicLimitInformation.LimitFlags)
}

/// `JOBOBJECT_BASIC_PROCESS_ID_LIST` with room for [`LISTED_IDS`] entries:
/// the same layout as the SDK struct's one-element trailing array.
#[repr(C)]
struct IdList {
    assigned: u32,
    listed: u32,
    ids: [usize; LISTED_IDS],
}

fn query_process_ids(job: HANDLE) -> io::Result<JobProcessIds> {
    let mut list = IdList {
        assigned: 0,
        listed: 0,
        ids: [0; LISTED_IDS],
    };
    match query(
        job,
        JobObjectBasicProcessIdList,
        (&raw mut list).cast(),
        size_of::<IdList>(),
    ) {
        Ok(()) => {}
        // A longer list still fills the counts and the entries that fit.
        Err(error) if error.raw_os_error() == Some(ERROR_MORE_DATA as i32) => {}
        Err(error) => return Err(error),
    }
    let listed = (list.listed as usize).min(LISTED_IDS);
    Ok(JobProcessIds {
        assigned: list.assigned,
        listed: list.ids[..listed].iter().map(|&id| id as u32).collect(),
    })
}

fn query(
    job: HANDLE,
    class: JOBOBJECTINFOCLASS,
    buffer: *mut core::ffi::c_void,
    len: usize,
) -> io::Result<()> {
    // SAFETY: `buffer` is a live, writable structure of `len` bytes of the
    // layout `class` names; `job` is a live job handle or null (the calling
    // process's immediate job).
    let ok =
        unsafe { QueryInformationJobObject(job, class, buffer, len as u32, std::ptr::null_mut()) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
