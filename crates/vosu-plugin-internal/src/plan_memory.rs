//! `PlanMemory`: opt-in synchronous reads (D5), workspace-only (AF.9).
//!
//! `VOSU_READ_PLAN_SYNC` makes the host deliver the matching
//! `VOSU_OP_READ_PLAN_RESULT` back on the submitting thread itself, before
//! `send` returns (`docs/v2/plugin-abi.md` §6.3) — for an in-process
//! plugin, by calling the plugin's own `dispatch` directly, reentrant on
//! this very call stack. `PlanMemory` routes that one message to a
//! thread-local slot before the plugin's own `on_message` ever sees it
//! (this crate's dispatch glue, `crate::glue`, checks this slot first, so
//! a plugin using `PlanMemory` registers through this crate's
//! `export_plugin!`), then retrieves it here once
//! `send` returns — exactly "the SDK routes flagged results to a per-plan
//! slot before user code sees them" (`docs/v2/implementation-plan.md`
//! D5). It refuses to run from a thread the ordinary, serialized
//! `on_message` is itself running on (the dispatch glue marks that thread
//! for the call's duration): submitting a synchronous plan there would
//! either deadlock (a `process`-runtime plugin's single dispatch thread)
//! or at best race the host's own documented refusal
//! (`include/vosu_plugin.h:396-399`).

use std::cell::Cell;

use vosu_plugin_api::{
    HostServices, Message, PlanBase, ReadPlanResult, ReadPlanSubmit, Stage, StageRef,
    encode_read_plan_submit_into, read_flat_header, validate_flags, validate_read_plan_result,
    vosu_opcode, vosu_plan_id, vosu_read_error_code, vosu_read_plan_result_header,
    vosu_read_result_flat, vosu_read_result_kind, vosu_session_id, vosu_status,
};

thread_local! {
    /// Set for the duration of an ordinary (non-synchronous) dispatch call
    /// on this thread, by this crate's dispatch glue (`crate::glue`).
    pub(crate) static ON_DISPATCH_THREAD: Cell<bool> = const { Cell::new(false) };
    /// The plan this thread is waiting on a synchronous result for, if any.
    /// Shared by [`PlanMemory::read`] and [`PlanMemory::read_flat`]: both
    /// expect at most one outstanding synchronous plan per thread at a
    /// time, and [`intercept_raw`] checks it for both.
    static EXPECTING_PLAN: Cell<Option<u64>> = const { Cell::new(None) };
    /// Where the dispatch glue stashes that result once it arrives,
    /// instead of forwarding it to the plugin's own `on_message`.
    /// [`PlanMemory::read`]'s own (allocating) slot.
    static SYNC_SLOT: Cell<Option<ReadPlanResult>> = const { Cell::new(None) };
    /// [`PlanMemory::read_flat`]'s own zero-allocation counterpart of
    /// `SYNC_SLOT`: a raw pointer at the caller's own
    /// `SyncReadBuffers::result`, valid only for the duration of one
    /// `read_flat` call on this thread (set just before, cleared again
    /// just after, the reentrant `send_flat` call it wraps).
    static SYNC_RAW_TARGET: Cell<Option<*mut Vec<u8>>> = const { Cell::new(None) };
    /// Whether [`intercept_raw`] found a match and filled `SYNC_RAW_TARGET`
    /// during the current `read_flat` call.
    static SYNC_RAW_PRESENT: Cell<bool> = const { Cell::new(false) };
}

