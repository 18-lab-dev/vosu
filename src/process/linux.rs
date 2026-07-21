//! Native Linux process metadata and memory access.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};

const MAX_PROC_FILE: u64 = 1024 * 1024;
const MAX_COMM_FILE: u64 = 4096;
const PERMISSION_GUIDANCE: &str =
    "Grant vosu CAP_SYS_PTRACE before starting it:\n  sudo setcap cap_sys_ptrace=eip /path/to/vosu";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxProcessMetadata {
    pub pid: u32,
    pub name: OsString,
    pub command_line: Vec<OsString>,
    pub executable: PathBuf,
    pub bitness: ProcessBitness,
}

trait ProcessVmReadv: Send + Sync {
    fn readv(
        &self,
        pid: libc::pid_t,
        local: &mut [libc::iovec],
        remote: &[libc::iovec],
    ) -> Result<usize, i32>;

    fn iov_max(&self) -> usize;
}

#[derive(Debug)]
struct LibcProcessVmReadv;

impl ProcessVmReadv for LibcProcessVmReadv {
    fn readv(
        &self,
        pid: libc::pid_t,
        local: &mut [libc::iovec],
        remote: &[libc::iovec],
    ) -> Result<usize, i32> {
        // SAFETY: both iovec arrays borrow live buffers for the duration of this call.
        let result = unsafe {
            libc::process_vm_readv(
                pid,
                local.as_ptr(),
                local.len() as libc::c_ulong,
                remote.as_ptr(),
                remote.len() as libc::c_ulong,
                0,
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EIO))
        } else {
            Ok(result as usize)
        }
    }

    fn iov_max(&self) -> usize {
        // SAFETY: sysconf has no memory-safety preconditions.
        let value = unsafe { libc::sysconf(libc::_SC_IOV_MAX) };
        if value > 0 { value as usize } else { 1024 }
    }
}

#[derive(Clone)]
pub struct LinuxProcessMemory {
    pid: libc::pid_t,
    proc_root: PathBuf,
    syscall: Arc<dyn ProcessVmReadv>,
}

impl std::fmt::Debug for LinuxProcessMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinuxProcessMemory")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

impl LinuxProcessMemory {
    #[must_use]
    pub fn new(pid: u32) -> Self {
        Self::with_parts(pid, PathBuf::from("/proc"), Arc::new(LibcProcessVmReadv))
    }

    fn with_parts(pid: u32, proc_root: PathBuf, syscall: Arc<dyn ProcessVmReadv>) -> Self {
        Self {
            pid: pid as libc::pid_t,
            proc_root,
            syscall,
        }
    }

    fn mapped_regions(&self, writable_only: bool) -> Result<Vec<MemoryRegion>, MemoryError> {
        parse_maps(
            &read_bounded(&self.proc_root.join(self.pid.to_string()).join("maps"))
                .map_err(|error| backend("read /proc maps", error))?,
            writable_only,
        )
    }

    fn read_many_inner(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        let regions = self.mapped_regions(false)?;
        let mut chunks = Vec::new();
        for (request_index, request) in reads.iter().enumerate() {
            let mut address = request.address;
            let mut offset = 0usize;
            while offset < request.dst.len() {
                let region = regions
                    .iter()
                    .find(|region| {
                        address >= region.address
                            && address < region.address.saturating_add(region.size)
                    })
                    .ok_or_else(|| {
                        read_error(
                            address,
                            request.dst.len() - offset,
                            "address is outside a readable mapping",
                        )
                    })?;
                let available = region.address.saturating_add(region.size) - address;
                let len = available.min(request.dst.len() - offset);
                chunks.push(Chunk {
                    request_index,
                    offset,
                    address,
                    len,
                });
                address = address
                    .checked_add(len)
                    .ok_or(super::MemoryError::AddressOverflow {
                        base: address,
                        offset: len,
                    })?;
                offset += len;
            }
        }

        let max = self.syscall.iov_max().max(1);
        for batch in chunks.chunks(max) {
            self.read_batch(reads, batch)?;
        }
        Ok(())
    }

