//! Native Windows process access.

use std::{
    ffi::{CStr, c_void},
    fmt,
    mem::{offset_of, size_of},
    sync::Arc,
};

use windows_sys::{
    Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation},
    Win32::{
        Foundation::{
            CloseHandle, ERROR_NO_MORE_FILES, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
            STILL_ACTIVE,
        },
        System::{
            Diagnostics::{Debug::ReadProcessMemory, ToolHelp::*},
            LibraryLoader::{GetModuleHandleW, GetProcAddress},
            Memory::{
                MEM_COMMIT, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_READWRITE,
                VirtualQueryEx,
            },
            SystemInformation::{IMAGE_FILE_MACHINE_AMD64, IMAGE_FILE_MACHINE_I386},
            Threading::{
                GetExitCodeProcess, IsWow64Process2, OpenProcess, PROCESS_QUERY_INFORMATION,
                PROCESS_VM_READ,
            },
            WindowsProgramming::SYSTEM_PROCESS_INFORMATION,
        },
        UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    },
};

use super::{MemoryError, MemoryRegion, ProcessBitness, ProcessHandle, ProcessMemory, ReadRequest};
mod read_impl;
use read_impl::{read_exact_impl, read_many_impl};

#[derive(Debug)]
pub struct OwnedHandle {
    handle: HANDLE,
    close: unsafe extern "system" fn(HANDLE) -> i32,
}

unsafe impl Send for OwnedHandle {}
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
        // SAFETY: no pointers are passed and the returned handle is immediately owned.
        let handle = unsafe { CreateToolhelp32Snapshot(flags, pid) };
        OwnedHandle::new(handle, "CreateToolhelp32Snapshot").map(Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent_pid: u32,
    pub thread_count: u32,
    pub base_priority: i32,
    pub executable: String,
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
    let mut storage = Vec::new();
    enumerate_processes_matching_with_buffer(&mut storage, |_| true)
}

pub(crate) fn enumerate_processes_matching_with_buffer(
    storage: &mut Vec<usize>,
    predicate: impl FnMut(&[u16]) -> bool,
) -> Result<Vec<ProcessEntry>, MemoryError> {
    if storage.len() < INITIAL_PROCESS_BUFFER_SIZE.div_ceil(size_of::<usize>()) {
        storage.resize(INITIAL_PROCESS_BUFFER_SIZE.div_ceil(size_of::<usize>()), 0);
    }
    enumerate_processes_with_buffer(storage, predicate)
}

