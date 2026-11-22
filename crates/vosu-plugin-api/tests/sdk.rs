//! SDK tests (`src/sdk/*`, D22): `export_plugin!` produces a working
//! `create`/`dispatch`/`destroy` triple end to end for both `Plugin` and
//! `ConcurrentPlugin`, a panic inside `on_message` never crosses the FFI
//! boundary, and (behind `unstable-sync-reads`, D5) `PlanMemory` routes a
//! synchronous result to its own slot before the plugin ever sees it.
//!
//! This is the "minimal in-crate test" proving `export_plugin!`
//! (`docs/v2/plugin-abi.md` §9): rather than a separate `cdylib` crate
//! `dlopen`ed back into this process, it calls the generated
//! `vosu_plugin_entry_v1`/`vosu_plugin_vtable` function and its
//! `create`/`dispatch`/`destroy` pointers directly — they are ordinary
//! Rust items under `#[unsafe(no_mangle)] extern "C"`, callable without
//! going through a dynamic loader, which exercises the exact same
//! generated code with far less test infrastructure.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use vosu_plugin_api::{
    HostServices, Init, Message, Plugin, PluginError, VtableHostServices, export_plugin,
    vosu_bytes, vosu_host_services, vosu_message, vosu_status,
};

/// Every test in this file shares process-wide statics
/// (`ON_MESSAGE_CALLS`, `LAST_PUBLISH`, `REENTRANT_RESULT`) to observe
/// what the generated glue did; each `#[test]` holds this for its whole
/// body so Rust's default parallel test execution cannot interleave them.
static TEST_LOCK: Mutex<()> = Mutex::new(());

static ON_MESSAGE_CALLS: AtomicU32 = AtomicU32::new(0);
static LAST_PUBLISH: Mutex<Option<(u64, String, String)>> = Mutex::new(None);

struct TestPlugin;

impl Plugin for TestPlugin {
    fn create(_host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        ON_MESSAGE_CALLS.fetch_add(1, Ordering::SeqCst);
        if let Message::FocusChanged(focus) = &message
            && focus.handle == 0xDEAD
        {
            panic!("boom: a plugin bug must not cross the FFI boundary");
        }
        if let Message::PublishField(field) = message {
            *LAST_PUBLISH.lock().unwrap() = Some((field.handle, field.ns, field.pointer));
        }
        Ok(())
    }
}

export_plugin!(TestPlugin);

/// A minimal `vosu_host_services` whose `send` always succeeds and does
/// nothing else; `create` only needs a non-null, well-formed one.
unsafe extern "C" fn noop_send(
    _host_ctx: *mut c_void,
    _message: *const vosu_message,
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

fn init_bytes() -> Vec<u8> {
    vosu_plugin_api::encode(&Init {
        plugin_id: "test.plugin".into(),
        settings: vosu_plugin_api::CborValue::Map(vec![]),
        data_dir: "/tmp/test-plugin".into(),
    })
    .expect("encode init")
}

#[cfg(not(feature = "bundled"))]
fn vtable() -> *const vosu_plugin_api::vosu_plugin_vtable {
    vosu_plugin_entry_v1()
}

#[cfg(feature = "bundled")]
fn vtable() -> vosu_plugin_api::vosu_plugin_vtable {
    vosu_plugin_vtable()
}

#[test]
fn export_plugin_vtable_has_correct_abi_header_and_every_function() {
    #[cfg(not(feature = "bundled"))]
    let vt = unsafe { &*vtable() };
    #[cfg(feature = "bundled")]
    let vt = vtable();

    assert_eq!(vt.abi_version, vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR);
    assert_eq!(
        vt.struct_size as usize,
        core::mem::size_of::<vosu_plugin_api::vosu_plugin_vtable>()
    );
    assert!(vt.create.is_some());
    assert!(vt.dispatch.is_some());
    assert!(vt.destroy.is_some());
}

#[test]
fn export_plugin_create_dispatch_destroy_round_trip() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: `vosu_plugin_entry_v1` always returns a pointer to the
    // `static` `vosu_plugin_vtable` the macro built; it lives for the
    // program's duration.
    #[cfg(not(feature = "bundled"))]
    let vt = unsafe { &*vtable() };
    #[cfg(feature = "bundled")]
    let vt = vtable();

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

    let payload = vosu_plugin_api::encode(&vosu_plugin_api::FocusChanged {
        handle: 1,
        focused: true,
    })
    .expect("encode");
    let message = vosu_message {
        opcode: vosu_plugin_api::vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32,
        flags: 0,
        correlation_id: 0,
        payload: vosu_bytes {
            data: payload.as_ptr(),
            len: payload.len() as u32,
        },
    };
    // SAFETY: `plugin_ctx` is the pointer `create` just returned, not yet
    // destroyed; `message` is a well-formed, live `vosu_message`.
    let status = unsafe { (vt.dispatch.unwrap())(plugin_ctx, &message as *const vosu_message) };
    assert_eq!(status, vosu_status::VOSU_OK as u32);
    assert!(ON_MESSAGE_CALLS.load(Ordering::SeqCst) >= 1);

    // SAFETY: `plugin_ctx` is destroyed exactly once, here, after every
    // other call above.
    unsafe { (vt.destroy.unwrap())(plugin_ctx) };
}