    fn read_batch(
        &self,
        reads: &mut [ReadRequest<'_>],
        chunks: &[Chunk],
    ) -> Result<(), MemoryError> {
        let mut local = Vec::with_capacity(chunks.len());
        let mut remote = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            let ptr = reads[chunk.request_index].dst[chunk.offset..].as_mut_ptr();
            local.push(libc::iovec {
                iov_base: ptr.cast(),
                iov_len: chunk.len,
            });
            remote.push(libc::iovec {
                iov_base: chunk.address as *mut libc::c_void,
                iov_len: chunk.len,
            });
        }
        let expected: usize = chunks.iter().map(|chunk| chunk.len).sum();
        let completed = self.call(&mut local, &remote, chunks[0].address, expected)?;
        if completed == expected {
            return Ok(());
        }
        if completed > expected {
            return Err(read_error(
                chunks[0].address,
                expected,
                "process_vm_readv returned more bytes than requested",
            ));
        }

        let mut prefix = completed;
        for chunk in chunks {
            if prefix >= chunk.len {
                prefix -= chunk.len;
                continue;
            }
            let suffix_offset = chunk.offset + prefix;
            let suffix_address = chunk.address + prefix;
            let suffix_len = chunk.len - prefix;
            let ptr = reads[chunk.request_index].dst[suffix_offset..].as_mut_ptr();
            let mut one_local = [libc::iovec {
                iov_base: ptr.cast(),
                iov_len: suffix_len,
            }];
            let one_remote = [libc::iovec {
                iov_base: suffix_address as *mut libc::c_void,
                iov_len: suffix_len,
            }];
            let retried = self.call(&mut one_local, &one_remote, suffix_address, suffix_len)?;
            if retried != suffix_len {
                return Err(read_error(
                    suffix_address,
                    suffix_len,
                    "short process_vm_readv retry",
                ));
            }
            prefix = 0;
        }
        Ok(())
    }

    fn call(
        &self,
        local: &mut [libc::iovec],
        remote: &[libc::iovec],
        address: usize,
        len: usize,
    ) -> Result<usize, MemoryError> {
        self.syscall
            .readv(self.pid, local, remote)
            .map_err(|errno| {
                let message = io::Error::from_raw_os_error(errno).to_string();
                if matches!(errno, libc::EPERM | libc::EACCES) {
                    MemoryError::PermissionDenied {
                        address,
                        len,
                        message: format!("{message}. {PERMISSION_GUIDANCE}"),
                    }
                } else {
                    read_error(address, len, message)
                }
            })
    }
}

#[derive(Debug, Clone, Copy)]
struct Chunk {
    request_index: usize,
    offset: usize,
    address: usize,
    len: usize,
}

impl ProcessMemory for LinuxProcessMemory {
    fn read_scanner_region_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        if dst.is_empty() {
            return Ok(());
        }
        let len = dst.len();
        let result = {
            let mut request = [ReadRequest::new(address, dst)];
            let chunks = [Chunk {
                request_index: 0,
                offset: 0,
                address,
                len,
            }];
            self.read_batch(&mut request, &chunks)
        };
        if result.is_err() {
            dst.fill(0);
        }
        result
    }

    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let mut request = [ReadRequest::new(address, dst)];
        self.read_many(&mut request)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        let result = self.read_many_inner(reads);
        if result.is_err() {
            for read in reads {
                read.dst.fill(0);
            }
        }
        result
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        self.mapped_regions(true)
    }

    fn process_id(&self) -> Option<u32> {
        u32::try_from(self.pid).ok()
    }

    fn is_alive(&self) -> bool {
        u32::try_from(self.pid).is_ok_and(is_process_alive)
    }
}

pub fn open_process(pid: u32) -> Result<LinuxProcessMemory, MemoryError> {
    let memory = LinuxProcessMemory::new(pid);
    if !is_process_alive(pid) {
        return Err(read_error(0, 0, "process is not alive"));
    }
    memory.mapped_regions(false)?;
    Ok(memory)
}

pub fn enumerate_processes() -> Result<Vec<LinuxProcessMetadata>, MemoryError> {
    enumerate_processes_matching(|_| true)
}

/// Reads `comm` first and defers heavier metadata reads until `include` accepts it.
pub fn enumerate_processes_matching(
    include: impl FnMut(&OsStr) -> bool,
) -> Result<Vec<LinuxProcessMetadata>, MemoryError> {
    enumerate_processes_matching_at(Path::new("/proc"), include)
}

#[cfg(test)]
fn enumerate_processes_at(proc_root: &Path) -> Result<Vec<LinuxProcessMetadata>, MemoryError> {
    enumerate_processes_matching_at(proc_root, |_| true)
}

