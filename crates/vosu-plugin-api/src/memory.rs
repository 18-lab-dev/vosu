//! Platform-neutral target-process memory vocabulary: the object-safe
//! [`ProcessMemory`] read boundary, its request/status/error types, the
//! ordered default chain and two-level read helpers, and signature
//! [`Pattern`] parsing.
//!
//! These are pure values and algorithms over the trait. Native process
//! handles, syscalls, readable-region enumeration and the signature
//! region walk live in `vosu-sys`, behind core; a plugin reaches target
//! memory only through `VOSU_OP_READ_PLAN_SUBMIT`/`VOSU_OP_SCAN_REQUEST`
//! (`docs/v2/plugin-abi.md` §6.3–§6.4), for example via a
//! `ProcessMemory` implementation built on those read plans.

mod backend;
mod pattern;
mod request;
mod status;
#[cfg(test)]
mod tests;

pub use backend::{ProcessBitness, ProcessMemory, read_chains_default, read_indirect_many_default};
pub use pattern::{BatchScanResult, Pattern, PatternParseError};
pub use request::{ChainReadRequest, IndirectReadRequest, ReadRequest};
pub use status::{MemoryError, MemoryRead, MemoryRegion, ReadStatus};
