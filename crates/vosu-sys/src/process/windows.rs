//! Native Windows process access.

use std::{
    ffi::{CStr, c_void},
    fmt,
    mem::{offset_of, size_of},
    sync::OnceLock,
};

use windows_sys::{
    Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation},
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ACCESS_DENIED, ERROR_BAD_LENGTH, ERROR_NO_MORE_FILES,
            ERROR_PARTIAL_COPY, FILETIME, GetLastError, HANDLE, INVALID_HANDLE_VALUE, STILL_ACTIVE,
        },
        Security::{EqualSid, GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
        System::{
            Diagnostics::{Debug::ReadProcessMemory, ToolHelp::*},
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
            Memory::{
                MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_READWRITE,
                VirtualQueryEx,
            },
            SystemInformation::{IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386},
            Threading::{
                GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, IsWow64Process2,
                OpenProcess, OpenProcessToken, PROCESS_QUERY_INFORMATION,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ, QueryFullProcessImageNameW,
            },
            WindowsProgramming::SYSTEM_PROCESS_INFORMATION,
        },
        UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    },
};

use vosu_plugin_api::memory::{
    MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest,
};
mod memory;
mod read_impl;
use read_impl::{read_exact_impl, read_many_impl};
mod launch_context;
pub(super) use launch_context::launch_context;

#[derive(Debug)]
pub struct OwnedHandle {
    handle: HANDLE,
    close: unsafe extern "system" fn(HANDLE) -> i32,
}

// SAFETY: `HANDLE` is an opaque kernel object reference; Windows documents
// handles as safe to use from any thread, and `close` is a plain function
// pointer with no thread-affine state.
unsafe impl Send for OwnedHandle {}
// SAFETY: see the `Send` impl above; nothing in `OwnedHandle` is accessed
// through `&self` in a way that requires exclusive access.
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    fn new(handle: HANDLE, operation: &'static str) -> Result<Self, MemoryError> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(last_error(operation));
        }
        Ok(Self {
            handle,
            close: CloseHandle,
        })
    }

    #[cfg(test)]
    fn with_closer(handle: HANDLE, close: unsafe extern "system" fn(HANDLE) -> i32) -> Self {
        assert!(!handle.is_null() && handle != INVALID_HANDLE_VALUE);
        Self { handle, close }
    }

    #[inline]
    fn raw(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this type uniquely owns a valid, closeable kernel handle.
        unsafe {
            (self.close)(self.handle);
        }
    }
}

struct Snapshot(OwnedHandle);

impl Snapshot {
    fn create(flags: u32, pid: u32) -> Result<Self, MemoryError> {
        let handle = snapshot_with_retry(
            flags,
            || {
                // SAFETY: no pointers are passed; capture the error before yielding.
                let handle = unsafe { CreateToolhelp32Snapshot(flags, pid) };
                if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                    // SAFETY: `GetLastError` has no preconditions.
                    Err(unsafe { GetLastError() })
                } else {
                    Ok(handle)
                }
            },
            std::thread::yield_now,
        )
        .map_err(|code| backend("CreateToolhelp32Snapshot", &format!("Windows error {code}")))?;
        OwnedHandle::new(handle, "CreateToolhelp32Snapshot").map(Self)
    }
}