fn enumerate_processes_matching_at(
    proc_root: &Path,
    mut include: impl FnMut(&OsStr) -> bool,
) -> Result<Vec<LinuxProcessMetadata>, MemoryError> {
    let entries = fs::read_dir(proc_root).map_err(|error| backend("enumerate /proc", error))?;
    let mut processes = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let dir = entry.path();
        let Ok(name) = process_name_at(&dir) else {
            continue;
        };
        if !include(&name) {
            continue;
        }
        let Ok(command_line) = command_line_at(&dir) else {
            continue;
        };
        let Ok(executable) = fs::read_link(dir.join("exe")) else {
            continue;
        };
        let Ok(bitness) = elf_bitness_at(&dir.join("exe")) else {
            continue;
        };
        processes.push(LinuxProcessMetadata {
            pid,
            name,
            command_line,
            executable,
            bitness,
        });
    }
    processes.sort_unstable_by_key(|process| process.pid);
    Ok(processes)
}

pub fn process_name(pid: u32) -> Result<OsString, MemoryError> {
    process_name_at(&Path::new("/proc").join(pid.to_string()))
}

fn process_name_at(dir: &Path) -> Result<OsString, MemoryError> {
    let mut bytes = read_prefix(&dir.join("comm"), MAX_COMM_FILE + 1)
        .map_err(|error| backend("read /proc comm", error))?;
    if bytes.len() > MAX_COMM_FILE as usize {
        return Err(backend_message(
            "read /proc comm",
            "comm exceeds 4096 bytes",
        ));
    }
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    Ok(OsString::from_vec(bytes))
}

pub fn process_command_line(pid: u32) -> Result<Vec<OsString>, MemoryError> {
    command_line_at(&Path::new("/proc").join(pid.to_string()))
}

fn command_line_at(dir: &Path) -> Result<Vec<OsString>, MemoryError> {
    let bytes =
        read_bounded(&dir.join("cmdline")).map_err(|error| backend("read /proc cmdline", error))?;
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| OsString::from_vec(part.to_vec()))
        .collect())
}

pub fn process_executable(pid: u32) -> Result<PathBuf, MemoryError> {
    fs::read_link(Path::new("/proc").join(pid.to_string()).join("exe"))
        .map_err(|error| backend("read /proc exe", error))
}

pub fn process_cwd(pid: u32) -> Result<PathBuf, MemoryError> {
    fs::read_link(Path::new("/proc").join(pid.to_string()).join("cwd"))
        .map_err(|error| backend("read /proc cwd", error))
}

pub fn process_root(pid: u32) -> Result<PathBuf, MemoryError> {
    let executable = process_executable(pid)?;
    Ok(root_for(Path::new("/proc"), pid, &executable))
}

fn root_for(proc_root: &Path, pid: u32, executable: &Path) -> PathBuf {
    let parent = executable.parent().unwrap_or(Path::new("/"));
    proc_root
        .join(pid.to_string())
        .join("root")
        .join(parent.strip_prefix("/").unwrap_or(parent))
}

pub fn process_bitness(pid: u32) -> Result<ProcessBitness, MemoryError> {
    elf_bitness_at(&Path::new("/proc").join(pid.to_string()).join("exe"))
}

fn elf_bitness_at(path: &Path) -> Result<ProcessBitness, MemoryError> {
    let bytes = read_prefix(path, 5).map_err(|error| backend("read ELF header", error))?;
    if bytes.len() < 5 || &bytes[..4] != b"\x7fELF" {
        return Err(backend_message(
            "read ELF header",
            "target executable is not ELF",
        ));
    }
    match bytes[4] {
        1 => Ok(ProcessBitness::X86),
        2 => Ok(ProcessBitness::X64),
        _ => Err(backend_message("read ELF header", "unknown ELF class")),
    }
}

pub fn is_process_alive(pid: u32) -> bool {
    is_process_alive_at(Path::new("/proc"), pid)
}

fn is_process_alive_at(proc_root: &Path, pid: u32) -> bool {
    let Ok(stat) = read_bounded(&proc_root.join(pid.to_string()).join("stat")) else {
        return false;
    };
    let Some(close) = stat.iter().rposition(|byte| *byte == b')') else {
        return false;
    };
    stat.get(close + 2)
        .is_some_and(|state| *state != b'Z' && *state != b'X')
}