/// Called by this crate's dispatch glue (`crate::glue`) for every
/// `VOSU_OP_READ_PLAN_RESULT` before the public SDK glue decodes it:
/// reads `payload`'s `plan` field directly out of the still-raw
/// `vosu_read_plan_result_header` (a `Copy` struct read, no allocation)
/// and reports "not mine" (`None`, so the ordinary decode-and-forward path
/// runs and the plugin's own `on_message` sees it) unless it matches this
/// thread's outstanding synchronous plan (`EXPECTING_PLAN`).
///
/// A match for [`PlanMemory::read_flat`] (a target buffer is waiting in
/// `SYNC_RAW_TARGET`) is validated exactly as `Message::decode` would
/// (`validate_flags`, `validate_read_plan_result` — both allocation-free)
/// before anything is copied into that target: a `HostServices`
/// implementation is a public, safe trait a test double (or a misbehaving
/// external host) can feed anything to, and `FlatReadResults::get`'s own
/// unchecked pointer arithmetic depends on this buffer already being
/// well-formed. A match for an ordinary [`PlanMemory::read`] call (no
/// target buffer) is decoded with `Message::decode`, the same validation
/// the ordinary dispatch path applies, and stashed in `SYNC_SLOT`; only
/// that call's own result is ever decoded here, never an unrelated one.
///
/// Returns `None` ("not mine"), `Some(VOSU_OK)` (matched and stored), or
/// `Some(VOSU_ERR_INVALID_ARGUMENT)` (matched this plan but failed
/// validation — never stored, so [`SYNC_RAW_TARGET`]/[`SYNC_RAW_PRESENT`]/
/// `SYNC_SLOT` are left exactly as both read paths already handle for "no
/// result").
pub(crate) fn intercept_raw(payload: &[u8], flags: u32) -> Option<vosu_status> {
    let expected = EXPECTING_PLAN.with(Cell::get)?;
    let header: vosu_read_plan_result_header = read_flat_header(payload).ok()?;
    if header.plan != expected {
        return None;
    }
    let Some(target) = SYNC_RAW_TARGET.with(Cell::get) else {
        return Some(
            match Message::decode(vosu_opcode::VOSU_OP_READ_PLAN_RESULT, flags, payload) {
                Ok(Message::ReadPlanResult(result)) => {
                    SYNC_SLOT.with(|slot| slot.set(Some(result)));
                    vosu_status::VOSU_OK
                }
                _ => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
            },
        );
    };
    // This message is unambiguously meant for this thread's `read_flat`
    // call now (the plan id matched); validate it before trusting
    // anything past the header.
    if validate_flags(vosu_opcode::VOSU_OP_READ_PLAN_RESULT, flags).is_err()
        || validate_read_plan_result(payload).is_err()
    {
        return Some(vosu_status::VOSU_ERR_INVALID_ARGUMENT);
    }
    // SAFETY: `target` was set by this same thread's own `read_flat` stack
    // frame (`SYNC_RAW_TARGET`'s own doc comment), for the duration of
    // this reentrant call only, and cleared again before that frame
    // returns — this function only ever runs reentrant inside that one
    // window, on this very thread.
    let buf = unsafe { &mut *target };
    buf.clear();
    buf.extend_from_slice(payload);
    SYNC_RAW_PRESENT.with(|present| present.set(true));
    Some(vosu_status::VOSU_OK)
}

/// Every way [`PlanMemory::read`] can fail without ever submitting a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanMemoryError {
    /// This call would run on the host's own serialized dispatch thread
    /// (`docs/v2/plugin-abi.md` §4); a plugin's own reader thread is
    /// required instead.
    OnDispatchThread,
    /// `send` rejected the submission itself.
    Rejected(vosu_status),
    /// `send` returned `VOSU_OK` but no synchronous result arrived — this
    /// SDK's own bug, since the host guarantees delivery before `send`
    /// returns, or `VOSU_ERR_TIMED_OUT` otherwise.
    NoResult,
}

/// A plugin's own reader-thread handle for submitting a `VOSU_READ_PLAN_SYNC`
/// read plan and blocking for its result inline (D5).
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanMemory;

impl PlanMemory {
    pub fn new() -> Self {
        Self
    }

    /// Submits `root`/`stages` as plan `plan` for `handle` with
    /// `VOSU_READ_PLAN_SYNC` set, and returns its result once `host.send`
    /// returns — on this calling thread, never the host's own dispatch
    /// thread (refused with [`PlanMemoryError::OnDispatchThread`] before
    /// even submitting, mirroring the host's own refusal,
    /// `include/vosu_plugin.h:396-399`).
    pub fn read(
        &self,
        host: &impl HostServices,
        plan: vosu_plan_id,
        handle: vosu_session_id,
        root: PlanBase,
        stages: Vec<Stage>,
    ) -> Result<ReadPlanResult, PlanMemoryError> {
        if ON_DISPATCH_THREAD.with(Cell::get) {
            return Err(PlanMemoryError::OnDispatchThread);
        }
        EXPECTING_PLAN.with(|cell| cell.set(Some(plan)));
        SYNC_SLOT.with(|slot| slot.set(None));
        // P2 (F4 review): a prior `read_flat` call on this thread whose
        // `host.send_flat` panicked, caught elsewhere, could otherwise
        // leave `SYNC_RAW_TARGET` pointing at a `SyncReadBuffers::result`
        // that may since have been dropped or reused. The generated
        // dispatch glue always tries the raw path first for every
        // `VOSU_OP_READ_PLAN_RESULT` (`crate::glue`), whichever
        // of `read`/`read_flat` this thread is actually running, so a
        // stale `Some` here would make `intercept_raw` treat this call's
        // own result as "mine" too and write through that dangling
        // pointer. `read_flat`'s own `SyncRawGuard` now keeps this clear
        // on its own exit paths, including unwind; this reset is this
        // function's own defense regardless of whether that guard ran.
        SYNC_RAW_TARGET.with(|cell| cell.set(None));
        // The one sanctioned synchronous submission (M5): a `sync` plan sent
        // any other way would reach `on_message` like any other message,
        // which §6.3 says never happens.
        let status = host.send(
            &Message::ReadPlanSubmit(ReadPlanSubmit {
                plan,
                handle,
                root,
                stages,
                sync: true,
            }),
            0,
        );
        EXPECTING_PLAN.with(|cell| cell.set(None));
        if status != vosu_status::VOSU_OK {
            return Err(PlanMemoryError::Rejected(status));
        }
        SYNC_SLOT
            .with(|slot| slot.take())
            .ok_or(PlanMemoryError::NoResult)
    }

