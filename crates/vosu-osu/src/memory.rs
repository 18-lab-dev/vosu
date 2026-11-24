//! `PlanProcessMemory`: a `ProcessMemory` implementation backed entirely by
//! `VOSU_READ_PLAN_SYNC` read plans (D5, P3 — `docs/v2/plugin-abi.md`
//! §6.3). Every `read_exact`/`read_many` call goes through
//! [`vosu_plugin_api::PlanMemory::read_flat`]'s zero-allocation path (F4,
//! D17) with thread-local scratch (a [`SyncReadBuffers`] and, for
//! `read_many`, a reused `Vec<PlanOp>`), copying a successful op's bytes
//! straight into the caller's own `dst` — never
//! [`vosu_plugin_api::PlanMemory::read`]'s allocating variant, and never an
//! intermediate owned `Vec<u8>` on the success path.
//!
//! A read plan's encoded submission and worst-case result are both capped
//! at `VOSU_READ_PLAN_MAX_BYTES` (64 KiB, §6.3): `read_exact` always
//! submits exactly one op per plan, splitting a longer read into several
//! sequential round trips when it exceeds that cap; `read_many` instead
//! batches as many of its independent requests as fit into one plan's ops
//! before starting the next round trip, so a short batch costs one round
//! trip rather than one per request.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::{cell::RefCell, mem::size_of};

use vosu_osu_reader::process::{
    MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest,
};
use vosu_plugin_api::{
    FlatReadResults, HostServices, PlanBase, PlanMemory, PlanMemoryError, PlanOp, StageBase,
    StageRef, SyncReadBuffers, VOSU_READ_PLAN_MAX_BYTES, VtableHostServices, vosu_plan_id,
    vosu_read_error_code, vosu_read_plan_result_header, vosu_read_plan_submit_header,
    vosu_read_result_flat, vosu_read_result_kind, vosu_session_id, vosu_stage_header, vosu_status,
};

thread_local! {
    /// Reused across every `read_exact`/`read_many` call this reader
    /// thread makes, on any session: once both of its buffers have grown
    /// to their steady-state capacity, no call here allocates (D17).
    static SYNC_READ_BUFFERS: RefCell<SyncReadBuffers> = RefCell::new(SyncReadBuffers::new());
    /// `read_many`'s batch scratch: reused the same way, instead of a
    /// fresh `Vec<PlanOp>` per batch.
    static OPS_SCRATCH: RefCell<Vec<PlanOp>> = const { RefCell::new(Vec::new()) };
}

const PLAN_CAP: usize = VOSU_READ_PLAN_MAX_BYTES as usize;
const RESULT_HEADER_BYTES: usize = size_of::<vosu_read_plan_result_header>();
const RESULT_ENTRY_BYTES: usize = size_of::<vosu_read_result_flat>();
const SUBMIT_HEADER_BYTES: usize = size_of::<vosu_read_plan_submit_header>();
const STAGE_HEADER_BYTES: usize = size_of::<vosu_stage_header>();
const OP_BYTES: usize = size_of::<PlanOp>();

/// The longest a single op's `length` can be while its own worst-case
/// result (header + one `vosu_read_result_flat` + its `length` bytes)
/// still fits `VOSU_READ_PLAN_MAX_BYTES`.
const MAX_SINGLE_OP_BYTES: usize = PLAN_CAP - RESULT_HEADER_BYTES - RESULT_ENTRY_BYTES;

/// A `ProcessMemory` backend that answers every read through this
/// session's host over `VOSU_READ_PLAN_SYNC` plans, generic over the
/// `HostServices` implementation so a unit test or C6's bench can supply
/// its own fake host instead of the real [`VtableHostServices`].
pub struct PlanProcessMemory<H: HostServices + Send + Sync + 'static = VtableHostServices> {
    host: H,
    handle: vosu_session_id,
    pid: u32,
    bitness: ProcessBitness,
    next_plan: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
}

