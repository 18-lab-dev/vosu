//! F4 review P1: the raw `VOSU_READ_PLAN_SYNC` intercept
//! (`sdk::plan_memory::intercept_raw`/`sdk::glue::try_intercept_sync_raw`)
//! must validate a `VOSU_OP_READ_PLAN_RESULT` payload exactly as
//! `Message::decode` would — `validate_flags` and
//! `validate_read_plan_result`, both allocation-free — before ever
//! copying it into `PlanMemory::read_flat`'s own `SyncReadBuffers::result`
//! and building a `FlatReadResults` from it. `FlatReadResults::get`'s own
//! `unsafe` pointer arithmetic trusts that buffer completely (no bounds
//! check of its own beyond the `index < self.len()` assert), so any of
//! these malformed payloads reaching it unvalidated would be an
//! out-of-bounds read or a panic inside `read_unaligned` — UB from a
//! safe public API, under a `panic = "abort"` release profile (root
//! `Cargo.toml:182`).
//!
//! Every test here drives the real generated `dispatch` (via
//! `MalformedHost::send_flat`, reentrant exactly as an in-process host's
//! real delivery is) with a hand-built, deliberately malformed
//! `VOSU_OP_READ_PLAN_RESULT` payload matching the submitted plan's own
//! id, and asserts `PlanMemory::read_flat` rejects it — never reaching
//! `FlatReadResults::get` at all, so there is nothing left to read out of
//! bounds. Run under miri when available (`cargo +nightly miri test -p
//! vosu-plugin-api --test plan_memory_validation`); without it, these
//! tests at least confirm every case returns `Err` instead of panicking
//! or reading garbage.

use std::cell::Cell;
use std::ffi::c_void;

use vosu_plugin_api::{
    HostServices, Init, Message, PlanBase, PlanMemory, PlanMemoryError, Plugin, PluginError,
    StageRef, SyncReadBuffers, read_flat_header, vosu_bytes, vosu_host_services, vosu_message,
    vosu_opcode, vosu_read_plan_submit_header, vosu_status,
};

struct TestPlugin;

impl Plugin for TestPlugin {
    fn create(
        _host: vosu_plugin_api::VtableHostServices,
        _init: Init,
    ) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        panic!(
            "a synchronous VOSU_OP_READ_PLAN_RESULT, malformed or not, must never reach \
             on_message: {message:?}"
        );
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
        plugin_id: "test.plan_memory_validation".to_owned(),
        settings: vosu_plugin_api::CborValue::Null,
        data_dir: std::path::PathBuf::from("/tmp"),
    })
    .expect("encode Init")
}

fn push_struct<T: Copy>(buf: &mut Vec<u8>, value: &T) {
    // SAFETY: every `T` this is called with is a `#[repr(C)]` flat header
    // struct of plain integers with no padding this byte read could
    // expose uninitialized memory from.
    let bytes = unsafe {
        core::slice::from_raw_parts((value as *const T).cast::<u8>(), core::mem::size_of::<T>())
    };
    buf.extend_from_slice(bytes);
}

/// Delivers a caller-built `VOSU_OP_READ_PLAN_RESULT` payload (`build`),
/// given the submitted plan's own id, through the real generated
/// `dispatch`, reentrant from `send_flat` exactly as an in-process host's
/// real `VOSU_READ_PLAN_SYNC` delivery does — never a well-formed one
/// this file builds itself, unlike `plan_memory_alloc.rs`'s `FakeHost`.
struct MalformedHost {
    plugin_ctx: Cell<*mut c_void>,
    dispatch: Cell<Option<unsafe extern "C" fn(*mut c_void, *const vosu_message) -> u32>>,
    flags: u32,
    build: Box<dyn Fn(u64) -> Vec<u8>>,
}

impl HostServices for MalformedHost {
    fn send(&self, _message: &Message, _correlation_id: u64) -> vosu_status {
        panic!("this test only ever goes through send_flat");
    }

    fn send_flat(
        &self,
        opcode: vosu_opcode,
        _flags: u32,
        _correlation_id: u64,
        payload: &[u8],
    ) -> vosu_status {
        assert_eq!(opcode, vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT);
        let header: vosu_read_plan_submit_header =
            read_flat_header(payload).expect("well-formed submit header");
        let result_payload = (self.build)(header.plan);
        let raw = vosu_message {
            opcode: vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
            flags: self.flags,
            correlation_id: 0,
            payload: vosu_bytes {
                data: result_payload.as_ptr(),
                len: result_payload.len() as u32,
            },
        };
        let dispatch = self.dispatch.get().expect("dispatch set before use");
        // SAFETY: `self.plugin_ctx` is a live context from a matching
        // `create` call, `dispatch` is that same plugin's own generated
        // `dispatch` function, and `raw` stays valid for this call
        // (`result_payload` outlives it).
        let raw_status = unsafe { dispatch(self.plugin_ctx.get(), &raw as *const vosu_message) };
        vosu_status::try_from(raw_status).unwrap_or(vosu_status::VOSU_ERR_BACKEND)
    }
}