// Module lists can change while Toolhelp copies them. Bound retries so an
// inaccessible or continuously changing target cannot stall attachment forever.
fn snapshot_with_retry<T>(
    flags: u32,
    mut call: impl FnMut() -> Result<T, u32>,
    mut yield_now: impl FnMut(),
) -> Result<T, u32> {
    const ATTEMPTS: usize = 8;
    for attempt in 0..ATTEMPTS {
        match call() {
            Err(code)
                if flags & (TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32) != 0
                    && matches!(code, ERROR_BAD_LENGTH | ERROR_PARTIAL_COPY)
                    && attempt + 1 < ATTEMPTS =>
            {
                yield_now();
            }
            result => return result,
        }
    }
    unreachable!("the final attempt always returns")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent_pid: u32,
    pub thread_count: u32,
    pub base_priority: i32,
    pub executable: String,
    /// This process occurrence's own start time (`FILETIME`-shaped: 100 ns
    /// ticks since 1601-01-01), read directly from
    /// `SYSTEM_PROCESS_INFORMATION`'s own `CreateTime` field without
    /// opening the process (`system_process_create_time`'s own doc
    /// comment) — the preferred source for `ProcessRecord::start_time`
    /// (P1 pid-reuse identity); `WindowsProcessMemory::creation_time`
    /// (`GetProcessTimes`, requires an open handle) is the fallback when
    /// a caller already has one open for another reason.
    pub create_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadEntry {
    pub tid: u32,
    pub owner_pid: u32,
    pub base_priority: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleEntry {
    pub process_id: u32,
    pub base_address: usize,
    pub size: u32,
    pub name: String,
    pub path: String,
}

const INITIAL_PROCESS_BUFFER_SIZE: usize = 64 * 1024;
const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;

pub fn enumerate_processes() -> Result<Vec<ProcessEntry>, MemoryError> {
    enumerate_processes_matching(|_| true)
}

pub fn enumerate_processes_matching(
    predicate: impl FnMut(&[u16]) -> bool,
) -> Result<Vec<ProcessEntry>, MemoryError> {
    enumerate_processes_with_buffer(predicate)
}

fn enumerate_processes_with_buffer(
    mut predicate: impl FnMut(&[u16]) -> bool,
) -> Result<Vec<ProcessEntry>, MemoryError> {
    let mut byte_capacity = INITIAL_PROCESS_BUFFER_SIZE;

    loop {
        let buffer_size = u32::try_from(byte_capacity)
            .map_err(|_| backend("NtQuerySystemInformation", "process list exceeds 4 GiB"))?;
        let mut storage = vec![0usize; byte_capacity.div_ceil(size_of::<usize>())];
        let mut returned = 0u32;
        // SAFETY: storage is writable for buffer_size bytes and returned is a valid output.
        let status = unsafe {
            NtQuerySystemInformation(
                SystemProcessInformation,
                storage.as_mut_ptr().cast(),
                buffer_size,
                &mut returned,
            )
        };

        if status == STATUS_INFO_LENGTH_MISMATCH {
            byte_capacity = (returned as usize).max(
                byte_capacity
                    .checked_mul(2)
                    .ok_or_else(|| backend("NtQuerySystemInformation", "buffer size overflow"))?,
            );
            continue;
        }
        nt_success(status, "NtQuerySystemInformation(SystemProcessInformation)")?;

        let byte_len = returned as usize;
        if byte_len == 0 || byte_len > storage.len() * size_of::<usize>() {
            return Err(backend(
                "NtQuerySystemInformation",
                "invalid process list length",
            ));
        }
        // SAFETY: storage remains alive and initialized; byte_len was validated above.
        let bytes = unsafe { std::slice::from_raw_parts(storage.as_ptr().cast::<u8>(), byte_len) };
        return decode_system_processes_matching(bytes, &mut predicate);
    }
}

fn decode_system_processes_matching(
    bytes: &[u8],
    mut predicate: impl FnMut(&[u16]) -> bool,
) -> Result<Vec<ProcessEntry>, MemoryError> {
    let mut entries = Vec::new();
    let mut offset = 0usize;

    loop {
        let remaining = bytes
            .len()
            .checked_sub(offset)
            .ok_or_else(|| backend("SystemProcessInformation", "entry offset overflow"))?;
        if remaining < size_of::<SYSTEM_PROCESS_INFORMATION>() {
            return Err(backend(
                "SystemProcessInformation",
                "truncated process entry",
            ));
        }
        let address = bytes.as_ptr() as usize + offset;
        if !address.is_multiple_of(std::mem::align_of::<SYSTEM_PROCESS_INFORMATION>()) {
            return Err(backend(
                "SystemProcessInformation",
                "misaligned process entry",
            ));
        }
        // SAFETY: size and alignment were checked; the system owns the record layout.
        let raw = unsafe {
            &*bytes
                .as_ptr()
                .add(offset)
                .cast::<SYSTEM_PROCESS_INFORMATION>()
        };
        let name = system_process_name_units(raw, bytes)?;
        if predicate(name) {
            let pid = u32::try_from(raw.UniqueProcessId as usize)
                .map_err(|_| backend("SystemProcessInformation", "process id exceeds u32"))?;
            let parent_pid = u32::try_from(raw.Reserved2 as usize).map_err(|_| {
                backend("SystemProcessInformation", "parent process id exceeds u32")
            })?;
            entries.push(ProcessEntry {
                pid,
                parent_pid,
                thread_count: raw.NumberOfThreads,
                base_priority: raw.BasePriority,
                executable: String::from_utf16_lossy(name),
                create_time: system_process_create_time(raw),
            });
        }

        let next = raw.NextEntryOffset as usize;
        if next == 0 {
            break;
        }
        if next < size_of::<SYSTEM_PROCESS_INFORMATION>() || next > remaining {
            return Err(backend(
                "SystemProcessInformation",
                "invalid next process offset",
            ));
        }
        offset = offset
            .checked_add(next)
            .ok_or_else(|| backend("SystemProcessInformation", "entry offset overflow"))?;
    }

    entries.sort_unstable_by_key(|entry| entry.pid);
    Ok(entries)
}

fn system_process_name_units<'a>(
    process: &SYSTEM_PROCESS_INFORMATION,
    bytes: &'a [u8],
) -> Result<&'a [u16], MemoryError> {
    let length = process.ImageName.Length as usize;
    if length == 0 {
        return Ok(&[]);
    }
    if !length.is_multiple_of(size_of::<u16>()) || process.ImageName.Buffer.is_null() {
        return Err(backend(
            "SystemProcessInformation",
            "invalid process image name",
        ));
    }

    let buffer_start = bytes.as_ptr() as usize;
    let name_start = process.ImageName.Buffer as usize;
    let relative = name_start
        .checked_sub(buffer_start)
        .ok_or_else(|| backend("SystemProcessInformation", "image name precedes buffer"))?;
    let name_end = relative
        .checked_add(length)
        .ok_or_else(|| backend("SystemProcessInformation", "image name length overflow"))?;
    if name_end > bytes.len() || !name_start.is_multiple_of(std::mem::align_of::<u16>()) {
        return Err(backend(
            "SystemProcessInformation",
            "image name lies outside process buffer",
        ));
    }

    // SAFETY: bounds, byte length, and u16 alignment were checked above.
    let units =
        unsafe { std::slice::from_raw_parts(process.ImageName.Buffer, length / size_of::<u16>()) };
    Ok(units)
}

