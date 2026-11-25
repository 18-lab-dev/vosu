//! F4 review P2: `PlanMemory::read_flat`'s `SYNC_RAW_TARGET` thread-local
//! holds a raw pointer at the caller's own `SyncReadBuffers::result` for
//! the duration of the reentrant `host.send_flat` call. `HostServices` is
//! a public, safe, overridable trait, so a panic inside an implementation
//! is something safe code must tolerate — if the unwind is caught
//! somewhere above `read_flat`'s own stack frame (as this test does)
//! instead of propagating out of the process, the old code left
//! `SYNC_RAW_TARGET` pointing at a buffer that can since be dropped and
//! its memory reused. The next matching `VOSU_OP_READ_PLAN_RESULT` on
//! this thread (even one meant for a later, unrelated `PlanMemory::read`
//! call: the generated dispatch glue always tries the raw intercept
//! first, for every `VOSU_OP_READ_PLAN_RESULT`, regardless of which of
//! `read`/`read_flat` is actually running) would then make
//! `intercept_raw` write through that dangling pointer — a use-after-free
//! from entirely safe code. Run under miri to actually observe the UB
//! this guards against (`cargo +nightly miri test -p vosu-plugin-internal
//! --test plan_memory_unwind`).

use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::sync::Mutex;

use vosu_plugin_api::{
    HostServices, Init, Message, PlanBase, Plugin, PluginError, ReadResult, StageRef,
    VtableHostServices, vosu_bytes, vosu_host_services, vosu_message, vosu_opcode, vosu_status,
};
use vosu_plugin_internal::{PlanMemory, SyncReadBuffers};

struct TestPlugin;

impl Plugin for TestPlugin {
    fn create(_host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        panic!("a synchronous VOSU_OP_READ_PLAN_RESULT must never reach on_message: {message:?}");
    }
}

unsafe extern "C" fn noop_send(_host_ctx: *mut c_void, _message: *const vosu_message) -> u32 {
    vosu_status::VOSU_OK as u32
}

fn noop_host_services() -> vosu_host_services {
    vosu_host_services {
        struct_size: core::mem::size_of::<vosu_host_services>() as u32,
        abi_version: vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR,
        send: Some(noop_send),
    }
}

fn init_bytes() -> Vec<u8> {
    vosu_plugin_api::encode(&Init {
        plugin_id: "test.plan_memory_unwind".to_owned(),
        settings: vosu_plugin_api::CborValue::Null,
        data_dir: std::path::PathBuf::from("/tmp"),
    })
    .expect("encode Init")
}

/// Panics unconditionally from `send_flat`, standing in for any
/// `HostServices` implementation that can unwind mid-call (a test
/// double, or a real host hitting an internal bug) — `read_flat` must
/// stay unwind-safe regardless of why the panic happened.
struct PanicOnSendFlat;

impl HostServices for PanicOnSendFlat {
    fn send(&self, _message: &Message, _correlation_id: u64) -> vosu_status {
        panic!("this test only ever goes through send_flat");
    }

    fn send_flat(
        &self,
        _opcode: vosu_opcode,
        _flags: u32,
        _correlation_id: u64,
        _payload: &[u8],
    ) -> vosu_status {
        panic!("send_flat panics unconditionally for this test");
    }
}

/// Answers a `VOSU_READ_PLAN_SYNC` submission through the ordinary typed
/// `send`/`PlanMemory::read` path, reentrant exactly as an in-process
/// host's real delivery is (mirrors `sdk.rs`'s own `SyncAnsweringHost`).
struct TypedAnsweringHost {
    plugin_ctx: *mut c_void,
    dispatch: unsafe extern "C" fn(*mut c_void, *const vosu_message) -> u32,
}

impl HostServices for TypedAnsweringHost {
    fn send(&self, message: &Message, _correlation_id: u64) -> vosu_status {
        let Message::ReadPlanSubmit(submit) = message else {
            panic!("test host only answers ReadPlanSubmit");
        };
        let result = Message::ReadPlanResult(vosu_plugin_api::ReadPlanResult {
            plan: submit.plan,
            results: vec![ReadResult::Ok(vec![9, 9, 9, 9])],
        });
        let payload = result.encode_payload().expect("encode");
        let raw = vosu_message {
            opcode: vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
            flags: 0,
            correlation_id: 0,
            payload: vosu_bytes {
                data: payload.as_ptr(),
                len: payload.len() as u32,
            },
        };
        // SAFETY: `self.plugin_ctx` is a live context from a matching
        // `create` call, and `raw` is a well-formed, live `vosu_message`.
        let raw_status = unsafe { (self.dispatch)(self.plugin_ctx, &raw as *const vosu_message) };
        vosu_status::try_from(raw_status).unwrap_or(vosu_status::VOSU_ERR_BACKEND)
    }
}

