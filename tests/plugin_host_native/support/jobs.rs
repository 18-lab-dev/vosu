//! Windows readbacks for the core-death trees (M5 N0), taken by the test
//! itself, never through the code under test: a process's immediate job,
//! token identity, and the object types in another process's handle table.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetLengthSid, GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation,
    TOKEN_ELEVATION, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TokenElevation,
    TokenIntegrityLevel, TokenUser,
};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
pub const KILL_ON_JOB_CLOSE: u32 = 0x2000;
/// `JOB_OBJECT_LIMIT_BREAKAWAY_OK | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`.
pub const BREAKAWAY: u32 = 0x0800 | 0x1000;

/// The calling process's immediate job.
pub struct Job {
    pub limit_flags: u32,
    /// The listed process IDs, comma-separated in job order.
    pub pids: String,
}

/// `JOBOBJECT_BASIC_PROCESS_ID_LIST` with room for 32 IDs.
#[repr(C)]
struct IdList {
    assigned: u32,
    listed: u32,
    ids: [usize; 32],
}

/// The calling process's immediate job (a null job handle names it).
/// Panics outside any job.
pub fn current_job() -> Job {
    // SAFETY: all-zero is a valid value of this plain C struct.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a null job names this process's immediate job; the buffer is
    // writable for exactly the length passed.
    let ok = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectExtendedLimitInformation,
            (&raw mut limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(ok, 0, "not in a job: {}", std::io::Error::last_os_error());
    let mut list = IdList {
        assigned: 0,
        listed: 0,
        ids: [0; 32],
    };
    // SAFETY: as above, for the process-ID list.
    let ok = unsafe {
        QueryInformationJobObject(
            std::ptr::null_mut(),
            JobObjectBasicProcessIdList,
            (&raw mut list).cast(),
            size_of::<IdList>() as u32,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
    let pids = list.ids[..(list.listed as usize).min(list.ids.len())]
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",");
    Job {
        limit_flags: limits.BasicLimitInformation.LimitFlags,
        pids,
    }
}

/// A process token's identity: user SID bytes, mandatory integrity RID
/// and whether it is elevated.
#[derive(Debug, PartialEq, Eq)]
pub struct TokenIdentity {
    pub user_sid: Vec<u8>,
    pub integrity_rid: u32,
    pub elevated: bool,
}

fn token_information(token: HANDLE, class: i32) -> Vec<u64> {
    let mut needed = 0;
    // SAFETY: a size query with no buffer.
    unsafe { GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut needed) };
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(8).max(1)];
    // SAFETY: a live token with query access and an 8-aligned writable
    // buffer of at least `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    };
    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
    buffer
}

/// Reads `process`'s token identity (`PROCESS_QUERY_LIMITED_INFORMATION`).
pub fn token_identity(process: HANDLE) -> TokenIdentity {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: a live process handle and a writable handle slot.
    let ok = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
    assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
    let user = token_information(token, TokenUser);
    let label = token_information(token, TokenIntegrityLevel);
    let elevation = token_information(token, TokenElevation);
    // SAFETY: each buffer holds the structure its class returns; the SIDs
    // they point to live inside the same buffers.
    let identity = unsafe {
        let user = &*user.as_ptr().cast::<TOKEN_USER>();
        let length = GetLengthSid(user.User.Sid) as usize;
        let user_sid = std::slice::from_raw_parts(user.User.Sid.cast::<u8>(), length).to_vec();
        let label = &*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let count = *GetSidSubAuthorityCount(label.Label.Sid);
        let integrity_rid = *GetSidSubAuthority(label.Label.Sid, u32::from(count) - 1);
        let elevation = &*elevation.as_ptr().cast::<TOKEN_ELEVATION>();
        TokenIdentity {
            user_sid,
            integrity_rid,
            elevated: elevation.TokenIsElevated != 0,
        }
    };
    // SAFETY: closes the token opened above.
    unsafe { CloseHandle(token) };
    identity
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: HANDLE,
        class: i32,
        information: *mut core::ffi::c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

/// `ProcessHandleInformation`.
const PROCESS_HANDLE_INFORMATION: i32 = 51;

/// `PROCESS_HANDLE_TABLE_ENTRY_INFO`.
#[repr(C)]
#[derive(Clone, Copy)]
struct TableEntry {
    value: HANDLE,
    _handle_count: usize,
    _pointer_count: usize,
    _access: u32,
    type_index: u32,
    _attributes: u32,
    _reserved: u32,
}

/// Every `(handle value, object type index)` in `process`'s handle table
/// (`PROCESS_QUERY_INFORMATION`).
pub fn handle_types(process: HANDLE) -> Vec<(usize, u32)> {
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    let header = 2 * size_of::<usize>();
    let mut words = 4096;
    loop {
        let mut buffer = vec![0_u64; words];
        let mut needed = 0;
        // SAFETY: a live process handle with query access and a writable,
        // 8-byte aligned buffer of the stated length.
        let status = unsafe {
            NtQueryInformationProcess(
                process,
                PROCESS_HANDLE_INFORMATION,
                buffer.as_mut_ptr().cast(),
                (words * 8) as u32,
                &mut needed,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            words = (needed as usize).div_ceil(8) + 512;
            continue;
        }
        assert!(status >= 0, "NtQueryInformationProcess: {status:#x}");
        let base = buffer.as_ptr().cast::<u8>();
        // SAFETY: the snapshot starts with its handle count.
        let count = unsafe { base.cast::<usize>().read() };
        assert!(header + count * size_of::<TableEntry>() <= words * 8);
        return (0..count)
            .map(|index| {
                // SAFETY: entry `index` lies inside the filled buffer
                // (asserted above); read by value, unaligned-safe.
                let entry = unsafe {
                    base.add(header + index * size_of::<TableEntry>())
                        .cast::<TableEntry>()
                        .read_unaligned()
                };
                (entry.value as usize, entry.type_index)
            })
            .collect();
    }
}

/// The object type index of job objects on this system, read from a job
/// this process creates for the purpose.
pub fn job_type_index() -> u32 {
    // SAFETY: an unnamed job with default security.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    assert!(!job.is_null(), "{}", std::io::Error::last_os_error());
    // SAFETY: the current-process pseudo handle is always valid.
    let own = handle_types(unsafe { GetCurrentProcess() });
    // SAFETY: closes the job created above.
    unsafe { CloseHandle(job) };
    own.into_iter()
        .find(|(value, _)| *value == job as usize)
        .map(|(_, index)| index)
        .expect("this process's own job handle is in its table")
}
