//! The workspace's dispatch glue: the public SDK's generic
//! `create`/`dispatch`/`destroy` (`vosu_plugin_api::__vtable_for_*`) with
//! [`PlanMemory`]'s synchronous-result interception (D5) in front of
//! `dispatch`, plus the bundled-registration form of [`export_plugin!`]
//! (D13). AF.9 moved both here so the published SDK carries neither.
//!
//! A synchronous `VOSU_OP_READ_PLAN_RESULT` arrives reentrantly, on the
//! submitting reader thread, inside the host's `send`; it must be claimed
//! before the SDK glue decodes it or takes a [`Plugin`]'s own lock, which
//! is why the interception wraps the whole public `dispatch` rather than
//! running inside `on_message`.
//!
//! [`PlanMemory`]: crate::PlanMemory
//! [`export_plugin!`]: crate::export_plugin

use std::ffi::c_void;

use vosu_plugin_api::{
    ConcurrentPlugin, Plugin, vosu_message, vosu_opcode, vosu_plugin_vtable, vosu_status,
};

use crate::plan_memory::{ON_DISPATCH_THREAD, intercept_raw};

type DispatchFn = unsafe extern "C" fn(*mut c_void, *const vosu_message) -> u32;

/// Checked before the SDK glue's own decode on every `dispatch` call: if
/// `message` is a `VOSU_OP_READ_PLAN_RESULT` this thread's own
/// `PlanMemory` call is waiting for, hands its still-raw payload and flags
/// to `intercept_raw`, which validates and stores it — no
/// `Message::decode` (and so no owned `ReadPlanResult`/`Vec<ReadResult>`)
/// runs for a `PlanMemory::read_flat` result (D17). `None` for anything
/// else, including a `VOSU_OP_READ_PLAN_RESULT` this thread was not
/// expecting, so `dispatch` falls through to the ordinary path.
fn try_intercept_sync(message: *const vosu_message) -> Option<vosu_status> {
    if message.is_null() {
        return None;
    }
    // SAFETY: `dispatch`'s `message` is borrowed for the duration of this
    // call (`include/vosu_plugin.h:493`).
    let raw = unsafe { &*message };
    if vosu_opcode::try_from(raw.opcode) != Ok(vosu_opcode::VOSU_OP_READ_PLAN_RESULT) {
        return None;
    }
    if raw.payload.data.is_null() {
        return None;
    }
    // SAFETY: `raw.payload` is call-scoped, per `vosu_bytes`'s own doc
    // comment.
    let payload =
        unsafe { core::slice::from_raw_parts(raw.payload.data, raw.payload.len as usize) };
    intercept_raw(payload, raw.flags)
}

/// Runs `inner` (the SDK glue's `dispatch` for the same plugin type) for
/// every message `try_intercept_sync` does not claim, with this thread
/// marked as a dispatch thread for the call's duration so `PlanMemory`
/// refuses to submit from inside `on_message` (D5). `inner` never unwinds:
/// the SDK glue runs `on_message` under `catch_unwind`.
fn dispatch_intercepting(
    plugin_ctx: *mut c_void,
    message: *const vosu_message,
    inner: Option<DispatchFn>,
) -> u32 {
    if plugin_ctx.is_null() {
        return vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32;
    }
    if let Some(status) = try_intercept_sync(message) {
        return status as u32;
    }
    let Some(inner) = inner else {
        return vosu_status::VOSU_ERR_UNSUPPORTED as u32;
    };
    ON_DISPATCH_THREAD.with(|cell| cell.set(true));
    // SAFETY: `inner` is the SDK glue's `dispatch` for the very plugin type
    // whose `create` produced `plugin_ctx` (both come from the same
    // `__vtable_for_*::<T>()`), and `plugin_ctx`/`message` are forwarded
    // unchanged under the host's own `dispatch` contract.
    let status = unsafe { inner(plugin_ctx, message) };
    ON_DISPATCH_THREAD.with(|cell| cell.set(false));
    status
}

extern "C" fn dispatch_serialized<T: Plugin>(
    plugin_ctx: *mut c_void,
    message: *const vosu_message,
) -> u32 {
    dispatch_intercepting(
        plugin_ctx,
        message,
        vosu_plugin_api::__vtable_for_serialized::<T>().dispatch,
    )
}

extern "C" fn dispatch_concurrent<T: ConcurrentPlugin>(
    plugin_ctx: *mut c_void,
    message: *const vosu_message,
) -> u32 {
    dispatch_intercepting(
        plugin_ctx,
        message,
        vosu_plugin_api::__vtable_for_concurrent::<T>().dispatch,
    )
}

/// The `vosu_plugin_vtable` for a serialized [`Plugin`] type `T`, with
/// synchronous-read interception in front of its `dispatch`.
pub fn vtable_for<T: Plugin>() -> vosu_plugin_vtable {
    vosu_plugin_vtable {
        dispatch: Some(dispatch_serialized::<T>),
        ..vosu_plugin_api::__vtable_for_serialized::<T>()
    }
}

/// The `vosu_plugin_vtable` for a [`ConcurrentPlugin`] type `T`, with
/// synchronous-read interception in front of its `dispatch`.
pub fn vtable_for_concurrent<T: ConcurrentPlugin>() -> vosu_plugin_vtable {
    vosu_plugin_vtable {
        dispatch: Some(dispatch_concurrent::<T>),
        ..vosu_plugin_api::__vtable_for_concurrent::<T>()
    }
}

/// Registers a workspace plugin through this crate's intercepting vtable.
///
/// - `export_plugin!(bundled MyPlugin)` / `export_plugin!(bundled
///   concurrent MyPlugin)`: a plain `pub fn vosu_plugin_vtable()` for the
///   root crate's explicit `BUNDLED` array (D13) to call directly.
/// - `export_plugin!(MyPlugin)` / `export_plugin!(concurrent MyPlugin)`:
///   `vosu_plugin_entry_v1` for a workspace `cdylib` that uses
///   [`PlanMemory`](crate::PlanMemory) under the process runtime (the
///   osu! process fixture). A plugin without synchronous reads uses the
///   published `vosu_plugin_api::export_plugin!` instead.
///
/// The call site names the form explicitly rather than a `cfg` inside the
/// macro: a `cfg` would be evaluated in the invoking crate, and a bundled
/// crate that forgot it would silently export a `#[no_mangle]` symbol that
/// collides with another bundled plugin's in one executable (M2).
#[macro_export]
macro_rules! export_plugin {
    (bundled concurrent $ty:ty) => {
        pub fn vosu_plugin_vtable() -> $crate::__api::vosu_plugin_vtable {
            $crate::vtable_for_concurrent::<$ty>()
        }
    };
    (bundled $ty:ty) => {
        pub fn vosu_plugin_vtable() -> $crate::__api::vosu_plugin_vtable {
            $crate::vtable_for::<$ty>()
        }
    };
    (concurrent $ty:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn vosu_plugin_entry_v1() -> *const $crate::__api::vosu_plugin_vtable {
            static VTABLE: ::std::sync::LazyLock<$crate::__api::vosu_plugin_vtable> =
                ::std::sync::LazyLock::new($crate::vtable_for_concurrent::<$ty>);
            &*VTABLE
        }
    };
    ($ty:ty) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn vosu_plugin_entry_v1() -> *const $crate::__api::vosu_plugin_vtable {
            static VTABLE: ::std::sync::LazyLock<$crate::__api::vosu_plugin_vtable> =
                ::std::sync::LazyLock::new($crate::vtable_for::<$ty>);
            &*VTABLE
        }
    };
}
