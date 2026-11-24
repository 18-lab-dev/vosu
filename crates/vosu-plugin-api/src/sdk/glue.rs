//! Generic `create`/`dispatch`/`destroy` glue monomorphized per plugin
//! type, and `export_plugin!` (`docs/v2/plugin-abi.md` §9, D22).
//!
//! Every function here crosses the FFI boundary (or is called only from
//! code that does), so a panic inside plugin code must never unwind past
//! it: each one runs the plugin call inside
//! `std::panic::catch_unwind`, converting an unwind into the same
//! "rejected"/"absent" outcome a well-behaved plugin would have returned.

use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;

use crate::message::Message;
use crate::opcode::vosu_opcode;
use crate::sdk::plugin::{ConcurrentPlugin, Init, Plugin, VtableHostServices};
use crate::{VOSU_PLUGIN_ABI_MAJOR, vosu_bytes, vosu_host_services, vosu_message, vosu_status};

fn decode_init(init: vosu_bytes) -> Result<Init, ()> {
    if init.data.is_null() {
        return Err(());
    }
    // SAFETY: `init` is `create`'s borrowed `vosu_bytes` view, valid for
    // the duration of this call per `vosu_message`'s own contract.
    let bytes = unsafe { core::slice::from_raw_parts(init.data, init.len as usize) };
    crate::decode(bytes).map_err(|_| ())
}

fn decode_message(message: *const vosu_message) -> Result<(Message, u64), vosu_status> {
    if message.is_null() {
        return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT);
    }
    // SAFETY: `dispatch`'s `message` is borrowed for the duration of this
    // call (`include/vosu_plugin.h:493`).
    let raw = unsafe { &*message };
    let Ok(opcode) = vosu_opcode::try_from(raw.opcode) else {
        return Err(vosu_status::VOSU_ERR_UNSUPPORTED);
    };
    let payload = if raw.payload.data.is_null() {
        &[][..]
    } else {
        // SAFETY: `raw.payload` is call-scoped, per `vosu_bytes`'s own doc
        // comment.
        unsafe { core::slice::from_raw_parts(raw.payload.data, raw.payload.len as usize) }
    };
    let message = Message::decode(opcode, raw.flags, payload)
        .map_err(|_| vosu_status::VOSU_ERR_INVALID_ARGUMENT)?;
    Ok((message, raw.correlation_id))
}

/// Checked before [`decode_message`] on every `dispatch` call, behind the
/// unpublished `unstable-sync-reads` feature: if `message` is a
/// `VOSU_OP_READ_PLAN_RESULT` this thread's own [`PlanMemory::read_flat`]
/// call is waiting for, hands its still-raw payload straight to
/// `sdk::plan_memory::intercept_raw` and reports "handled" — no
/// `Message::decode` (and so no owned `ReadPlanResult`/`Vec<ReadResult>`)
/// ever runs for it (D17). `None` for anything else (including a
/// `VOSU_OP_READ_PLAN_RESULT` this thread was not expecting, or one an
/// ordinary [`PlanMemory::read`] call is waiting for instead), so
/// `dispatch` falls through to the normal decode-and-forward path, which
/// `run_dispatch`'s own (typed) `intercept` still covers.
///
/// [`PlanMemory::read_flat`]: crate::sdk::plan_memory::PlanMemory::read_flat
/// [`PlanMemory::read`]: crate::sdk::plan_memory::PlanMemory::read
#[cfg(feature = "unstable-sync-reads")]
fn try_intercept_sync_raw(message: *const vosu_message) -> Option<vosu_status> {
    if message.is_null() {
        return None;
    }
    // SAFETY: see `decode_message`'s identical borrow of `message` for the
    // duration of this call.
    let raw = unsafe { &*message };
    if vosu_opcode::try_from(raw.opcode) != Ok(vosu_opcode::VOSU_OP_READ_PLAN_RESULT) {
        return None;
    }
    if raw.payload.data.is_null() {
        return None;
    }
    // SAFETY: see `decode_message`'s identical borrow of `raw.payload`.
    let payload =
        unsafe { core::slice::from_raw_parts(raw.payload.data, raw.payload.len as usize) };
    crate::sdk::plan_memory::intercept_raw(payload).then_some(vosu_status::VOSU_OK)
}

/// `Plugin`'s serialized context: `Mutex` is this SDK's own lock
/// (`docs/v2/plugin-abi.md` §9), independent of whatever the manifest's
/// `concurrent_dispatch` declares.
struct SerializedContext<T> {
    inner: Mutex<T>,
}