/// `SYSTEM_PROCESS_INFORMATION.CreateTime` (`FILETIME`-shaped: 100 ns
/// ticks since 1601-01-01), read directly out of the raw record without
/// opening the process — the undocumented `winternl.h`/`phnt` layout
/// `windows-sys` does not expose by name, folding it into the opaque
/// `Reserved1` byte array along with `WorkingSetPrivateSize`,
/// `HardFaultCount`, `NumberOfThreadsHighWatermark` and `CycleTime`
/// before it, and `UserTime`/`KernelTime` after it: struct layout (every
/// field before `ImageName`, byte offset from the struct's own start)
/// `WorkingSetPrivateSize`(8, LARGE_INTEGER) `HardFaultCount`(4)
/// `NumberOfThreadsHighWatermark`(4) `CycleTime`(8, ULONGLONG)
/// `CreateTime`(8, LARGE_INTEGER, at offset 32) `UserTime`(8)
/// `KernelTime`(8) -- summing to `Reserved1`'s own declared 48 bytes
/// before `ImageName`, matching this module's existing reliance on the
/// same layout for `Reserved2` (`InheritedFromUniqueProcessId`, the
/// parent pid, above).
fn system_process_create_time(process: &SYSTEM_PROCESS_INFORMATION) -> u64 {
    const CREATE_TIME_OFFSET: usize = 24; // within Reserved1, after an 8+4+4+8-byte header
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&process.Reserved1[CREATE_TIME_OFFSET..CREATE_TIME_OFFSET + 8]);
    u64::from_le_bytes(bytes)
}

pub fn enumerate_threads(pid: Option<u32>) -> Result<Vec<ThreadEntry>, MemoryError> {
    let snapshot = Snapshot::create(TH32CS_SNAPTHREAD, 0)?;
    // SAFETY: `THREADENTRY32` is a C struct of plain integer fields; zero is
    // a valid bit pattern that `dwSize` below overwrites before use.
    let mut raw: THREADENTRY32 = unsafe { std::mem::zeroed() };
    raw.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut entries = Vec::new();
    // SAFETY: `snapshot` holds a valid toolhelp snapshot handle and `raw`
    // was just sized via `dwSize` above.
    let mut ok = unsafe { Thread32First(snapshot.0.raw(), &mut raw) };
    while ok != 0 {
        if pid.is_none_or(|owner| owner == raw.th32OwnerProcessID) {
            entries.push(ThreadEntry {
                tid: raw.th32ThreadID,
                owner_pid: raw.th32OwnerProcessID,
                base_priority: raw.tpBasePri,
            });
        }
        // SAFETY: same preconditions as `Thread32First` above; `raw` is
        // reused in place across iterations as the API requires.
        ok = unsafe { Thread32Next(snapshot.0.raw(), &mut raw) };
    }
    finish_enumeration("Thread32Next")?;
    Ok(entries)
}