/// P2 (F4 review): a panic inside `send_flat`, caught above `read_flat`'s
/// own stack frame, must not leave `SYNC_RAW_TARGET` dangling at the
/// dropped `SyncReadBuffers` it pointed into — a later, unrelated
/// `PlanMemory::read` call on the same thread (using the *same* plan id,
/// so it would match if `SYNC_RAW_TARGET` were still set) must still
/// complete correctly instead of writing through that stale pointer.
#[test]
fn unwinding_out_of_read_flat_does_not_leave_a_dangling_sync_raw_target() {
    let vt = vosu_plugin_internal::vtable_for::<TestPlugin>();
    let noop = noop_host_services();
    let init = init_bytes();
    // SAFETY: `noop`/`init` are valid for this call's duration; `vt` is a
    // freshly built vtable for `TestPlugin`.
    let plugin_ctx = unsafe {
        (vt.create.unwrap())(
            &noop as *const vosu_host_services,
            core::ptr::null_mut(),
            vosu_bytes {
                data: init.as_ptr(),
                len: init.len() as u32,
            },
        )
    };
    assert!(!plugin_ctx.is_null());

    let plan_memory = PlanMemory::new();
    const PLAN: u64 = 77;

    {
        let mut buffers = SyncReadBuffers::new();
        let stages: [StageRef<'_>; 0] = [];
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            plan_memory
                .read_flat(
                    &PanicOnSendFlat,
                    PLAN,
                    1,
                    PlanBase::Absolute(0),
                    &stages,
                    &mut buffers,
                )
                .map(|results| results.len())
        }));
        assert!(
            outcome.is_err(),
            "send_flat's panic must propagate out of read_flat, not be swallowed"
        );
        // `buffers` is dropped here, at the end of this block: `result`
        // is still empty (`send_flat` panicked before ever writing to
        // it, so there is no heap allocation of its own to free), but
        // `buffers`' own stack slot — what `SYNC_RAW_TARGET`'s raw
        // pointer actually points into — is gone once this block exits
        // and that stack space is reused. Without the P2 fix,
        // `SYNC_RAW_TARGET` would still point there.
    }

    // A later, unrelated `read` call on this same thread, reusing the
    // same plan id: with the fix, `SYNC_RAW_TARGET` is `None` (cleared by
    // `read_flat`'s own drop guard on unwind, and defensively by `read`
    // itself), so the real, typed `intercept` handles this result
    // exactly as if the panicked `read_flat` call never happened.
    let host = TypedAnsweringHost {
        plugin_ctx,
        dispatch: vt.dispatch.unwrap(),
    };
    let result = plan_memory
        .read(&host, PLAN, 1, PlanBase::Absolute(0), vec![])
        .expect("a later read on the same thread must still succeed");
    assert_eq!(result.plan, PLAN);
    assert_eq!(result.results, vec![ReadResult::Ok(vec![9, 9, 9, 9])]);

    // SAFETY: `plugin_ctx` was created by `vt.create` above and is not
    // used again after this.
    unsafe { (vt.destroy.unwrap())(plugin_ctx) };
}

/// Records every `Message::ReadPlanResult` it is dispatched, instead of
/// panicking the way `TestPlugin` does: the guard-only regression below
/// delivers one on purpose, through a raw `dispatch` call standing in
/// for a host's own raw `send` — never through `PlanMemory` — so this
/// plugin's own `on_message` seeing it, and being handed the right
/// payload, is exactly what proves `intercept_raw` correctly reported
/// "not mine" instead of writing through a stale `SYNC_RAW_TARGET`.
struct RecordingPlugin;

static RECORDED_RESULTS: Mutex<Vec<vosu_plugin_api::ReadPlanResult>> = Mutex::new(Vec::new());

impl Plugin for RecordingPlugin {
    fn create(_host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        if let Message::ReadPlanResult(result) = message {
            RECORDED_RESULTS
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(result);
        }
        Ok(())
    }
}