/// Builds the plugin, runs one `read_flat` call against a `MalformedHost`
/// answering with `build(plan)`/`flags`, and returns `read_flat`'s own
/// outcome. Destroys the plugin afterward either way: a rejected
/// (`Err`) call must leave the plugin just as destroyable as a normal
/// one — this guards against `MalformedHost`/the intercept path leaving
/// the plugin's own context in some half-torn-down state.
fn try_read_flat_against(
    flags: u32,
    build: impl Fn(u64) -> Vec<u8> + 'static,
) -> Result<usize, PlanMemoryError> {
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

    let host = MalformedHost {
        plugin_ctx: Cell::new(plugin_ctx),
        dispatch: Cell::new(vt.dispatch),
        flags,
        build: Box::new(build),
    };
    let plan_memory = PlanMemory::new();
    let mut buffers = SyncReadBuffers::new();
    let stages: [StageRef<'_>; 0] = [];
    let result = plan_memory
        .read_flat(
            &host,
            1,
            1,
            PlanBase::Absolute(0x1000),
            &stages,
            &mut buffers,
        )
        .map(|results| results.len());

    // SAFETY: `plugin_ctx` was created by `vt.create` above and is not
    // used again after this.
    unsafe { (vt.destroy.unwrap())(plugin_ctx) };
    result
}

/// A payload too short to even hold a `vosu_read_plan_result_header`:
/// already refused by `intercept_raw`'s own `read_flat_header` call
/// (unchanged by the P1 fix), falling through to `Message::decode`,
/// which rejects it the same way.
#[test]
fn short_payload_is_rejected_with_no_oob_read() {
    let result = try_read_flat_against(0, |_plan| vec![1, 2, 3]);
    assert_eq!(
        result,
        Err(PlanMemoryError::Rejected(
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        ))
    );
}

/// A well-formed header naming a `results_offset`/`result_count` pair
/// that runs past the end of the buffer: without the P1 fix,
/// `FlatReadResults::get` would read a `vosu_read_result_flat` out of
/// bounds the first time anything called it.
#[test]
fn out_of_range_results_array_is_rejected_with_no_oob_read() {
    let result = try_read_flat_against(0, |plan| {
        let mut buf = Vec::new();
        push_struct(
            &mut buf,
            &vosu_plugin_api::vosu_read_plan_result_header {
                plan,
                // Claims 4 results starting right at the end of this
                // (header-only) buffer: `validate_array_range` must
                // reject this before anything indexes into it.
                results_offset: core::mem::size_of::<vosu_plugin_api::vosu_read_plan_result_header>(
                ) as u32,
                result_count: 4,
            },
        );
        buf
    });
    assert_eq!(
        result,
        Err(PlanMemoryError::Rejected(
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        ))
    );
}

/// One result entry whose `kind` is not a valid `vosu_read_result_kind`
/// discriminant: without the P1 fix, `FlatReadResults::get` panics on
/// `vosu_read_result_kind::try_from(entry.kind).expect(..)` instead of
/// this call ever being reached.
#[test]
fn bad_kind_is_rejected_with_no_oob_read() {
    let result = try_read_flat_against(0, |plan| {
        let mut buf = Vec::new();
        let header_size =
            core::mem::size_of::<vosu_plugin_api::vosu_read_plan_result_header>() as u32;
        push_struct(
            &mut buf,
            &vosu_plugin_api::vosu_read_plan_result_header {
                plan,
                results_offset: header_size,
                result_count: 1,
            },
        );
        push_struct(
            &mut buf,
            &vosu_plugin_api::vosu_read_result_flat {
                kind: 0xFFFF_FFFF,
                bytes_offset: 0,
                bytes_len: 0,
                error_code: 0,
                error_message_offset: 0,
                error_message_len: 0,
            },
        );
        buf
    });
    assert_eq!(
        result,
        Err(PlanMemoryError::Rejected(
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        ))
    );
}

/// One `VOSU_READ_OK` entry whose `bytes_offset + bytes_len` overflows
/// `u32`: without the P1 fix, `FlatReadResults::get`'s
/// `entry.bytes_offset..(entry.bytes_offset + entry.bytes_len)` slice
/// range would itself overflow-panic (debug) or wrap to a bogus,
/// out-of-bounds range (release) the moment anything called it.
#[test]
fn overflowing_bytes_range_is_rejected_with_no_oob_read() {
    let result = try_read_flat_against(0, |plan| {
        let mut buf = Vec::new();
        let header_size =
            core::mem::size_of::<vosu_plugin_api::vosu_read_plan_result_header>() as u32;
        push_struct(
            &mut buf,
            &vosu_plugin_api::vosu_read_plan_result_header {
                plan,
                results_offset: header_size,
                result_count: 1,
            },
        );
        push_struct(
            &mut buf,
            &vosu_plugin_api::vosu_read_result_flat {
                kind: vosu_plugin_api::vosu_read_result_kind::VOSU_READ_OK as u32,
                bytes_offset: u32::MAX - 1,
                bytes_len: 10,
                error_code: 0,
                error_message_offset: 0,
                error_message_len: 0,
            },
        );
        buf
    });
    assert_eq!(
        result,
        Err(PlanMemoryError::Rejected(
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        ))
    );
}

/// An otherwise well-formed, empty result (`result_count: 0`) delivered
/// with a nonzero `flags`: `VOSU_OP_READ_PLAN_RESULT` recognizes no flag
/// at all (only `VOSU_OP_READ_PLAN_SUBMIT`'s own `VOSU_READ_PLAN_SYNC`
/// bit is ever valid on any opcode), so `validate_flags` must reject this
/// before the (otherwise harmless) payload is ever copied in.
#[test]
fn nonzero_flags_on_a_result_is_rejected() {
    let result = try_read_flat_against(1, |plan| {
        let mut buf = Vec::new();
        push_struct(
            &mut buf,
            &vosu_plugin_api::vosu_read_plan_result_header {
                plan,
                results_offset: core::mem::size_of::<vosu_plugin_api::vosu_read_plan_result_header>(
                ) as u32,
                result_count: 0,
            },
        );
        buf
    });
    assert_eq!(
        result,
        Err(PlanMemoryError::Rejected(
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        ))
    );
}
