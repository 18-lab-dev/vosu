//! The object-safe read boundary and its ordered default helpers.

use super::{ChainReadRequest, IndirectReadRequest, MemoryError, MemoryRegion, ReadRequest};

fn resolve_chain<M: ProcessMemory + ?Sized>(
    memory: &M,
    mut address: usize,
    offsets: &[usize],
    bitness: ProcessBitness,
) -> Result<usize, MemoryError> {
    for &offset in offsets {
        let mut bytes = [0; 8];
        memory.read_exact(address, &mut bytes[..bitness.pointer_size()])?;
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
    Ok(address)
}

/// Ordered fallback for backends without a mixed-chain executor.
pub fn read_chains_default<M: ProcessMemory + ?Sized>(
    memory: &M,
    reads: &mut [ChainReadRequest<'_>],
    bitness: ProcessBitness,
) -> Result<(), MemoryError> {
    for read in reads {
        if let Some(dst) = read.resolved_address.as_deref_mut() {
            let address = resolve_chain(memory, read.address, read.offsets, bitness)?;
            memory.read_exact(address, read.dst)?;
            *dst = address;
        } else {
            memory.read_chain(read.address, read.offsets, bitness, read.dst)?;
        }
    }
    Ok(())
}

/// Object-safe read boundary. Native implementations own their process
/// resource and release it when the final backend `Arc` is dropped; plan
/// implementations answer every read through the host's read-plan broker.
pub trait ProcessMemory: Send + Sync {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError>;
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError>;
    /// Follows pointer reads at `address`, then each relative offset, writing only
    /// the final read into `dst`. Zero pointers are ordinary addresses, not sentinels.
    /// Backends may execute the dependencies together without changing read order.
    fn read_chain(
        &self,
        address: usize,
        offsets: &[usize],
        bitness: ProcessBitness,
        dst: &mut [u8],
    ) -> Result<(), MemoryError> {
        let address = resolve_chain(self, address, offsets, bitness)?;
        self.read_exact(address, dst)
    }
    /// Executes chains in request order. Backends may combine their dependency
    /// stages, but must publish destinations and report errors in that order.
    fn read_chains(
        &self,
        reads: &mut [ChainReadRequest<'_>],
        bitness: ProcessBitness,
    ) -> Result<(), MemoryError> {
        read_chains_default(self, reads, bitness)
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