pub fn enumerate_modules(pid: u32) -> Result<Vec<ModuleEntry>, MemoryError> {
    let snapshot = Snapshot::create(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)?;
    // SAFETY: `MODULEENTRY32W` is a C struct of plain integer and array
    // fields; zero is a valid bit pattern that `dwSize` below overwrites
    // before use.
    let mut raw: MODULEENTRY32W = unsafe { std::mem::zeroed() };
    raw.dwSize = size_of::<MODULEENTRY32W>() as u32;
    let mut entries = Vec::new();
    // SAFETY: `snapshot` holds a valid toolhelp snapshot handle and `raw`
    // was just sized via `dwSize` above.
    let mut ok = unsafe { Module32FirstW(snapshot.0.raw(), &mut raw) };
    while ok != 0 {
        entries.push(ModuleEntry {
            process_id: raw.th32ProcessID,
            base_address: raw.modBaseAddr as usize,
            size: raw.modBaseSize,
            name: wide_array(&raw.szModule),
            path: wide_array(&raw.szExePath),
        });
        // SAFETY: same preconditions as `Module32FirstW` above; `raw` is
        // reused in place across iterations as the API requires.
        ok = unsafe { Module32NextW(snapshot.0.raw(), &mut raw) };
    }
    finish_enumeration("Module32NextW")?;
    Ok(entries)
}

pub fn process_bitness(pid: u32) -> Result<ProcessBitness, MemoryError> {
    WindowsProcessMemory::open(pid)?.bitness()
}

pub fn process_command_line(pid: u32) -> Result<String, MemoryError> {
    WindowsProcessMemory::open(pid)?.command_line()
}

pub fn foreground_process_id() -> Option<u32> {
    // SAFETY: these APIs require no caller-owned buffers beyond the valid local pid pointer.
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(window, &mut pid);
        (pid != 0).then_some(pid)
    }
}

/// Whether `pid` currently names a live process, probed with the
/// weakest possible access right (`PROCESS_QUERY_LIMITED_INFORMATION`)
/// so a denied [`WindowsProcessMemory::open`] can be told apart from a
/// genuinely dead `pid` (`docs/v2/plugin-abi.md` §5: only a dead process
/// is `VOSU_ERR_TARGET_GONE`; an OS failure to open a live one is
/// `VOSU_ERR_PERMISSION_DENIED`/`VOSU_ERR_BACKEND`). Even this weakest
/// right is denied by the kernel for a Protected-Process-Light target
/// (`csrss.exe`, `smss.exe`, `wininit.exe`, `services.exe`, `lsass.exe`,
/// `Registry`, `Memory Compression`, `Secure System`, a protected
/// antimalware service): `OpenProcess` there fails with
/// `ERROR_ACCESS_DENIED`, an error only a *live* process can produce (a
/// dead or never-existing `pid` fails with `ERROR_INVALID_PARAMETER`
/// instead), so that specific error is still reported as alive; every
/// other `OpenProcess` failure is reported as dead.
#[must_use]
pub fn is_process_alive(pid: u32) -> bool {
    // SAFETY: `OpenProcess` returns an owned handle on success; no
    // caller-owned buffers are passed.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        // SAFETY: `GetLastError` has no memory-safety preconditions;
        // reading it immediately after the failed call above is required
        // for it to reflect that call's own error, not some later one.
        let error = unsafe { GetLastError() };
        return error == ERROR_ACCESS_DENIED;
    }
    let mut exit_code = 0u32;
    // SAFETY: `handle` was just opened above and is closed below;
    // `exit_code` is a valid writable local.
    let alive = unsafe {
        GetExitCodeProcess(handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE as u32
    };
    // SAFETY: `handle` is a valid, owned kernel handle not used again.
    unsafe {
        CloseHandle(handle);
    }
    alive
}