pub extern "C" fn create_serialized<T: Plugin>(
    host: *const vosu_host_services,
    host_ctx: *mut c_void,
    init: vosu_bytes,
) -> *mut c_void {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let init = decode_init(init).ok()?;
        // SAFETY: `create`'s doc comment (`include/vosu_plugin.h:467`)
        // promises `host`/`host_ctx` outlive every call made through them
        // until `destroy` returns, exactly `VtableHostServices::new`'s
        // contract.
        let services = unsafe { VtableHostServices::new(host, host_ctx) };
        T::create(services, init).ok()
    }));
    match result {
        Ok(Some(plugin)) => {
            let boxed = Box::new(SerializedContext {
                inner: Mutex::new(plugin),
            });
            Box::into_raw(boxed).cast::<c_void>()
        }
        _ => core::ptr::null_mut(),
    }
}

/// Runs one decoded message, plus its `correlation_id`, through `call`,
/// under `catch_unwind`, after first giving the `unstable-sync-reads`
/// feature's `PlanMemory` a chance to intercept a
/// `VOSU_OP_READ_PLAN_RESULT` meant for it (D5) — see
/// `sdk::plan_memory`'s module doc comment for why this has to happen
/// here, before `call` (the plugin's own `on_message`) ever sees it.
fn run_dispatch(
    decoded: Message,
    correlation_id: u64,
    call: impl FnOnce(Message, u64) -> Result<(), crate::sdk::plugin::PluginError>,
) -> vosu_status {
    #[cfg(feature = "unstable-sync-reads")]
    if let Message::ReadPlanResult(result) = &decoded
        && crate::sdk::plan_memory::intercept(result)
    {
        return vosu_status::VOSU_OK;
    }
    #[cfg(feature = "unstable-sync-reads")]
    crate::sdk::plan_memory::ON_DISPATCH_THREAD.with(|cell| cell.set(true));
    let result = panic::catch_unwind(AssertUnwindSafe(|| call(decoded, correlation_id)));
    #[cfg(feature = "unstable-sync-reads")]
    crate::sdk::plan_memory::ON_DISPATCH_THREAD.with(|cell| cell.set(false));
    match result {
        Ok(Ok(())) => vosu_status::VOSU_OK,
        Ok(Err(err)) => err.0,
        Err(_) => vosu_status::VOSU_ERR_BACKEND,
    }
}

pub extern "C" fn dispatch_serialized<T: Plugin>(
    plugin_ctx: *mut c_void,
    message: *const vosu_message,
) -> u32 {
    if plugin_ctx.is_null() {
        return vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32;
    }
    #[cfg(feature = "unstable-sync-reads")]
    if let Some(status) = try_intercept_sync_raw(message) {
        return status as u32;
    }
    let (decoded, correlation_id) = match decode_message(message) {
        Ok(decoded) => decoded,
        Err(status) => return status as u32,
    };
    // SAFETY: `plugin_ctx` is the pointer `create_serialized::<T>` returned,
    // which the host passes back unchanged on every later call
    // (`include/vosu_plugin.h:493`); it stays valid until `destroy`
    // consumes it.
    let context = unsafe { &*plugin_ctx.cast::<SerializedContext<T>>() };
    run_dispatch(decoded, correlation_id, |message, correlation_id| {
        let mut guard = context
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        guard.on_message(message, correlation_id)
    }) as u32
}

pub extern "C" fn destroy_serialized<T: Plugin>(plugin_ctx: *mut c_void) {
    if plugin_ctx.is_null() {
        return;
    }
    // SAFETY: the host calls `destroy` exactly once, after which
    // `plugin_ctx` is never passed to another call
    // (`include/vosu_plugin.h:495-498`), so reclaiming the `Box` here is
    // the one place that may.
    let boxed = unsafe { Box::from_raw(plugin_ctx.cast::<SerializedContext<T>>()) };
    let _ = panic::catch_unwind(AssertUnwindSafe(|| drop(boxed)));
}

pub extern "C" fn create_concurrent<T: ConcurrentPlugin>(
    host: *const vosu_host_services,
    host_ctx: *mut c_void,
    init: vosu_bytes,
) -> *mut c_void {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        let init = decode_init(init).ok()?;
        // SAFETY: see `create_serialized`'s identical contract.
        let services = unsafe { VtableHostServices::new(host, host_ctx) };
        T::create(services, init).ok()
    }));
    match result {
        Ok(Some(plugin)) => Box::into_raw(Box::new(plugin)).cast::<c_void>(),
        _ => core::ptr::null_mut(),
    }
}

pub extern "C" fn dispatch_concurrent<T: ConcurrentPlugin>(
    plugin_ctx: *mut c_void,
    message: *const vosu_message,
) -> u32 {
    if plugin_ctx.is_null() {
        return vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32;
    }
    #[cfg(feature = "unstable-sync-reads")]
    if let Some(status) = try_intercept_sync_raw(message) {
        return status as u32;
    }
    let (decoded, correlation_id) = match decode_message(message) {
        Ok(decoded) => decoded,
        Err(status) => return status as u32,
    };
    // SAFETY: see `dispatch_serialized`'s identical contract.
    let plugin = unsafe { &*plugin_ctx.cast::<T>() };
    run_dispatch(decoded, correlation_id, |message, correlation_id| {
        plugin.on_message(message, correlation_id)
    }) as u32
}

