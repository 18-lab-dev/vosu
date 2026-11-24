//! Safe, platform-independent target-process memory boundary.
//!
//! OS-specific handle management and syscalls implement [`ProcessMemory`]. This
//! module deliberately keeps decoding above that object-safe boundary so generic
//! collection callbacks remain monomorphized.

use std::fmt;

use thiserror::Error;

mod clock;
pub use clock::coarse_monotonic_millis;
pub mod scanner;
pub use scanner::{BatchScanResult, Pattern, PatternParseError, SCAN_CHUNK_SIZE};

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::{
    ModuleEntry, OwnedHandle, ProcessEntry, ThreadEntry, TimerResolutionGuard,
    WindowsProcessMemory, enumerate_modules, enumerate_processes, enumerate_threads,
    foreground_process_id, is_process_alive, process_bitness, process_command_line,
    process_image_path,
};
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::{
    LinuxProcessMemory, LinuxProcessMetadata, ModuleEntry, enumerate_modules, enumerate_processes,
    enumerate_processes_matching, is_process_alive, open_process, process_bitness,
    process_command_line, process_cwd, process_executable, process_metadata_matching, process_name,
    process_root,
};

/// A borrowed remote read. Backends must fill every destination or return an
/// error; partial initialization must never be reported as success.
pub struct ReadRequest<'a> {
    pub address: usize,
    pub dst: &'a mut [u8],
}

impl<'a> ReadRequest<'a> {
    #[must_use]
    pub fn new(address: usize, dst: &'a mut [u8]) -> Self {
        Self { address, dst }
    }
}

/// A read at the pointer stored at `address`, plus `offset`.
pub struct IndirectReadRequest<'a> {
    pub address: usize,
    pub offset: usize,
    pub dst: &'a mut [u8],
}

impl<'a> IndirectReadRequest<'a> {
    #[must_use]
    pub fn new(address: usize, offset: usize, dst: &'a mut [u8]) -> Self {
        Self {
            address,
            offset,
            dst,
        }
    }
}

impl fmt::Debug for ReadRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReadRequest")
            .field("address", &format_args!("{:#x}", self.address))
            .field("len", &self.dst.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRegion {
    pub address: usize,
    pub size: usize,
}

impl MemoryRegion {
    pub fn end(self) -> Result<usize, MemoryError> {
        self.address
            .checked_add(self.size)
            .ok_or(MemoryError::AddressOverflow {
                base: self.address,
                offset: self.size,
            })
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MemoryError {
    #[error("memory backend failed during {operation}: {message}")]
    Backend {
        operation: &'static str,
        message: String,
    },
    #[error("permission denied reading {len} bytes at {address:#x}: {message}")]
    PermissionDenied {
        address: usize,
        len: usize,
        message: String,
    },
    #[error("failed to read {len} bytes at {address:#x}: {message}")]
    Read {
        address: usize,
        len: usize,
        message: String,
    },
    #[error("address arithmetic overflow: {base:#x} + {offset:#x}")]
    AddressOverflow { base: usize, offset: usize },
    #[error("corrupt {context} at {address:#x}: {message}")]
    Corrupt {
        context: &'static str,
        address: usize,
        message: &'static str,
    },
    #[error("invalid UTF-16 in managed string at {address:#x}")]
    InvalidUtf16 { address: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadStatus<T> {
    Ready(T),
    NotReady,
    Unchanged,
    Unsupported(&'static str),
}

pub type MemoryRead<T> = Result<ReadStatus<T>, MemoryError>;

/// Object-safe syscall boundary. Implementations own their native process
/// resource and release it when the final backend `Arc` is dropped.
pub trait ProcessMemory: Send + Sync {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError>;
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError>;
    /// Follows pointer reads at `address`, then each relative offset, writing only
    /// the final read into `dst`. Zero pointers are ordinary addresses, not sentinels.
    /// Backends may execute the dependencies together without changing read order.
    fn read_chain(
        &self,
        mut address: usize,
        offsets: &[usize],
        bitness: ProcessBitness,
        dst: &mut [u8],
    ) -> Result<(), MemoryError> {
        for &offset in offsets {
            let mut bytes = [0; 8];
            self.read_exact(address, &mut bytes[..bitness.pointer_size()])?;
            let pointer =
                usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| MemoryError::Corrupt {
                    context: "pointer",
                    address,
                    message: "target pointer does not fit the host address width",
                })?;
            address = pointer
                .checked_add(offset)
                .ok_or(MemoryError::AddressOverflow {
                    base: pointer,
                    offset,
                })?;
        }
        self.read_exact(address, dst)
    }
    /// Reads pointer holders together, then their pointees together, in chunks
    /// of at most 24. Native backends retain two batches; plan backends may
    /// express both levels in one dependent plan. Null pointers remain readable.
    fn read_indirect_many(
        &self,
        reads: &mut [IndirectReadRequest<'_>],
        bitness: ProcessBitness,
    ) -> Result<(), MemoryError> {
        read_indirect_many_default(self, reads, bitness)
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError>;
    /// Reads bytes wholly contained in a region returned by [`Self::readable_regions`].
    ///
    /// Backends may bypass repeated mapping validation because the scanner has
    /// already obtained the region boundary. The default preserves existing
    /// behavior for backends without a cheaper native path.
    fn read_scanner_region_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.read_exact(address, dst)
    }

    /// Native process identifier when the backend represents an OS process.
    fn process_id(&self) -> Option<u32> {
        None
    }

    /// Cheap process-health probe. In-memory/test backends are alive by default.
    fn is_alive(&self) -> bool {
        true
    }

    /// Whether successful reads are tied to this process identity, either by
    /// an owned OS object or a generation check after every native read.
    /// Such backends can defer a plan's health probe until a read fails without
    /// allowing PID reuse to publish bytes from another process.
    fn reads_pin_process_identity(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessBitness {
    X86,
    X64,
}

impl ProcessBitness {
    #[must_use]
    pub const fn pointer_size(self) -> usize {
        match self {
            Self::X86 => 4,
            Self::X64 => 8,
        }
    }
}

/// Ordered two-level implementation for backends that cannot batch dependencies.
pub fn read_indirect_many_default<M: ProcessMemory + ?Sized>(
    memory: &M,
    reads: &mut [IndirectReadRequest<'_>],
    bitness: ProcessBitness,
) -> Result<(), MemoryError> {
    for chunk in reads.chunks_mut(24) {
        let mut pointers = [[0; 8]; 24];
        {
            let mut requests: [_; 24] = std::array::from_fn(|_| ReadRequest::new(0, &mut []));
            for (request, (read, bytes)) in requests
                .iter_mut()
                .zip(chunk.iter().zip(pointers.iter_mut()))
            {
                *request = ReadRequest::new(read.address, &mut bytes[..bitness.pointer_size()]);
            }
            memory.read_many(&mut requests[..chunk.len()])?;
        }
        let count = chunk.len();
        let mut requests: [_; 24] = std::array::from_fn(|_| ReadRequest::new(0, &mut []));
        for (request, (read, bytes)) in requests.iter_mut().zip(chunk.iter_mut().zip(pointers)) {
            let pointer =
                usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| MemoryError::Corrupt {
                    context: "pointer",
                    address: read.address,
                    message: "target pointer does not fit the host address width",
                })?;
            let address = pointer
                .checked_add(read.offset)
                .ok_or(MemoryError::AddressOverflow {
                    base: pointer,
                    offset: read.offset,
                })?;
            *request = ReadRequest::new(address, read.dst);
        }
        memory.read_many(&mut requests[..count])?;
    }
    Ok(())
}
