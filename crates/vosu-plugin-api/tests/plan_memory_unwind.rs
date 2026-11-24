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
//! this guards against (`cargo +nightly miri test -p vosu-plugin-api
//! --test plan_memory_unwind`).

use std::ffi::c_void;
use std::panic::AssertUnwindSafe;

use vosu_plugin_api::{
    HostServices, Init, Message, PlanBase, PlanMemory, Plugin, PluginError, ReadResult, StageRef,
    SyncReadBuffers, VtableHostServices, vosu_bytes, vosu_host_services, vosu_message, vosu_opcode,
    vosu_status,
};

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
    let vt = vosu_plugin_api::__vtable_for_serialized::<TestPlugin>();
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
        // `buffers` is dropped here, at the end of this block: its
        // `result` Vec's heap allocation is freed. Without the P2 fix,
        // `SYNC_RAW_TARGET` would still point into it.
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