/// The full executable image path for `pid`
/// (`QueryFullProcessImageNameW`, `PROCESS_NAME_WIN32` format), opened
/// with the same weakest-possible access right [`is_process_alive`] uses
/// (`PROCESS_QUERY_LIMITED_INFORMATION`) so an elevated process this
/// host cannot `PROCESS_VM_READ` still yields a path. A
/// Protected-Process-Light target denies even this right
/// ([`is_process_alive`]'s own doc comment); callers fall back to the
/// process list's bare name instead (`windows_source.rs`'s own
/// `build_record`) when this fails. The preferred source for
/// `process_path_patterns` matching (§6.1): unlike a raw command line's
/// argv[0], this is never relative, never bare, and never whatever an
/// unrelated `lpApplicationName` happened to be.
pub fn process_image_path(pid: u32) -> Result<String, MemoryError> {
    // SAFETY: `OpenProcess` returns an owned handle on success; no
    // caller-owned buffers are passed.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    let handle = OwnedHandle::new(handle, "OpenProcess")?;
    let mut buffer = vec![0u16; 32_768];
    let mut size = u32::try_from(buffer.len())
        .map_err(|_| backend("QueryFullProcessImageNameW", "buffer size exceeds u32"))?;
    // SAFETY: `handle` is a valid open handle; `buffer` is writable for
    // `size` UTF-16 units and `size` is updated in place on success.
    let ok = unsafe { QueryFullProcessImageNameW(handle.raw(), 0, buffer.as_mut_ptr(), &mut size) };
    if ok == 0 {
        return Err(last_error("QueryFullProcessImageNameW"));
    }
    Ok(String::from_utf16_lossy(&buffer[..size as usize]))
}

fn same_process_user(process: HANDLE) -> Result<bool, MemoryError> {
    fn user_token(process: HANDLE) -> Result<OwnedHandle, MemoryError> {
        let mut token = std::ptr::null_mut();
        // SAFETY: process is an open handle (or current-process pseudohandle);
        // token is a valid output pointer and ownership transfers on success.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
            return Err(last_error("OpenProcessToken"));
        }
        OwnedHandle::new(token, "OpenProcessToken")
    }
    fn read_user(token: &OwnedHandle, buffer: &mut [usize; 32]) -> Result<(), MemoryError> {
        let mut length = 0;
        // TOKEN_USER plus the largest Windows SID fits this aligned fixed buffer.
        // SAFETY: the owned token and the entire writable buffer remain live.
        if unsafe {
            GetTokenInformation(
                token.raw(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size_of::<[usize; 32]>() as u32,
                &mut length,
            )
        } == 0
        {
            return Err(last_error("GetTokenInformation(TokenUser)"));
        }
        if length < size_of::<TOKEN_USER>() as u32 {
            return Err(backend("TokenUser", "truncated token user"));
        }
        Ok(())
    }
    let target = user_token(process)?;
    // SAFETY: GetCurrentProcess returns a non-owning pseudohandle.
    let current = user_token(unsafe { GetCurrentProcess() })?;
    let mut target_user = [0usize; 32];
    let mut current_user = [0usize; 32];
    read_user(&target, &mut target_user)?;
    read_user(&current, &mut current_user)?;
    // SAFETY: GetTokenInformation initialized both aligned TOKEN_USER records
    // and their SIDs; buffers stay in place and live through EqualSid.
    Ok(unsafe {
        let target = &*target_user.as_ptr().cast::<TOKEN_USER>();
        let current = &*current_user.as_ptr().cast::<TOKEN_USER>();
        EqualSid(target.User.Sid, current.User.Sid) != 0
    })
}

#[derive(Debug)]
pub struct WindowsProcessMemory {
    pid: u32,
    handle: OwnedHandle,
}