/// Resolves the target's osu! data directory without invoking another process.
pub fn resolve_osu_path(pid: u32) -> Result<PathBuf, MemoryError> {
    let environment: Vec<_> = std::env::vars_os().collect();
    resolve_osu_path_at(Path::new("/proc"), pid, &environment)
}

fn resolve_osu_path_at(
    proc_root: &Path,
    pid: u32,
    env: &[(OsString, OsString)],
) -> Result<PathBuf, MemoryError> {
    let env_value = |key: &str| {
        env.iter()
            .find(|(name, _)| name == OsStr::new(key))
            .map(|(_, value)| value)
    };
    if let Some(path) = env_value("VOSU_OSU_PATH").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let dir = proc_root.join(pid.to_string());
    let executable =
        fs::read_link(dir.join("exe")).map_err(|error| backend("read /proc exe", error))?;
    if executable
        .as_os_str()
        .as_bytes()
        .windows(b"wine-preloader".len())
        .any(|part| part == b"wine-preloader")
    {
        let data_home = env_value("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env_value("HOME").map(|home| PathBuf::from(home).join(".local/share")));
        if let Some(config) = data_home.map(|path| path.join("osuconfig/osupath"))
            && config.is_file()
        {
            let bytes =
                read_bounded(&config).map_err(|error| backend("read osuconfig/osupath", error))?;
            return Ok(PathBuf::from(OsString::from_vec(
                trim_ascii(&bytes).to_vec(),
            )));
        }
        for argument in command_line_at(&dir)?.into_iter().skip(1) {
            let bytes = argument.as_bytes();
            if bytes.to_ascii_lowercase().ends_with(b"osu!.exe") {
                let converted: Vec<u8> = bytes
                    .iter()
                    .map(|byte| if *byte == b'\\' { b'/' } else { *byte })
                    .collect();
                let without_drive = if converted.get(1) == Some(&b':') {
                    &converted[2..]
                } else {
                    &converted
                };
                let path = PathBuf::from(OsString::from_vec(without_drive.to_vec()));
                return Ok(path.parent().unwrap_or(Path::new("/")).to_path_buf());
            }
        }
    }
    fs::read_link(dir.join("cwd")).map_err(|error| backend("read /proc cwd", error))
}

fn parse_maps(bytes: &[u8], writable_only: bool) -> Result<Vec<MemoryRegion>, MemoryError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| backend_message("parse /proc maps", "maps is not UTF-8"))?;
    let mut regions = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let range = fields
            .next()
            .ok_or_else(|| backend_message("parse /proc maps", "missing address range"))?;
        let permissions = fields
            .next()
            .ok_or_else(|| backend_message("parse /proc maps", "missing permissions"))?;
        if !permissions.starts_with('r') || (writable_only && !permissions.starts_with("rw")) {
            continue;
        }
        let (start, end) = range
            .split_once('-')
            .ok_or_else(|| backend_message("parse /proc maps", "invalid address range"))?;
        let start = usize::from_str_radix(start, 16)
            .map_err(|_| backend_message("parse /proc maps", "invalid start address"))?;
        let end = usize::from_str_radix(end, 16)
            .map_err(|_| backend_message("parse /proc maps", "invalid end address"))?;
        let size = end
            .checked_sub(start)
            .filter(|size| *size != 0)
            .ok_or_else(|| backend_message("parse /proc maps", "empty or reversed range"))?;
        regions.push(MemoryRegion {
            address: start,
            size,
        });
    }
    Ok(regions)
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length > MAX_PROC_FILE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "proc file exceeds 1 MiB",
        ));
    }
    use std::io::Read;
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_PROC_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PROC_FILE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "proc file exceeds 1 MiB",
        ));
    }
    Ok(bytes)
}

fn read_prefix(path: &Path, length: u64) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::with_capacity(length as usize);
    fs::File::open(path)?.take(length).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

fn backend(operation: &'static str, error: io::Error) -> MemoryError {
    backend_message(operation, error.to_string())
}
fn backend_message(operation: &'static str, message: impl Into<String>) -> MemoryError {
    MemoryError::Backend {
        operation,
        message: message.into(),
    }
}
fn read_error(address: usize, len: usize, message: impl Into<String>) -> MemoryError {
    MemoryError::Read {
        address,
        len,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;
