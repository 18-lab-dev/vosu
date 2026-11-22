//! Safe, platform-independent target-process memory boundary.
//!
//! OS-specific handle management and syscalls implement [`ProcessMemory`]. This
//! module deliberately keeps decoding above that object-safe boundary so generic
//! collection callbacks remain monomorphized.

use std::fmt;

use thiserror::Error;

pub mod scanner;
pub use scanner::{BatchScanResult, Pattern, PatternParseError, SCAN_CHUNK_SIZE};

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::{
    ModuleEntry, OwnedHandle, ProcessEntry, ThreadEntry, TimerResolutionGuard,
    WindowsProcessMemory, enumerate_modules, enumerate_processes, enumerate_threads,
    foreground_process_id, process_bitness, process_command_line,
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