fn enumerate_processes_with_buffer(
    storage: &mut Vec<usize>,
    mut predicate: impl FnMut(&[u16]) -> bool,
) -> Result<Vec<ProcessEntry>, MemoryError> {
    loop {
        let byte_capacity = storage.len() * size_of::<usize>();
        let buffer_size = u32::try_from(byte_capacity)
            .map_err(|_| backend("NtQuerySystemInformation", "process list exceeds 4 GiB"))?;
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
            let required = (returned as usize).max(
                byte_capacity
                    .checked_mul(2)
                    .ok_or_else(|| backend("NtQuerySystemInformation", "buffer size overflow"))?,
            );
            storage.resize(required.div_ceil(size_of::<usize>()), 0);
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

pub fn enumerate_threads(pid: Option<u32>) -> Result<Vec<ThreadEntry>, MemoryError> {
    let snapshot = Snapshot::create(TH32CS_SNAPTHREAD, 0)?;
    let mut raw: THREADENTRY32 = unsafe { std::mem::zeroed() };
    raw.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut entries = Vec::new();
    let mut ok = unsafe { Thread32First(snapshot.0.raw(), &mut raw) };
    while ok != 0 {
        if pid.is_none_or(|owner| owner == raw.th32OwnerProcessID) {
            entries.push(ThreadEntry {
                tid: raw.th32ThreadID,
                owner_pid: raw.th32OwnerProcessID,
                base_priority: raw.tpBasePri,
            });
        }
        ok = unsafe { Thread32Next(snapshot.0.raw(), &mut raw) };
    }
    finish_enumeration("Thread32Next")?;
    Ok(entries)
}

pub fn enumerate_modules(pid: u32) -> Result<Vec<ModuleEntry>, MemoryError> {
    let snapshot = Snapshot::create(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid)?;
    let mut raw: MODULEENTRY32W = unsafe { std::mem::zeroed() };
    raw.dwSize = size_of::<MODULEENTRY32W>() as u32;
    let mut entries = Vec::new();
    let mut ok = unsafe { Module32FirstW(snapshot.0.raw(), &mut raw) };
    while ok != 0 {
        entries.push(ModuleEntry {
            process_id: raw.th32ProcessID,
            base_address: raw.modBaseAddr as usize,
            size: raw.modBaseSize,
            name: wide_array(&raw.szModule),
            path: wide_array(&raw.szExePath),
        });
        ok = unsafe { Module32NextW(snapshot.0.raw(), &mut raw) };
    }
    finish_enumeration("Module32NextW")?;
    Ok(entries)
}

pub fn open_process(pid: u32) -> Result<ProcessHandle, MemoryError> {
    let memory = WindowsProcessMemory::open(pid)?;
    let bitness = memory.bitness()?;
    Ok(ProcessHandle::new(Arc::new(memory), bitness))
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

#[derive(Debug)]
pub struct WindowsProcessMemory {
    pid: u32,
    handle: OwnedHandle,
}

impl WindowsProcessMemory {
    pub fn open(pid: u32) -> Result<Self, MemoryError> {
        // SAFETY: OpenProcess returns an owned handle and inherit is disabled.
        let handle = unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid) };
        Ok(Self {
            pid,
            handle: OwnedHandle::new(handle, "OpenProcess")?,
        })
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

    pub fn command_line(&self) -> Result<String, MemoryError> {
        let ntdll = Ntdll::load()?;
        let bitness = self.bitness()?;
        let descriptor = match bitness {
            ProcessBitness::X64 => {
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

impl ProcessMemory for WindowsProcessMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        read_exact_impl(address, dst, |address, dst| self.raw_read(address, dst))
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        read_many_impl(reads, |address, dst| self.raw_read(address, dst))
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        let mut regions = Vec::new();
        let mut address = 0usize;
        loop {
            let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: info is a correctly sized writable output buffer.
            let returned = unsafe {
                VirtualQueryEx(
                    self.handle.raw(),
                    address as *const c_void,
                    &mut info,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if returned == 0 {
                break;
            }
            // Signature scans intentionally mirror the source backend: only writable
            // committed mappings participate, avoiding false positives in image pages.
            if info.State == MEM_COMMIT
                && info.Protect & (PAGE_READWRITE | PAGE_EXECUTE_READWRITE) != 0
            {
                regions.push(MemoryRegion {
                    address: info.BaseAddress as usize,
                    size: info.RegionSize,
                });
            }
            let next = (info.BaseAddress as usize)
                .checked_add(info.RegionSize)
                .ok_or(MemoryError::AddressOverflow {
                    base: info.BaseAddress as usize,
                    offset: info.RegionSize,
                })?;
            if next <= address {
                break;
            }
            address = next;
        }
        Ok(regions)
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn is_alive(&self) -> bool {
        let mut exit_code = 0;
        // SAFETY: handle remains owned and exit_code is a valid writable pointer.
        unsafe {
            GetExitCodeProcess(self.handle.raw(), &mut exit_code) != 0
                && exit_code == STILL_ACTIVE as u32
        }
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
    fn load() -> Result<Self, MemoryError> {
        const NTDLL: &[u16] = &[110, 116, 100, 108, 108, 46, 100, 108, 108, 0];
        // GetModuleHandle deliberately does not load a new library.
        let module = unsafe { GetModuleHandleW(NTDLL.as_ptr()) };
        if module.is_null() {
            return Err(last_error("GetModuleHandleW(ntdll.dll)"));
        }
        Ok(Self {
            query_process: resolve_required(module, c"NtQueryInformationProcess")?,
            query_timer: resolve_optional(module, c"NtQueryTimerResolution"),
            set_timer: resolve_optional(module, c"NtSetTimerResolution"),
        })
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
        let status = unsafe { query(&mut minimum, &mut maximum, &mut current) };
        if status < 0 {
            warn("NtQueryTimerResolution failed; retaining the default timer policy");
            return Self {
                set: None,
                requested: 0,
            };
        }
        let mut actual = current;
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
    let address = unsafe { GetProcAddress(module, name.as_ptr().cast()) }?;
    // Function pointers and FARPROC have the same representation on Windows.
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
        &format!("Windows error {}", unsafe { GetLastError() }),
    )
}

#[cfg(test)]
mod tests;