    /// [`Self::read`]'s zero-allocation counterpart (D17): encodes
    /// `root`/`stages` into `buffers.submit` with
    /// [`encode_read_plan_submit_into`] (no owned
    /// `ReadPlanSubmit`/`Stage`/`Vec` intermediate), submits it through
    /// [`HostServices::send_flat`] (no `Message` enum round trip either),
    /// and returns a [`FlatReadResults`] borrowing `buffers.result` — the
    /// raw `VOSU_OP_READ_PLAN_RESULT` payload `intercept_raw` copied
    /// into it, decoded on demand by [`FlatReadResults::get`] rather than
    /// up front. Reusing the same `buffers` across repeated same-shaped
    /// plans on this reader thread allocates nothing once both of its
    /// buffers have grown to their steady-state capacity. Same
    /// dispatch-thread refusal as [`Self::read`].
    pub fn read_flat<'b>(
        &self,
        host: &impl HostServices,
        plan: vosu_plan_id,
        handle: vosu_session_id,
        root: PlanBase,
        stages: &[StageRef<'_>],
        buffers: &'b mut SyncReadBuffers,
    ) -> Result<FlatReadResults<'b>, PlanMemoryError> {
        if ON_DISPATCH_THREAD.with(Cell::get) {
            return Err(PlanMemoryError::OnDispatchThread);
        }
        {
            let _encode_profile =
                crate::read_plan_profile::Span::new(crate::read_plan_profile::Phase::SdkEncode);
            encode_read_plan_submit_into(&mut buffers.submit, plan, handle, &root, stages);
        }

        EXPECTING_PLAN.with(|cell| cell.set(Some(plan)));
        SYNC_RAW_PRESENT.with(|present| present.set(false));
        // SAFETY: this raw pointer is used only by `intercept_raw`,
        // reentrant on this very thread inside the `send_flat` call right
        // below, and is cleared again before this function returns, on
        // every exit path including an unwind caught elsewhere
        // (`SyncRawGuard`, constructed right after this) —
        // `SYNC_RAW_TARGET`'s own doc comment.
        SYNC_RAW_TARGET.with(|cell| cell.set(Some(&raw mut buffers.result)));
        // P2 (F4 review): `host` is a public, safe trait; if
        // `send_flat` panics and the unwind is caught elsewhere instead of
        // propagating out of this process, this guard's `Drop` still
        // clears `EXPECTING_PLAN`/`SYNC_RAW_TARGET` before this stack
        // frame is gone — without it, `SYNC_RAW_TARGET` would keep
        // pointing at this call's own `buffers.result` after `buffers`
        // may since have been dropped or reused, and the next matching
        // `VOSU_OP_READ_PLAN_RESULT` on this thread would write through
        // that dangling pointer from `intercept_raw` (UB from safe code).
        let _guard = SyncRawGuard;
        let status = host.send_flat(
            vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT,
            vosu_plugin_api::VOSU_READ_PLAN_SYNC,
            0,
            &buffers.submit,
        );
        drop(_guard);

        if status != vosu_status::VOSU_OK {
            return Err(PlanMemoryError::Rejected(status));
        }
        if !SYNC_RAW_PRESENT.with(Cell::get) {
            return Err(PlanMemoryError::NoResult);
        }
        let header: vosu_read_plan_result_header =
            read_flat_header(&buffers.result).map_err(|_| PlanMemoryError::NoResult)?;
        Ok(FlatReadResults {
            buffer: &buffers.result,
            results_offset: header.results_offset,
            count: header.result_count as usize,
        })
    }
}

/// Clears [`EXPECTING_PLAN`]/[`SYNC_RAW_TARGET`] when dropped, including
/// on unwind (F4 review, P2): `host.send_flat` is a call into a public,
/// safe, overridable trait method, so [`PlanMemory::read_flat`] cannot
/// assume it never panics. Without this, a panic caught above
/// `read_flat`'s own stack frame would leave `SYNC_RAW_TARGET` holding a
/// raw pointer at a `SyncReadBuffers::result` that may since have been
/// dropped or reused — the next matching `VOSU_OP_READ_PLAN_RESULT` on
/// this thread would then make `intercept_raw` write through it.
struct SyncRawGuard;

