use super::*;
use crate::process::launch_context::{LaunchContext, MAX_ENVIRONMENT_BYTES};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

pub(crate) fn launch_context(pid: u32) -> Result<LaunchContext, MemoryError> {
    let memory = WindowsProcessMemory::open(pid)?;
    if memory.bitness()? != ProcessBitness::X64 {
        return Err(backend(
            "launch context",
            "only 64-bit targets are supported",
        ));
    }
    let ntdll = Ntdll::load()?;
    // SAFETY: the C output structure contains only integers/pointers and accepts zero initialization.
    let mut pbi: ProcessBasicInformation64 = unsafe { std::mem::zeroed() };
    let mut returned = 0;
    // SAFETY: valid owned process handle, correctly sized writable output and length pointer.
    let status = unsafe {
        (ntdll.query_process)(
            memory.handle.raw(),
            0,
            (&mut pbi as *mut ProcessBasicInformation64).cast(),
            size_of::<ProcessBasicInformation64>() as u32,
            &mut returned,
        )
    };
    nt_success(status, "NtQueryInformationProcess")?;
    let checked = |base: usize, offset: usize| {
        base.checked_add(offset)
            .filter(|p| *p != 0)
            .ok_or_else(|| backend("launch context", "invalid remote address"))
    };
    let params = memory.read_remote_pointer(
        checked(pbi.peb_base_address as usize, PEB64_PROCESS_PARAMETERS)?,
        8,
    )?;
    // x64 RTL_USER_PROCESS_PARAMETERS: CurrentDirectory.DosPath at 0x38,
    // Environment at 0x80, EnvironmentSize at 0x3f0 (Vista and later).
    let descriptor = memory.read_unicode_descriptor(checked(params, 0x38)?, false)?;
    if descriptor.length == 0 {
        return Err(backend("launch context", "empty cwd"));
    }
    let mut cwd = vec![0; descriptor.length as usize];
    memory.read_exact(descriptor.buffer, &mut cwd)?;
    let cwd = PathBuf::from(OsString::from_wide(
        &cwd.chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>(),
    ));
    let environment = memory.read_remote_pointer(checked(params, 0x80)?, 8)?;
    let size = memory.read_remote_pointer(checked(params, 0x3f0)?, 8)?;
    if !cwd.is_absolute() || !(4..=MAX_ENVIRONMENT_BYTES).contains(&size) || size % 2 != 0 {
        return Err(backend("launch context", "invalid context lengths"));
    }
    let mut bytes = vec![0; size];
    memory.read_exact(environment, &mut bytes)?;
    let units = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect::<Vec<_>>();
    if !units.ends_with(&[0, 0]) {
        return Err(backend("launch context", "unterminated environment"));
    }
    let mut environment = Vec::new();
    for entry in units[..units.len() - 2].split(|c| *c == 0) {
        // Windows includes per-drive cwd entries named '=C:'. Command's env
        // API cannot represent those; ordinary variables retain exact OS bytes.
        if entry.first() == Some(&(b'=' as u16)) {
            continue;
        }
        let split = entry
            .iter()
            .position(|c| *c == b'=' as u16)
            .filter(|p| *p > 0)
            .ok_or_else(|| backend("launch context", "malformed environment"))?;
        environment.push((
            OsString::from_wide(&entry[..split]),
            OsString::from_wide(&entry[split + 1..]),
        ));
    }
    if environment.is_empty() {
        return Err(backend("launch context", "empty environment"));
    }
    Ok(LaunchContext { cwd, environment })
}
