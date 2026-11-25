//! Borrowed read requests passed to [`super::ProcessMemory`].

use std::fmt;

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

/// An ordered pointer chain ending in a byte read. Empty offsets are a direct
/// read, so a batch can combine independent spans and dependent scalar fields.
pub struct ChainReadRequest<'a> {
    pub address: usize,
    pub offsets: &'a [usize],
    pub dst: &'a mut [u8],
    /// Receives the final byte-read address only after this request succeeds.
    pub resolved_address: Option<&'a mut usize>,
}

impl<'a> ChainReadRequest<'a> {
    #[must_use]
    pub fn new(address: usize, offsets: &'a [usize], dst: &'a mut [u8]) -> Self {
        Self {
            address,
            offsets,
            dst,
            resolved_address: None,
        }
    }

    #[must_use]
    pub fn with_resolved_address(mut self, dst: &'a mut usize) -> Self {
        self.resolved_address = Some(dst);
        self
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