impl WindowsProcessMemory {
    pub fn open(pid: u32) -> Result<Self, MemoryError> {
        // SAFETY: OpenProcess returns an owned handle and inherit is disabled.
        let handle = unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid) };
        let handle = OwnedHandle::new(handle, "OpenProcess")?;
        if !same_process_user(handle.raw())? {
            return Err(backend(
                "OpenProcess",
                "process is owned by another Windows account",
            ));
        }
        Ok(Self { pid, handle })
    }

    pub fn bitness(&self) -> Result<ProcessBitness, MemoryError> {
        let mut process_machine = 0u16;
        let mut native_machine = 0u16;
        // SAFETY: both output pointers refer to initialized local storage.
        if unsafe { IsWow64Process2(self.handle.raw(), &mut process_machine, &mut native_machine) }
            == 0
        {
            return Err(last_error("IsWow64Process2"));
        }
        match (process_machine, native_machine) {
            (IMAGE_FILE_MACHINE_I386, _) => Ok(ProcessBitness::X86),
            (0, IMAGE_FILE_MACHINE_AMD64) | (IMAGE_FILE_MACHINE_AMD64, _) => {
                Ok(ProcessBitness::X64)
            }
            (0, IMAGE_FILE_MACHINE_I386) => Ok(ProcessBitness::X86),
            _ => Err(backend("IsWow64Process2", "unsupported target machine")),
        }
    }

    pub fn creation_time(&self) -> Result<u64, MemoryError> {
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = creation;
        let mut kernel = creation;
        let mut user = creation;
        // SAFETY: all output pointers refer to initialized local FILETIME values.
        if unsafe {
            GetProcessTimes(
                self.handle.raw(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return Err(last_error("GetProcessTimes"));
        }
        Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
    }

    pub fn command_line(&self) -> Result<String, MemoryError> {
        self.command_line_for_bitness(self.bitness()?)
    }

    pub fn command_line_for_bitness(&self, bitness: ProcessBitness) -> Result<String, MemoryError> {
        let ntdll = Ntdll::load()?;
        let descriptor = match bitness {
            ProcessBitness::X64 => {
                // SAFETY: `ProcessBasicInformation64` is a C struct of plain
                // integer/pointer-sized fields; zero is a valid bit pattern
                // that the `NtQueryInformationProcess` call below overwrites.
                let mut pbi: ProcessBasicInformation64 = unsafe { std::mem::zeroed() };
                let mut returned = 0;
                // SAFETY: function signature matches ntdll and pbi is a correctly sized output buffer.
                let status = unsafe {
                    (ntdll.query_process)(
                        self.handle.raw(),
                        0,
                        (&mut pbi as *mut ProcessBasicInformation64).cast(),
                        size_of::<ProcessBasicInformation64>() as u32,
                        &mut returned,
                    )
                };
                nt_success(status, "NtQueryInformationProcess")?;
                let peb = pbi.peb_base_address as usize;
                let params = self.read_remote_pointer(peb + PEB64_PROCESS_PARAMETERS, 8)?;
                self.read_unicode_descriptor(params + RTL64_COMMAND_LINE, false)?
            }
            ProcessBitness::X86 => {
                let mut peb = 0usize;
                let mut returned = 0;
                // ProcessWow64Information returns the 32-bit PEB address.
                // SAFETY: function signature matches ntdll and `peb` is a
                // correctly sized output buffer for `ProcessWow64Information`.
                let status = unsafe {
                    (ntdll.query_process)(
                        self.handle.raw(),
                        26,
                        (&mut peb as *mut usize).cast(),
                        size_of::<usize>() as u32,
                        &mut returned,
                    )
                };
                nt_success(status, "NtQueryInformationProcess(ProcessWow64Information)")?;
                let params = self.read_remote_pointer(peb + PEB32_PROCESS_PARAMETERS, 4)?;
                self.read_unicode_descriptor(params + RTL32_COMMAND_LINE, true)?
            }
        };
        let mut bytes = vec![0u8; descriptor.length as usize];
        self.read_exact(descriptor.buffer, &mut bytes)?;
        let units = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]));
        String::from_utf16(&units.collect::<Vec<_>>())
            .map_err(|_| backend("command line", "invalid UTF-16"))
    }

    fn read_remote_pointer(&self, address: usize, width: usize) -> Result<usize, MemoryError> {
        let mut bytes = [0u8; 8];
        self.read_exact(address, &mut bytes[..width])?;
        Ok(if width == 4 {
            u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize
        } else {
            u64::from_le_bytes(bytes)
                .try_into()
                .map_err(|_| backend("pointer", "pointer does not fit usize"))?
        })
    }

    fn read_unicode_descriptor(
        &self,
        address: usize,
        wow64: bool,
    ) -> Result<UnicodeDescriptor, MemoryError> {
        let size = if wow64 { 8 } else { 16 };
        let mut bytes = [0u8; 16];
        self.read_exact(address, &mut bytes[..size])?;
        decode_unicode_descriptor(&bytes[..size], wow64)
    }

    fn raw_read(&self, address: usize, dst: &mut [u8]) -> (bool, usize) {
        if dst.is_empty() {
            return (true, 0);
        }
        let mut read = 0;
        // SAFETY: the destination is valid for its full length; the remote pointer is treated as opaque.
        let ok = unsafe {
            ReadProcessMemory(
                self.handle.raw(),
                address as *const c_void,
                dst.as_mut_ptr().cast(),
                dst.len(),
                &mut read,
            )
        };
        (ok != 0, read)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcessBasicInformation64 {
    exit_status: i32,
    _padding: u32,
    peb_base_address: u64,
    affinity_mask: u64,
    base_priority: i32,
    _padding2: u32,
    unique_process_id: u64,
    inherited_from_unique_process_id: u64,
}

const PEB64_PROCESS_PARAMETERS: usize = 0x20;
const PEB32_PROCESS_PARAMETERS: usize = 0x10;
const RTL64_COMMAND_LINE: usize = 0x70;
const RTL32_COMMAND_LINE: usize = 0x40;
const _: () = assert!(size_of::<ProcessBasicInformation64>() == 48);
const _: () = assert!(offset_of!(ProcessBasicInformation64, peb_base_address) == 8);
const _: () = assert!(offset_of!(ProcessBasicInformation64, unique_process_id) == 32);

#[repr(C)]
struct Peb64Layout {
    reserved: [u8; PEB64_PROCESS_PARAMETERS],
    process_parameters: u64,
}

#[repr(C)]
struct Peb32Layout {
    reserved: [u8; PEB32_PROCESS_PARAMETERS],
    process_parameters: u32,
}

#[repr(C)]
struct UnicodeString64Layout {
    length: u16,
    maximum_length: u16,
    padding: u32,
    buffer: u64,
}

#[repr(C)]
struct UnicodeString32Layout {
    length: u16,
    maximum_length: u16,
    buffer: u32,
}

#[repr(C)]
struct ProcessParameters64Layout {
    reserved: [u8; RTL64_COMMAND_LINE],
    command_line: UnicodeString64Layout,
}

#[repr(C)]
struct ProcessParameters32Layout {
    reserved: [u8; RTL32_COMMAND_LINE],
    command_line: UnicodeString32Layout,
}

const _: () = assert!(size_of::<Peb64Layout>() == 0x28);
const _: () = assert!(offset_of!(Peb64Layout, process_parameters) == PEB64_PROCESS_PARAMETERS);
const _: () = assert!(size_of::<Peb32Layout>() == 0x14);
const _: () = assert!(offset_of!(Peb32Layout, process_parameters) == PEB32_PROCESS_PARAMETERS);
const _: () = assert!(size_of::<UnicodeString64Layout>() == 16);
const _: () = assert!(offset_of!(UnicodeString64Layout, buffer) == 8);
const _: () = assert!(size_of::<UnicodeString32Layout>() == 8);
const _: () = assert!(offset_of!(UnicodeString32Layout, buffer) == 4);
const _: () = assert!(offset_of!(ProcessParameters64Layout, command_line) == RTL64_COMMAND_LINE);
const _: () = assert!(offset_of!(ProcessParameters32Layout, command_line) == RTL32_COMMAND_LINE);

struct UnicodeDescriptor {
    length: u16,
    buffer: usize,
}

fn decode_unicode_descriptor(bytes: &[u8], wow64: bool) -> Result<UnicodeDescriptor, MemoryError> {
    let expected = if wow64 { 8 } else { 16 };
    if bytes.len() != expected {
        return Err(backend("command line", "invalid UNICODE_STRING size"));
    }
    let length = u16::from_le_bytes(bytes[0..2].try_into().unwrap());
    let maximum = u16::from_le_bytes(bytes[2..4].try_into().unwrap());
    if length > maximum || length % 2 != 0 {
        return Err(backend("command line", "invalid UNICODE_STRING length"));
    }
    let buffer = if wow64 {
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize
    } else {
        usize::try_from(u64::from_le_bytes(bytes[8..16].try_into().unwrap()))
            .map_err(|_| backend("command line", "buffer pointer does not fit usize"))?
    };
    Ok(UnicodeDescriptor { length, buffer })
}

type NtQueryInformationProcess =
    unsafe extern "system" fn(HANDLE, u32, *mut c_void, u32, *mut u32) -> i32;
type NtQueryTimerResolution = unsafe extern "system" fn(*mut u32, *mut u32, *mut u32) -> i32;
type NtSetTimerResolution = unsafe extern "system" fn(u32, u8, *mut u32) -> i32;

struct Ntdll {
    query_process: NtQueryInformationProcess,
    query_timer: Option<NtQueryTimerResolution>,
    set_timer: Option<NtSetTimerResolution>,
}

impl Ntdll {
    fn load() -> Result<&'static Self, MemoryError> {
        static INSTANCE: OnceLock<Ntdll> = OnceLock::new();

        if let Some(ntdll) = INSTANCE.get() {
            return Ok(ntdll);
        }

        const NTDLL: &[u16] = &[110, 116, 100, 108, 108, 46, 100, 108, 108, 0];
        // SAFETY: `NTDLL` is a NUL-terminated UTF-16 string, the only
        // precondition `GetModuleHandleW` documents for its pointer
        // argument; GetModuleHandle deliberately does not load a new library.
        let module = unsafe { GetModuleHandleW(NTDLL.as_ptr()) };
        if module.is_null() {
            return Err(last_error("GetModuleHandleW(ntdll.dll)"));
        }
        let loaded = Self {
            query_process: resolve_required(module, c"NtQueryInformationProcess")?,
            query_timer: resolve_optional(module, c"NtQueryTimerResolution"),
            set_timer: resolve_optional(module, c"NtSetTimerResolution"),
        };
        let _ = INSTANCE.set(loaded);
        Ok(INSTANCE
            .get()
            .expect("ntdll export cache initialized by this call"))
    }
}

