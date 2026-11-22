//! `export_plugin!`'s two call-site forms (M2): the bundled form
//! (`export_plugin!(bundled MyPlugin)`), a plain `vosu_plugin_vtable()`
//! function for the root crate's explicit `BUNDLED` array, and the
//! `cdylib` form (`export_plugin!(MyPlugin)`), a `#[no_mangle]
//! vosu_plugin_entry_v1`. Both must compile from the same call site syntax
//! regardless of whether this crate's own `bundled` Cargo feature is
//! enabled — that feature no longer gates which one `export_plugin!`
//! expands to.
//!
//! This is a separate test binary (rather than another `mod` in
//! `tests/sdk.rs`) because the `cdylib` form's `#[no_mangle]
//! vosu_plugin_entry_v1` is a single process-wide symbol: `tests/sdk.rs`
//! already defines one for its own plugin type, so a second one in the
//! same binary would collide at link time.

use std::ffi::c_void;

use vosu_plugin_api::{
    Init, Message, Plugin, PluginError, VtableHostServices, export_plugin, vosu_bytes,
    vosu_host_services, vosu_status,
};

struct BundledTestPlugin;

impl Plugin for BundledTestPlugin {
    fn create(_host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn on_message(&mut self, _message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        Ok(())
    }
}

export_plugin!(bundled BundledTestPlugin);

struct CdylibTestPlugin;

impl Plugin for CdylibTestPlugin {
    fn create(_host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn on_message(&mut self, _message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        Ok(())
    }
}

export_plugin!(CdylibTestPlugin);

fn init_bytes() -> Vec<u8> {
    vosu_plugin_api::encode(&Init {
        plugin_id: "test.plugin".into(),
        settings: vosu_plugin_api::CborValue::Map(vec![]),
        data_dir: "/tmp/test-plugin".into(),
    })
    .expect("encode init")
}

unsafe extern "C" fn noop_send(
    _host_ctx: *mut c_void,
    _message: *const vosu_plugin_api::vosu_message,
) -> u32 {
    vosu_status::VOSU_OK as u32
}

fn noop_host_services() -> vosu_host_services {
    vosu_host_services {
        struct_size: core::mem::size_of::<vosu_host_services>() as u32,
        abi_version: vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR,
        send: Some(noop_send),
    }
}

#[test]
fn bundled_form_produces_a_plain_vtable_function() {
    let vt = vosu_plugin_vtable();
    assert_eq!(vt.abi_version, vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR);
    assert!(vt.create.is_some());

    let host = noop_host_services();
    let init = init_bytes();
    // SAFETY: `host` is a local, well-formed `vosu_host_services` that
    // outlives this call; `init` is a valid encoded `Init` and its bytes
    // outlive the call.
    let plugin_ctx = unsafe {
        (vt.create.unwrap())(
            &host as *const vosu_host_services,
            core::ptr::null_mut(),
            vosu_bytes {
                data: init.as_ptr(),
                len: init.len() as u32,
            },
        )
    };
    assert!(!plugin_ctx.is_null());
    // SAFETY: `plugin_ctx` is destroyed exactly once, here.
    unsafe { (vt.destroy.unwrap())(plugin_ctx) };
}

#[test]
fn cdylib_form_produces_a_no_mangle_entry_point() {
    // SAFETY: `vosu_plugin_entry_v1` always returns a pointer to the
    // `static` `vosu_plugin_vtable` the macro built; it lives for the
    // program's duration.
    let vt = unsafe { &*vosu_plugin_entry_v1() };
    assert_eq!(vt.abi_version, vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR);
    assert!(vt.create.is_some());

    let host = noop_host_services();
    let init = init_bytes();
    // SAFETY: see `bundled_form_produces_a_plain_vtable_function`.
    let plugin_ctx = unsafe {
        (vt.create.unwrap())(
            &host as *const vosu_host_services,
            core::ptr::null_mut(),
            vosu_bytes {
                data: init.as_ptr(),
                len: init.len() as u32,
            },
        )
    };
    assert!(!plugin_ctx.is_null());
    // SAFETY: `plugin_ctx` is destroyed exactly once, here.
    unsafe { (vt.destroy.unwrap())(plugin_ctx) };
}
