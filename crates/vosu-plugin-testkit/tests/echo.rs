//! Exercises the bundled-form `EchoPlugin`/`ConcurrentEchoPlugin` vtables
//! directly: `create`, `dispatch` an echoed message back through a fake
//! `vosu_host_services::send`, and `destroy`.
#![cfg(feature = "test-harness")]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};

use vosu_plugin_api::{Init, vosu_bytes, vosu_host_services, vosu_message, vosu_status};

fn init_bytes() -> Vec<u8> {
    vosu_plugin_api::encode(&Init {
        plugin_id: "vosu.testkit.echo".into(),
        settings: vosu_plugin_api::CborValue::Map(vec![]),
        data_dir: std::env::temp_dir(),
    })
    .expect("encode init")
}

/// Counts `send` calls through `host_ctx` rather than a shared `static`, so
/// concurrently run tests in this file never race on the same counter.
unsafe extern "C" fn counting_send(host_ctx: *mut c_void, _message: *const vosu_message) -> u32 {
    // SAFETY: `host_ctx` is always this test's live `&AtomicU32` for the
    // duration of the call that passed it.
    let calls = unsafe { &*host_ctx.cast::<AtomicU32>() };
    calls.fetch_add(1, Ordering::SeqCst);
    vosu_status::VOSU_OK as u32
}

fn counting_host_services() -> vosu_host_services {
    vosu_host_services {
        struct_size: core::mem::size_of::<vosu_host_services>() as u32,
        abi_version: vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR,
        send: Some(counting_send),
    }
}

fn focus_changed_payload(handle: u64, focused: bool) -> Vec<u8> {
    vosu_plugin_api::encode(&vosu_plugin_api::FocusChanged { handle, focused }).unwrap()
}

#[test]
fn bundled_echo_plugin_creates_dispatches_and_echoes() {
    let vtable = vosu_plugin_testkit::echo::vosu_plugin_vtable();
    assert_eq!(vtable.abi_version, vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR);

    let host = counting_host_services();
    let calls = AtomicU32::new(0);
    let init = init_bytes();
    // SAFETY: `host` and `calls` are local values that outlive every call
    // made through them until `destroy` returns; `init` is a valid encoded
    // `Init` and its bytes outlive the call.
    let plugin_ctx = unsafe {
        (vtable.create.unwrap())(
            &host as *const vosu_host_services,
            (&calls as *const AtomicU32).cast_mut().cast::<c_void>(),
            vosu_bytes {
                data: init.as_ptr(),
                len: init.len() as u32,
            },
        )
    };
    assert!(!plugin_ctx.is_null());

    let payload = focus_changed_payload(7, true);
    let message = vosu_message {
        opcode: vosu_plugin_api::vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32,
        flags: 0,
        correlation_id: 0,
        payload: vosu_bytes {
            data: payload.as_ptr(),
            len: payload.len() as u32,
        },
    };
    // SAFETY: `plugin_ctx` is live; `message`'s payload outlives this call.
    let status = unsafe { (vtable.dispatch.unwrap())(plugin_ctx, &message as *const vosu_message) };
    assert_eq!(status, vosu_status::VOSU_OK as u32);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "echo plugin did not send back the dispatched message"
    );

    // SAFETY: `plugin_ctx` is destroyed exactly once, here.
    unsafe { (vtable.destroy.unwrap())(plugin_ctx) };
}

#[test]
fn bundled_concurrent_echo_plugin_creates_dispatches_and_echoes() {
    let vtable = vosu_plugin_testkit::concurrent::vosu_plugin_vtable();
    let host = counting_host_services();
    let calls = AtomicU32::new(0);
    let init = init_bytes();
    // SAFETY: see the serialized test above.
    let plugin_ctx = unsafe {
        (vtable.create.unwrap())(
            &host as *const vosu_host_services,
            (&calls as *const AtomicU32).cast_mut().cast::<c_void>(),
            vosu_bytes {
                data: init.as_ptr(),
                len: init.len() as u32,
            },
        )
    };
    assert!(!plugin_ctx.is_null());

    let payload = focus_changed_payload(9, false);
    let message = vosu_message {
        opcode: vosu_plugin_api::vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32,
        flags: 0,
        correlation_id: 0,
        payload: vosu_bytes {
            data: payload.as_ptr(),
            len: payload.len() as u32,
        },
    };
    // SAFETY: see the serialized test above.
    let status = unsafe { (vtable.dispatch.unwrap())(plugin_ctx, &message as *const vosu_message) };
    assert_eq!(status, vosu_status::VOSU_OK as u32);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "echo plugin did not send back the dispatched message"
    );

    // SAFETY: `plugin_ctx` is destroyed exactly once, here.
    unsafe { (vtable.destroy.unwrap())(plugin_ctx) };
}