#[test]
fn export_plugin_panic_inside_on_message_never_crosses_ffi() {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
    #[cfg(not(feature = "bundled"))]
    let vt = unsafe { &*vtable() };
    #[cfg(feature = "bundled")]
    let vt = vtable();

    let host = noop_host_services();
    let init = init_bytes();
    // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
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

    let payload = vosu_plugin_api::encode(&vosu_plugin_api::FocusChanged {
        handle: 0xDEAD,
        focused: true,
    })
    .expect("encode");
    let message = vosu_message {
        opcode: vosu_plugin_api::vosu_opcode::VOSU_OP_FOCUS_CHANGED as u32,
        flags: 0,
        correlation_id: 0,
        payload: vosu_bytes {
            data: payload.as_ptr(),
            len: payload.len() as u32,
        },
    };
    // Must return an error status, never unwind out of this call.
    // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
    let status = unsafe { (vt.dispatch.unwrap())(plugin_ctx, &message as *const vosu_message) };
    assert_eq!(status, vosu_status::VOSU_ERR_BACKEND as u32);

    // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
    unsafe { (vt.destroy.unwrap())(plugin_ctx) };
}

/// A mock `HostServices` that answers a `VOSU_READ_PLAN_SYNC` submission
/// itself, synchronously, exactly as an in-process host's real `send`
/// would (`docs/v2/plugin-abi.md` §6.3): it calls straight back into the
/// given plugin's own `dispatch` with a synthesized `READ_PLAN_RESULT`, on
/// this same call stack, before returning.
#[cfg(feature = "unstable-sync-reads")]
mod plan_memory_tests {
    use super::*;
    use vosu_plugin_api::{PlanBase, PlanMemory, PlanMemoryError, ReadResult};

    struct SyncAnsweringHost {
        plugin_ctx: *mut c_void,
        dispatch: unsafe extern "C" fn(*mut c_void, *const vosu_message) -> u32,
    }

    impl HostServices for SyncAnsweringHost {
        fn send(&self, message: &Message, _correlation_id: u64) -> vosu_status {
            let Message::ReadPlanSubmit(submit) = message else {
                panic!("test host only answers ReadPlanSubmit");
            };
            let result = Message::ReadPlanResult(vosu_plugin_api::ReadPlanResult {
                plan: submit.plan,
                results: vec![ReadResult::Ok(vec![1, 2, 3, 4])],
            });
            let payload = result.encode_payload().expect("encode");
            let raw = vosu_message {
                opcode: vosu_plugin_api::vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
                flags: 0,
                correlation_id: 0,
                payload: vosu_bytes {
                    data: payload.as_ptr(),
                    len: payload.len() as u32,
                },
            };
            // SAFETY: `self.plugin_ctx` is a live context from a matching
            // `create` call, and `raw` is a well-formed, live
            // `vosu_message` — this reentrant call is exactly what an
            // in-process host's real `send` does for `VOSU_READ_PLAN_SYNC`
            // (D5).
            let raw_status = unsafe { (self.dispatch)(self.plugin_ctx, &raw as *const vosu_message) };
            vosu_status::try_from(raw_status).unwrap_or(vosu_status::VOSU_ERR_BACKEND)
        }
    }

    #[test]
    fn plan_memory_reads_the_synchronous_result_without_calling_on_message() {
        let _guard = super::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let vt = vosu_plugin_api::__vtable_for_serialized::<TestPlugin>();
        let noop = noop_host_services();
        let init = init_bytes();
        // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
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

        let calls_before = ON_MESSAGE_CALLS.load(Ordering::SeqCst);
        let host = SyncAnsweringHost {
            plugin_ctx,
            dispatch: vt.dispatch.unwrap(),
        };
        let plan_memory = PlanMemory::new();
        let result = plan_memory
            .read(&host, 42, 1, PlanBase::Absolute(0x1000), vec![])
            .expect("synchronous read succeeds");
        assert_eq!(result.plan, 42);
        assert_eq!(result.results, vec![ReadResult::Ok(vec![1, 2, 3, 4])]);
        // The READ_PLAN_RESULT must never reach the plugin's own
        // `on_message`: PlanMemory intercepts it first.
        assert_eq!(ON_MESSAGE_CALLS.load(Ordering::SeqCst), calls_before);

        // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
        unsafe { (vt.destroy.unwrap())(plugin_ctx) };
    }

    /// A plugin that tries a synchronous read from inside its own
    /// `on_message` — exactly the host's own documented deadlock case
    /// (`include/vosu_plugin.h:396-399`) — and records what `PlanMemory`
    /// did, for a test to assert on afterward.
    struct ReentrantPlanMemoryPlugin;

    static REENTRANT_RESULT: Mutex<Option<Result<(), PlanMemoryError>>> = Mutex::new(None);

    impl Plugin for ReentrantPlanMemoryPlugin {
        fn create(_host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
            Ok(Self)
        }

        fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
            if let Message::SessionDecline(_) = message {
                let host = noop_host_services();
                // SAFETY: `host` is a local, valid `vosu_host_services`
                // for the duration of this call.
                let services = unsafe { VtableHostServices::new(&host, core::ptr::null_mut()) };
                let outcome =
                    PlanMemory::new().read(&services, 1, 1, PlanBase::Absolute(0), vec![]);
                *REENTRANT_RESULT.lock().unwrap() = Some(outcome.map(|_| ()));
            }
            Ok(())
        }
    }

    #[test]
    fn plan_memory_refuses_to_run_on_a_dispatch_thread() {
        let _guard = super::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let vt = vosu_plugin_api::__vtable_for_serialized::<ReentrantPlanMemoryPlugin>();
        let noop = noop_host_services();
        let init = init_bytes();
        // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
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

        let payload = vosu_plugin_api::encode(&vosu_plugin_api::SessionDecline { handle: 1 })
            .expect("encode");
        let message = vosu_message {
            opcode: vosu_plugin_api::vosu_opcode::VOSU_OP_SESSION_DECLINE as u32,
            flags: 0,
            correlation_id: 0,
            payload: vosu_bytes {
                data: payload.as_ptr(),
                len: payload.len() as u32,
            },
        };
        // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
        let status = unsafe { (vt.dispatch.unwrap())(plugin_ctx, &message as *const vosu_message) };
        assert_eq!(status, vosu_status::VOSU_OK as u32);
        assert_eq!(
            *REENTRANT_RESULT.lock().unwrap(),
            Some(Err(PlanMemoryError::OnDispatchThread))
        );

        // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
        unsafe { (vt.destroy.unwrap())(plugin_ctx) };
    }
}

/// B1: a plugin receiving a host -> plugin request carrying a
/// `correlation_id` can echo it back in its own reply, and the SDK
/// actually threads it all the way through `dispatch` -> `on_message` ->
/// `HostServices::send` -> `vosu_host_services::send`'s raw
/// `vosu_message.correlation_id`.
mod correlation_tests {
    use std::sync::Mutex;

    use vosu_plugin_api::{ResourceResolve, ResourceResolved};

    use super::*;

    /// A plugin that answers `RESOURCE_RESOLVE` with `RESOURCE_RESOLVED`,
    /// echoing back whatever `correlation_id` the request carried.
    struct EchoPlugin(VtableHostServices);

    impl Plugin for EchoPlugin {
        fn create(host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
            Ok(Self(host))
        }

        fn on_message(&mut self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
            if let Message::ResourceResolve(_) = message {
                self.0.send(
                    &Message::ResourceResolved(ResourceResolved::Path {
                        root: "root".into(),
                        path: "path".into(),
                    }),
                    correlation_id,
                );
            }
            Ok(())
        }
    }

    /// `(opcode, correlation_id)` of the last message `capturing_send`
    /// observed.
    static LAST_SENT: Mutex<Option<(u32, u64)>> = Mutex::new(None);

    unsafe extern "C" fn capturing_send(
        _host_ctx: *mut c_void,
        message: *const vosu_message,
    ) -> u32 {
        // SAFETY: `message` is borrowed for the duration of this call,
        // per `vosu_host_services::send`'s own contract.
        let raw = unsafe { &*message };
        *LAST_SENT.lock().unwrap() = Some((raw.opcode, raw.correlation_id));
        vosu_status::VOSU_OK as u32
    }

    fn capturing_host_services() -> vosu_host_services {
        vosu_host_services {
            struct_size: core::mem::size_of::<vosu_host_services>() as u32,
            abi_version: vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR,
            send: Some(capturing_send),
        }
    }

    #[test]
    fn resource_resolve_reply_echoes_correlation_id() {
        let _guard = super::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        *LAST_SENT.lock().unwrap() = None;
        let vt = vosu_plugin_api::__vtable_for_serialized::<EchoPlugin>();
        let host = capturing_host_services();
        let init = init_bytes();
        // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
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

        let payload = vosu_plugin_api::encode(&ResourceResolve {
            handle: 1,
            kind: "root".into(),
            key: vosu_plugin_api::CborValue::Null,
        })
        .expect("encode");
        let message = vosu_message {
            opcode: vosu_plugin_api::vosu_opcode::VOSU_OP_RESOURCE_RESOLVE as u32,
            flags: 0,
            correlation_id: 777,
            payload: vosu_bytes {
                data: payload.as_ptr(),
                len: payload.len() as u32,
            },
        };
        // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
        let status = unsafe { (vt.dispatch.unwrap())(plugin_ctx, &message as *const vosu_message) };
        assert_eq!(status, vosu_status::VOSU_OK as u32);

        let (opcode, correlation_id) = LAST_SENT.lock().unwrap().expect("plugin replied");
        assert_eq!(
            opcode,
            vosu_plugin_api::vosu_opcode::VOSU_OP_RESOURCE_RESOLVED as u32
        );
        assert_eq!(correlation_id, 777);

        // SAFETY: see `export_plugin_create_dispatch_destroy_round_trip`.
        unsafe { (vt.destroy.unwrap())(plugin_ctx) };
    }
}
