//! Per-read outcomes: regions, errors and decoding readiness.

use thiserror::Error;

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