pub extern "C" fn destroy_concurrent<T: ConcurrentPlugin>(plugin_ctx: *mut c_void) {
    if plugin_ctx.is_null() {
        return;
    }
    // SAFETY: see `destroy_serialized`'s identical contract.
    let boxed = unsafe { Box::from_raw(plugin_ctx.cast::<T>()) };
    let _ = panic::catch_unwind(AssertUnwindSafe(|| drop(boxed)));
}

/// Builds the `vosu_plugin_vtable` for a serialized `Plugin` type `T`.
pub fn vtable_for<T: Plugin>() -> crate::vosu_plugin_vtable {
    crate::vosu_plugin_vtable {
        struct_size: core::mem::size_of::<crate::vosu_plugin_vtable>() as u32,
        abi_version: VOSU_PLUGIN_ABI_MAJOR,
        create: Some(create_serialized::<T>),
        dispatch: Some(dispatch_serialized::<T>),
        destroy: Some(destroy_serialized::<T>),
    }
}

/// Builds the `vosu_plugin_vtable` for a `ConcurrentPlugin` type `T`.
pub fn vtable_for_concurrent<T: ConcurrentPlugin>() -> crate::vosu_plugin_vtable {
    crate::vosu_plugin_vtable {
        struct_size: core::mem::size_of::<crate::vosu_plugin_vtable>() as u32,
        abi_version: VOSU_PLUGIN_ABI_MAJOR,
        create: Some(create_concurrent::<T>),
        dispatch: Some(dispatch_concurrent::<T>),
        destroy: Some(destroy_concurrent::<T>),
    }
}

/// One macro call generates either a plain `vosu_plugin_vtable()` function
/// for the root crate's explicit `BUNDLED` array (D13) to call directly,
/// or `vosu_plugin_entry_v1` for an external `cdylib` build — the call
/// site says which, explicitly, rather than a `#[cfg(feature = "bundled")]`
/// inside this `macro_rules!`: that `cfg` is evaluated in the crate that
/// invokes the macro, so a downstream crate without its own `bundled`
/// feature would get an `unexpected_cfgs` warning (`-D warnings` failure)
/// for a feature it never declared, and a bundled plugin crate that forgot
/// to enable it would silently export a `#[no_mangle] vosu_plugin_entry_v1`
/// instead (M2) — two such crates linked into one executable would then
/// collide.
///
/// Write `export_plugin!(bundled MyPlugin)` for the bundled form,
/// `export_plugin!(MyPlugin)` for the `cdylib` form, each for a type
/// implementing [`Plugin`] (serialized, `&mut self`); prefix either with
/// `concurrent` for one implementing [`ConcurrentPlugin`] instead (`&self`,
/// D22) — `export_plugin!(bundled concurrent MyPlugin)` or
/// `export_plugin!(concurrent MyPlugin)`. This differs from
/// `docs/v2/plugin-abi.md` §9's sketch, which shows one spelling for both
/// `Plugin`/`ConcurrentPlugin` and says the macro "picks the dispatch path
/// from which trait the plugin's type implements": Rust has no stable,
/// non-fragile way for one `macro_rules!` invocation to query which trait
/// an arbitrary type implements, so this SDK asks the plugin author to say
/// which once, explicitly, at the call site instead.
#[macro_export]
macro_rules! export_plugin {
    (bundled concurrent $ty:ty) => {
        pub fn vosu_plugin_vtable() -> $crate::vosu_plugin_vtable {
            $crate::__vtable_for_concurrent::<$ty>()
        }
    };
    (bundled $ty:ty) => {
        pub fn vosu_plugin_vtable() -> $crate::vosu_plugin_vtable {
            $crate::__vtable_for_serialized::<$ty>()
        }
    };
    (concurrent $ty:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn vosu_plugin_entry_v1() -> *const $crate::vosu_plugin_vtable {
            static VTABLE: ::std::sync::LazyLock<$crate::vosu_plugin_vtable> =
                ::std::sync::LazyLock::new($crate::__vtable_for_concurrent::<$ty>);
            &*VTABLE
        }
    };
    ($ty:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn vosu_plugin_entry_v1() -> *const $crate::vosu_plugin_vtable {
            static VTABLE: ::std::sync::LazyLock<$crate::vosu_plugin_vtable> =
                ::std::sync::LazyLock::new($crate::__vtable_for_serialized::<$ty>);
            &*VTABLE
        }
    };
}