impl<H: HostServices + Send + Sync + 'static> PlanProcessMemory<H> {
    #[must_use]
    pub fn new(host: H, handle: vosu_session_id, pid: u32, bitness: ProcessBitness) -> Self {
        Self::with_shared(
            host,
            handle,
            pid,
            bitness,
            Arc::new(AtomicU64::new(1)),
            Arc::new(AtomicBool::new(true)),
        )
    }

    /// Like [`Self::new`], sharing its plan-id counter and liveness flag
    /// with another backend for the same session. Pass the same `alive`
    /// `Arc` to [`crate::scan::PlanScanBackend::with_shared`] for the same
    /// session (`handle`): a `VOSU_ERR_TARGET_GONE` seen on either path
    /// then marks the session dead for both. `next_plan` is this read
    /// path's own plan-id counter; the scan path uses no plan ids, so
    /// sharing it is only for a single counter source, not collision
    /// avoidance with scans.
    #[must_use]
    pub fn with_shared(
        host: H,
        handle: vosu_session_id,
        pid: u32,
        bitness: ProcessBitness,
        next_plan: Arc<AtomicU64>,
        alive: Arc<AtomicBool>,
    ) -> Self {
        Self {
            host,
            handle,
            pid,
            bitness,
            next_plan,
            alive,
        }
    }

    #[must_use]
    pub fn alive_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.alive)
    }

    #[must_use]
    pub fn plan_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.next_plan)
    }

    #[must_use]
    pub const fn bitness(&self) -> ProcessBitness {
        self.bitness
    }

    fn next_plan_id(&self) -> vosu_plan_id {
        self.next_plan.fetch_add(1, Ordering::Relaxed)
    }

    /// Submits `root`/`ops` as one plan's single stage and applies each
    /// op's decoded result in submission order via `apply`, entirely
    /// inside this thread's [`SYNC_READ_BUFFERS`] borrow — nothing
    /// borrowed from it ever escapes this call. A `VOSU_ERR_TARGET_GONE`
    /// rejection flips [`Self::alive`] false before returning;
    /// `VOSU_ERR_PERMISSION_DENIED` becomes [`MemoryError::PermissionDenied`]
    /// rather than the generic [`MemoryError::Backend`] every other
    /// rejection becomes, since nothing else here can tell a permission
    /// failure apart from any other rejected submission (no per-address
    /// detail is available at this level — `address`/`len` are `0`).
    fn submit(
        &self,
        root: PlanBase,
        ops: &[PlanOp],
        mut apply: impl FnMut(usize, &FlatReadResults<'_>) -> Result<(), MemoryError>,
    ) -> Result<(), MemoryError> {
        let plan = self.next_plan_id();
        let stage = StageRef {
            base: StageBase::Root,
            base_offset: 0,
            ops,
        };
        SYNC_READ_BUFFERS.with(|cell| {
            let mut buffers = cell.borrow_mut();
            match PlanMemory::new().read_flat(
                &self.host,
                plan,
                self.handle,
                root,
                &[stage],
                &mut buffers,
            ) {
                Ok(results) => {
                    for index in 0..ops.len() {
                        apply(index, &results)?;
                    }
                    Ok(())
                }
                Err(PlanMemoryError::Rejected(status)) => {
                    if status == vosu_status::VOSU_ERR_TARGET_GONE {
                        self.alive.store(false, Ordering::Relaxed);
                    }
                    if status == vosu_status::VOSU_ERR_PERMISSION_DENIED {
                        return Err(MemoryError::PermissionDenied {
                            address: 0,
                            len: 0,
                            message: "host rejected the read plan: VOSU_ERR_PERMISSION_DENIED"
                                .to_owned(),
                        });
                    }
                    Err(MemoryError::Backend {
                        operation: "read plan",
                        message: format!("host rejected the read plan: {status:?}"),
                    })
                }
                Err(PlanMemoryError::NoResult) => Err(MemoryError::Backend {
                    operation: "read plan",
                    message: "the host accepted the plan but sent no synchronous result".to_owned(),
                }),
                Err(PlanMemoryError::OnDispatchThread) => Err(MemoryError::Backend {
                    operation: "read plan",
                    message: "called from the host's own serialized dispatch thread".to_owned(),
                }),
            }
        })
    }
}

/// Copies one op's decoded flat result straight into `dst` (no
/// intermediate owned `Vec<u8>`, D17), or returns the `MemoryError` it
/// decodes to. `results.error_code` recovers `VOSU_READ_ERROR_PERMISSION_DENIED`
/// as [`MemoryError::PermissionDenied`]; every other error code becomes
/// [`MemoryError::Read`], since nothing downstream matches on the
/// remaining distinctions ([`vosu_read_error_code::VOSU_READ_ERROR_BACKEND`]/
/// `_READ_FAILED`/`_ADDRESS_OVERFLOW`) today. This is not a hot path, so
/// its one allocation (the UTF-8 message) is fine — only the success path
/// is zero-allocation.
fn apply_result(
    results: &FlatReadResults<'_>,
    index: usize,
    address: usize,
    dst: &mut [u8],
) -> Result<(), MemoryError> {
    let (kind, bytes) = results.get(index);
    match kind {
        vosu_read_result_kind::VOSU_READ_OK => {
            if bytes.len() != dst.len() {
                return Err(MemoryError::Backend {
                    operation: "read plan op",
                    message: format!(
                        "host returned {} bytes for a {}-byte op",
                        bytes.len(),
                        dst.len()
                    ),
                });
            }
            dst.copy_from_slice(bytes);
            Ok(())
        }
        vosu_read_result_kind::VOSU_READ_NOT_READY => Err(MemoryError::Read {
            address,
            len: dst.len(),
            message: "target is not ready".to_owned(),
        }),
        vosu_read_result_kind::VOSU_READ_UNSUPPORTED => Err(MemoryError::Backend {
            operation: "read plan op",
            message: "host does not support this read".to_owned(),
        }),
        vosu_read_result_kind::VOSU_READ_ERROR => {
            let message = String::from_utf8_lossy(bytes).into_owned();
            match results.error_code(index) {
                Some(vosu_read_error_code::VOSU_READ_ERROR_PERMISSION_DENIED) => {
                    Err(MemoryError::PermissionDenied {
                        address,
                        len: dst.len(),
                        message,
                    })
                }
                _ => Err(MemoryError::Read {
                    address,
                    len: dst.len(),
                    message,
                }),
            }
        }
    }
}