impl Drop for SyncRawGuard {
    fn drop(&mut self) {
        EXPECTING_PLAN.with(|cell| cell.set(None));
        SYNC_RAW_TARGET.with(|cell| cell.set(None));
    }
}

/// Reusable per-reader-thread scratch buffers for [`PlanMemory::read_flat`]
/// (D17): `submit` holds the flat `VOSU_OP_READ_PLAN_SUBMIT` payload
/// [`encode_read_plan_submit_into`] builds; `result` holds the
/// raw `VOSU_OP_READ_PLAN_RESULT` payload `intercept_raw` copies into
/// it. Neither ever shrinks on its own, so a reader thread that keeps
/// reusing the same `SyncReadBuffers` across repeated same-shaped plans
/// allocates nothing once both have grown to their steady-state capacity.
#[derive(Debug, Default)]
pub struct SyncReadBuffers {
    submit: Vec<u8>,
    result: Vec<u8>,
}

impl SyncReadBuffers {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Borrows `SyncReadBuffers::result`: one [`PlanMemory::read_flat`]
/// call's results, in the same (stage, op) order as the matching
/// submission, decoded on demand by [`Self::get`] — no owned
/// `ReadResult`/`Vec<ReadResult>` is ever built.
pub struct FlatReadResults<'b> {
    buffer: &'b [u8],
    results_offset: u32,
    count: usize,
}

impl<'b> FlatReadResults<'b> {
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Reads one flat `vosu_read_result_flat` entry (a plain `Copy`
    /// struct, no allocation). Panics if `index >= self.len()`; shared by
    /// [`Self::get`] and [`Self::error_code`] so both read the same
    /// validated bytes the same way.
    #[inline]
    fn entry(&self, index: usize) -> vosu_read_result_flat {
        assert!(index < self.count, "FlatReadResults index out of bounds");
        let elem_size = core::mem::size_of::<vosu_read_result_flat>();
        let offset = self.results_offset as usize + index * elem_size;
        // SAFETY: `read_flat` only ever builds this type from
        // `buffers.result`, and the only thing that ever writes to that
        // buffer is `intercept_raw`, which calls
        // `vosu_plugin_api::validate_read_plan_result` on `payload` and
        // refuses to copy it in (returning `VOSU_ERR_INVALID_ARGUMENT`
        // instead) on any validation failure — so this buffer is always
        // the validated one by the time `read_flat` ever constructs a
        // `FlatReadResults` from it. `index < self.count` was just
        // asserted above, so `offset` names a `vosu_read_result_flat`-
        // sized range inside that validated `results` array.
        unsafe {
            self.buffer
                .as_ptr()
                .add(offset)
                .cast::<vosu_read_result_flat>()
                .read_unaligned()
        }
    }

    /// One flat `vosu_read_result_flat` entry's kind, plus its trailing
    /// bytes (the read bytes for `VOSU_READ_OK`, the error message for
    /// `VOSU_READ_ERROR`, empty otherwise). Panics if
    /// `index >= self.len()`.
    #[inline]
    pub fn get(&self, index: usize) -> (vosu_read_result_kind, &'b [u8]) {
        let entry = self.entry(index);
        let kind = vosu_read_result_kind::try_from(entry.kind)
            .expect("a host-produced VOSU_OP_READ_PLAN_RESULT always carries a valid kind");
        let bytes = match kind {
            vosu_read_result_kind::VOSU_READ_OK => {
                &self.buffer
                    [entry.bytes_offset as usize..(entry.bytes_offset + entry.bytes_len) as usize]
            }
            vosu_read_result_kind::VOSU_READ_ERROR => {
                &self.buffer[entry.error_message_offset as usize
                    ..(entry.error_message_offset + entry.error_message_len) as usize]
            }
            vosu_read_result_kind::VOSU_READ_NOT_READY
            | vosu_read_result_kind::VOSU_READ_UNSUPPORTED => &[][..],
        };
        (kind, bytes)
    }

    /// `index`'s `vosu_read_error_code`, or `None` when its kind is not
    /// `VOSU_READ_ERROR` (`include/vosu_plugin.h:412-420`'s own per-op
    /// error vocabulary, mirroring `src/process.rs`'s `MemoryError`
    /// subset): lets a caller recover `MemoryError::PermissionDenied`
    /// instead of [`Self::get`]'s generic error-message bytes alone.
    /// Panics if `index >= self.len()`.
    pub fn error_code(&self, index: usize) -> Option<vosu_read_error_code> {
        let entry = self.entry(index);
        if vosu_read_result_kind::try_from(entry.kind) != Ok(vosu_read_result_kind::VOSU_READ_ERROR)
        {
            return None;
        }
        Some(
            vosu_read_error_code::try_from(entry.error_code)
                .expect("a host-produced VOSU_READ_ERROR entry always carries a valid error_code"),
        )
    }
}