pub struct TimerResolutionGuard {
    set: Option<NtSetTimerResolution>,
    requested: u32,
}

impl fmt::Debug for TimerResolutionGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimerResolutionGuard")
            .field("requested", &self.requested)
            .finish()
    }
}

impl TimerResolutionGuard {
    pub fn request_minimum(warn: impl FnOnce(&str)) -> Self {
        let Ok(ntdll) = Ntdll::load() else {
            warn("ntdll.dll is unavailable; retaining the default timer policy");
            return Self {
                set: None,
                requested: 0,
            };
        };
        let (Some(query), Some(set)) = (ntdll.query_timer, ntdll.set_timer) else {
            warn(
                "ntdll timer-resolution exports are unavailable; retaining the default timer policy",
            );
            return Self {
                set: None,
                requested: 0,
            };
        };
        let (mut minimum, mut maximum, mut current) = (0, 0, 0);
        // SAFETY: `minimum`, `maximum`, and `current` are valid, writable
        // local `u32`s; `query` was resolved from ntdll and matches its
        // documented `NtQueryTimerResolution` signature.
        let status = unsafe { query(&mut minimum, &mut maximum, &mut current) };
        if status < 0 {
            warn("NtQueryTimerResolution failed; retaining the default timer policy");
            return Self {
                set: None,
                requested: 0,
            };
        }
        let mut actual = current;
        // SAFETY: `actual` is a valid, writable local `u32`; `set` was
        // resolved from ntdll and matches its documented
        // `NtSetTimerResolution` signature.
        let status = unsafe { set(minimum, 1, &mut actual) };
        if status < 0 {
            warn("NtSetTimerResolution failed; retaining the default timer policy");
            return Self {
                set: None,
                requested: 0,
            };
        }
        Self {
            set: Some(set),
            requested: minimum,
        }
    }
}