impl<H: HostServices + Send + Sync + 'static> ProcessMemory for PlanProcessMemory<H> {
    /// Root `Absolute(address)`, one op: splits into several sequential
    /// plans once `dst` is longer than `MAX_SINGLE_OP_BYTES`.
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let mut filled = 0usize;
        while filled < dst.len() {
            let chunk_len = (dst.len() - filled).min(MAX_SINGLE_OP_BYTES);
            let chunk_address =
                address
                    .checked_add(filled)
                    .ok_or(MemoryError::AddressOverflow {
                        base: address,
                        offset: filled,
                    })?;
            let root_address =
                u64::try_from(chunk_address).map_err(|_| MemoryError::AddressOverflow {
                    base: chunk_address,
                    offset: 0,
                })?;
            let op = PlanOp {
                offset: 0,
                length: u32::try_from(chunk_len).expect("chunk_len is capped well under u32::MAX"),
            };
            let dst_chunk = &mut dst[filled..filled + chunk_len];
            self.submit(PlanBase::Absolute(root_address), &[op], |index, results| {
                apply_result(results, index, chunk_address, dst_chunk)
            })?;
            filled += chunk_len;
        }
        Ok(())
    }

    /// Root `Absolute(0)`, one stage, one op per request with
    /// `offset: address` — batched into as many ops as fit one plan's 64
    /// KiB result cap before starting the next round trip, using a
    /// thread-local `Vec<PlanOp>` scratch buffer instead of a fresh one
    /// per batch. A single request longer than `MAX_SINGLE_OP_BYTES`
    /// falls back to [`Self::read_exact`]'s own splitting instead of ever
    /// being batched.
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        let mut index = 0;
        while index < reads.len() {
            if reads[index].dst.len() > MAX_SINGLE_OP_BYTES {
                let address = reads[index].address;
                self.read_exact(address, reads[index].dst)?;
                index += 1;
                continue;
            }

            let mut end = index;
            let mut result_budget = RESULT_HEADER_BYTES;
            let mut submit_budget = SUBMIT_HEADER_BYTES + STAGE_HEADER_BYTES;
            while end < reads.len() {
                let len = reads[end].dst.len();
                if len > MAX_SINGLE_OP_BYTES {
                    break;
                }
                let next_result = result_budget + RESULT_ENTRY_BYTES + len;
                let next_submit = submit_budget + OP_BYTES;
                if next_result > PLAN_CAP || next_submit > PLAN_CAP {
                    break;
                }
                result_budget = next_result;
                submit_budget = next_submit;
                end += 1;
            }
            debug_assert!(
                end > index,
                "a request within MAX_SINGLE_OP_BYTES always fits alone"
            );

            OPS_SCRATCH.with(|cell| {
                let mut ops = cell.borrow_mut();
                ops.clear();
                for read in &reads[index..end] {
                    let offset =
                        i64::try_from(read.address).map_err(|_| MemoryError::AddressOverflow {
                            base: read.address,
                            offset: 0,
                        })?;
                    ops.push(PlanOp {
                        offset,
                        length: u32::try_from(read.dst.len())
                            .expect("read_many chunk lengths are capped well under u32::MAX"),
                    });
                }

                let batch = &mut reads[index..end];
                self.submit(PlanBase::Absolute(0), &ops, |op_index, results| {
                    let address = batch[op_index].address;
                    apply_result(results, op_index, address, batch[op_index].dst)
                })
            })?;
            index = end;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Err(MemoryError::Backend {
            operation: "readable_regions",
            message: "not available through read plans".to_owned(),
        })
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests;