/// ReviewF4fix's own P3: the previous version of this file only proved
/// `read`'s own defensive `SYNC_RAW_TARGET` reset was enough by itself
/// (it always is, for a later call that goes through `PlanMemory`) —
/// never the one case only `SyncRawGuard` covers: a later
/// `VOSU_OP_READ_PLAN_RESULT` delivered through a raw `send`/`dispatch`
/// call that never goes through `PlanMemory` at all (any C plugin, or an
/// SDK plugin's own host reusing a plan id some other way), reusing the
/// panicked `read_flat` call's own plan id. Without `SyncRawGuard`
/// (`EXPECTING_PLAN` still `Some(PLAN)`, `SYNC_RAW_TARGET` still
/// dangling), `intercept_raw` would match this delivery and write
/// through that dangling pointer instead of ever reaching `on_message`.
#[test]
fn unwinding_out_of_read_flat_does_not_leave_a_dangling_target_for_a_later_raw_send() {
    let vt = vosu_plugin_internal::vtable_for::<RecordingPlugin>();
    let noop = noop_host_services();
    let init = vosu_plugin_api::encode(&Init {
        plugin_id: "test.plan_memory_unwind.recording".to_owned(),
        settings: vosu_plugin_api::CborValue::Null,
        data_dir: std::path::PathBuf::from("/tmp"),
    })
    .expect("encode Init");
    // SAFETY: `noop`/`init` are valid for this call's duration; `vt` is a
    // freshly built vtable for `RecordingPlugin`.
    let plugin_ctx = unsafe {
        (vt.create.unwrap())(
            &noop as *const vosu_host_services,
            core::ptr::null_mut(),
            vosu_bytes {
                data: init.as_ptr(),
                len: init.len() as u32,
            },
        )
    };
    assert!(!plugin_ctx.is_null());

    let plan_memory = PlanMemory::new();
    const PLAN: u64 = 55;

    {
        let mut buffers = SyncReadBuffers::new();
        let stages: [StageRef<'_>; 0] = [];
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            plan_memory
                .read_flat(
                    &PanicOnSendFlat,
                    PLAN,
                    1,
                    PlanBase::Absolute(0),
                    &stages,
                    &mut buffers,
                )
                .map(|results| results.len())
        }));
        assert!(
            outcome.is_err(),
            "send_flat's panic must propagate out of read_flat, not be swallowed"
        );
        // `buffers`' own stack slot — what `SYNC_RAW_TARGET` points into
        // — is gone once this block exits. Without `SyncRawGuard`,
        // `SYNC_RAW_TARGET` would still point there, and `EXPECTING_PLAN`
        // would still be `Some(PLAN)`.
    }

    // A later `VOSU_OP_READ_PLAN_RESULT` reusing `PLAN`'s id, delivered
    // through a raw `dispatch` call — standing in for a host's own raw
    // `send`, never `PlanMemory::read`/`read_flat` — on this same
    // thread. Built directly, not through any `HostServices`: nothing
    // here is "waiting" on this plan the way `PlanMemory` tracks it.
    let result = vosu_plugin_api::ReadPlanResult {
        plan: PLAN,
        results: vec![ReadResult::Ok(vec![5, 5, 5, 5])],
    };
    let payload = Message::ReadPlanResult(result.clone())
        .encode_payload()
        .expect("encode");
    let raw = vosu_message {
        opcode: vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
        flags: 0,
        correlation_id: 0,
        payload: vosu_bytes {
            data: payload.as_ptr(),
            len: payload.len() as u32,
        },
    };
    // SAFETY: `plugin_ctx` is a live context from the matching `create`
    // call above, and `raw` is a well-formed, live `vosu_message`.
    let status = unsafe { (vt.dispatch.unwrap())(plugin_ctx, &raw as *const vosu_message) };
    assert_eq!(
        status,
        vosu_status::VOSU_OK as u32,
        "a plain, unexpected ReadPlanResult must dispatch normally to on_message"
    );
    let recorded = RECORDED_RESULTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .pop();
    assert_eq!(
        recorded,
        Some(result),
        "on_message must actually receive this result — intercept_raw must report \"not \
         mine\" for it, not swallow it by writing through a stale SYNC_RAW_TARGET"
    );

    // SAFETY: `plugin_ctx` was created by `vt.create` above and is not
    // used again after this.
    unsafe { (vt.destroy.unwrap())(plugin_ctx) };
}