impl Drop for TimerResolutionGuard {
    fn drop(&mut self) {
        if let Some(set) = self.set {
            let mut actual = 0;
            // SAFETY: `actual` is a valid, writable local `u32`; `set` was
            // resolved from ntdll and matches its documented
            // `NtSetTimerResolution` signature; restoring on drop is best
            // effort and its result is intentionally discarded.
            unsafe {
                set(self.requested, 0, &mut actual);
            }
        }
    }
}

fn resolve_optional<T: Copy>(
    module: windows_sys::Win32::Foundation::HMODULE,
    name: &CStr,
) -> Option<T> {
    // SAFETY: `module` is a valid module handle from `GetModuleHandleW`
    // above and `name` is a NUL-terminated C string, the only precondition
    // `GetProcAddress` documents for its pointer arguments.
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
    // SAFETY: function pointers and `FARPROC` have the same representation
    // on Windows, and every caller resolves `T` for the symbol it names.
    Some(unsafe { std::mem::transmute_copy(&address) })
}

fn resolve_required<T: Copy>(
    module: windows_sys::Win32::Foundation::HMODULE,
    name: &CStr,
) -> Result<T, MemoryError> {
    resolve_optional(module, name)
        .ok_or_else(|| backend("GetProcAddress", "required ntdll export is unavailable"))
}

fn nt_success(status: i32, operation: &'static str) -> Result<(), MemoryError> {
    if status >= 0 {
        Ok(())
    } else {
        Err(backend(operation, &format!("NTSTATUS {status:#010x}")))
    }
}

fn wide_array(chars: &[u16]) -> String {
    let len = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
    String::from_utf16_lossy(&chars[..len])
}

fn backend(operation: &'static str, message: &str) -> MemoryError {
    MemoryError::Backend {
        operation,
        message: message.to_owned(),
    }
}

fn finish_enumeration(operation: &'static str) -> Result<(), MemoryError> {
    // SAFETY: `GetLastError` has no memory-safety preconditions.
    let error = unsafe { GetLastError() };
    if error == ERROR_NO_MORE_FILES {
        Ok(())
    } else {
        Err(backend(operation, &format!("Windows error {error}")))
    }
}

fn last_error(operation: &'static str) -> MemoryError {
    backend(
        operation,
        // SAFETY: `GetLastError` has no memory-safety preconditions.
        &format!("Windows error {}", unsafe { GetLastError() }),
    )
}

#[cfg(test)]
mod tests;
