//! `PlanMemory`'s synchronous-result interception (D5) through this
//! crate's dispatch glue (AF.9): the result of a `VOSU_READ_PLAN_SYNC`
//! submission is routed to `PlanMemory`'s own slot before the plugin's
//! `on_message` ever sees it, for both a serialized `Plugin` and a
//! `ConcurrentPlugin`, and `PlanMemory` refuses to run on a dispatch
//! thread. Moved with `PlanMemory` from `vosu-plugin-api/tests/sdk.rs`.

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use vosu_plugin_api::{
    ConcurrentPlugin, HostServices, Init, Message, Plugin, PluginError, VtableHostServices,
    vosu_bytes, vosu_host_services, vosu_message, vosu_status,
};

/// Every test in this file shares process-wide statics
/// (`ON_MESSAGE_CALLS`, `REENTRANT_RESULT`) to observe what the glue did;
/// each `#[test]` holds this for its whole body so Rust's default parallel
/// test execution cannot interleave them.
static TEST_LOCK: Mutex<()> = Mutex::new(());

static ON_MESSAGE_CALLS: AtomicU32 = AtomicU32::new(0);

struct TestPlugin;

impl Plugin for TestPlugin {
    fn create(_host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn on_message(&mut self, _message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        ON_MESSAGE_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct ConcurrentTestPlugin;

impl ConcurrentPlugin for ConcurrentTestPlugin {
    fn create(_host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn on_message(&self, _message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        ON_MESSAGE_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A minimal `vosu_host_services` whose `send` always succeeds and does
/// nothing else; `create` only needs a non-null, well-formed one.
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
        plugin_id: "test.plugin".into(),
        settings: vosu_plugin_api::CborValue::Map(vec![]),
        data_dir: "/tmp/test-plugin".into(),
    })
    .expect("encode init")
}

/// A mock `HostServices` that answers a `VOSU_READ_PLAN_SYNC` submission
/// itself, synchronously, exactly as an in-process host's real `send`
/// would (`docs/v2/plugin-abi.md` §6.3): it calls straight back into the
/// given plugin's own `dispatch` with a synthesized `READ_PLAN_RESULT`, on
/// this same call stack, before returning.
mod plan_memory_tests {
    use super::*;
    use vosu_plugin_api::{PlanBase, ReadResult};
    use vosu_plugin_internal::{PlanMemory, PlanMemoryError};

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
            let raw_status =
                unsafe { (self.dispatch)(self.plugin_ctx, &raw as *const vosu_message) };
            vosu_status::try_from(raw_status).unwrap_or(vosu_status::VOSU_ERR_BACKEND)
        }
    }

    #[test]
    fn plan_memory_reads_the_synchronous_result_without_calling_on_message() {
        let _guard = super::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let vt = vosu_plugin_internal::vtable_for::<TestPlugin>();
        let noop = noop_host_services();
        let init = init_bytes();
        // SAFETY: the create/dispatch/destroy contract `vosu-plugin-api`'s
        // `export_plugin_create_dispatch_destroy_round_trip` documents.
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

        // SAFETY: the create/dispatch/destroy contract `vosu-plugin-api`'s
        // `export_plugin_create_dispatch_destroy_round_trip` documents.
        unsafe { (vt.destroy.unwrap())(plugin_ctx) };
    }

    /// Like [`SyncAnsweringHost`], but answers with a caller-chosen
    /// `ReadResult` so a test can exercise an error kind/code instead of
    /// the fixed `VOSU_READ_OK` payload above.
    struct SyncAnsweringHostWithResult {
        plugin_ctx: *mut c_void,
        dispatch: unsafe extern "C" fn(*mut c_void, *const vosu_message) -> u32,
        result: ReadResult,
    }

    impl HostServices for SyncAnsweringHostWithResult {
        fn send(&self, message: &Message, _correlation_id: u64) -> vosu_status {
            let Message::ReadPlanSubmit(submit) = message else {
                panic!("test host only answers ReadPlanSubmit");
            };
            let result = Message::ReadPlanResult(vosu_plugin_api::ReadPlanResult {
                plan: submit.plan,
                results: vec![self.result.clone()],
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
            // SAFETY: see `SyncAnsweringHost::send`.
            let raw_status =
                unsafe { (self.dispatch)(self.plugin_ctx, &raw as *const vosu_message) };
            vosu_status::try_from(raw_status).unwrap_or(vosu_status::VOSU_ERR_BACKEND)
        }
    }

    #[test]
    fn flat_read_results_exposes_the_permission_denied_error_code() {
        let _guard = super::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let vt = vosu_plugin_internal::vtable_for::<TestPlugin>();
        let noop = noop_host_services();
        let init = init_bytes();
        // SAFETY: the create/dispatch/destroy contract `vosu-plugin-api`'s
        // `export_plugin_create_dispatch_destroy_round_trip` documents.
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

        let host = SyncAnsweringHostWithResult {
            plugin_ctx,
            dispatch: vt.dispatch.unwrap(),
            result: ReadResult::Error {
                code: vosu_plugin_api::ReadErrorCode::PermissionDenied,
                message: "ptrace scope forbids this read".to_owned(),
            },
        };
        let plan_memory = PlanMemory::new();
        let mut buffers = vosu_plugin_internal::SyncReadBuffers::new();
        let op = vosu_plugin_api::PlanOp {
            offset: 0,
            length: 4,
        };
        let stage = vosu_plugin_api::StageRef {
            base: vosu_plugin_api::StageBase::Root,
            base_offset: 0,
            ops: core::slice::from_ref(&op),
        };
        let results = plan_memory
            .read_flat(
                &host,
                7,
                1,
                PlanBase::Absolute(0x1000),
                &[stage],
                &mut buffers,
            )
            .expect("synchronous read succeeds");
        assert_eq!(results.len(), 1);
        let (kind, _) = results.get(0);
        assert_eq!(
            kind,
            vosu_plugin_api::vosu_read_result_kind::VOSU_READ_ERROR
        );
        assert_eq!(
            results.error_code(0),
            Some(vosu_plugin_api::vosu_read_error_code::VOSU_READ_ERROR_PERMISSION_DENIED)
        );

        // SAFETY: the create/dispatch/destroy contract `vosu-plugin-api`'s
        // `export_plugin_create_dispatch_destroy_round_trip` documents.
        unsafe { (vt.destroy.unwrap())(plugin_ctx) };
    }

    #[test]
    fn flat_read_results_error_code_is_none_for_a_successful_op() {
        let _guard = super::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let vt = vosu_plugin_internal::vtable_for::<TestPlugin>();
        let noop = noop_host_services();
        let init = init_bytes();
        // SAFETY: the create/dispatch/destroy contract `vosu-plugin-api`'s
        // `export_plugin_create_dispatch_destroy_round_trip` documents.
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

        let host = SyncAnsweringHost {
            plugin_ctx,
            dispatch: vt.dispatch.unwrap(),
        };
        let plan_memory = PlanMemory::new();
        let mut buffers = vosu_plugin_internal::SyncReadBuffers::new();
        let op = vosu_plugin_api::PlanOp {
            offset: 0,
            length: 4,
        };
        let stage = vosu_plugin_api::StageRef {
            base: vosu_plugin_api::StageBase::Root,
            base_offset: 0,
            ops: core::slice::from_ref(&op),
        };
        let results = plan_memory
            .read_flat(
                &host,
                8,
                1,
                PlanBase::Absolute(0x1000),
                &[stage],
                &mut buffers,
            )
            .expect("synchronous read succeeds");
        assert_eq!(
            results.error_code(0),
            None,
            "a VOSU_READ_OK entry has no error_code"
        );

        // SAFETY: the create/dispatch/destroy contract `vosu-plugin-api`'s
        // `export_plugin_create_dispatch_destroy_round_trip` documents.
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

        fn on_message(
            &mut self,
            message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
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
        let vt = vosu_plugin_internal::vtable_for::<ReentrantPlanMemoryPlugin>();
        let noop = noop_host_services();
        let init = init_bytes();
        // SAFETY: the create/dispatch/destroy contract `vosu-plugin-api`'s
        // `export_plugin_create_dispatch_destroy_round_trip` documents.
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
        // SAFETY: the create/dispatch/destroy contract `vosu-plugin-api`'s
        // `export_plugin_create_dispatch_destroy_round_trip` documents.
        let status = unsafe { (vt.dispatch.unwrap())(plugin_ctx, &message as *const vosu_message) };
        assert_eq!(status, vosu_status::VOSU_OK as u32);
        assert_eq!(
            *REENTRANT_RESULT.lock().unwrap(),
            Some(Err(PlanMemoryError::OnDispatchThread))
        );

        // SAFETY: the create/dispatch/destroy contract `vosu-plugin-api`'s
        // `export_plugin_create_dispatch_destroy_round_trip` documents.
        unsafe { (vt.destroy.unwrap())(plugin_ctx) };
    }
}

/// The concurrent glue routes the synchronous result exactly as the
/// serialized one does (D22 plugins share D5's delivery contract).
mod concurrent_tests {
    use super::*;
    use vosu_plugin_api::{PlanBase, ReadResult};
    use vosu_plugin_internal::PlanMemory;

    struct SyncAnsweringHost {
        plugin_ctx: *mut c_void,
        dispatch: unsafe extern "C" fn(*mut c_void, *const vosu_message) -> u32,
    }

    impl HostServices for SyncAnsweringHost {
        fn send(&self, message: &Message, _correlation_id: u64) -> vosu_status {
            let Message::ReadPlanSubmit(submit) = message else {
                panic!("test host only answers ReadPlanSubmit");
            };
            assert!(submit.sync, "PlanMemory submits with VOSU_READ_PLAN_SYNC");
            let result = Message::ReadPlanResult(vosu_plugin_api::ReadPlanResult {
                plan: submit.plan,
                results: vec![ReadResult::Ok(vec![5, 6])],
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
            // `create` call and `raw` is a well-formed, live
            // `vosu_message`: the reentrant delivery an in-process host's
            // real `send` performs for `VOSU_READ_PLAN_SYNC` (D5).
            let raw_status =
                unsafe { (self.dispatch)(self.plugin_ctx, &raw as *const vosu_message) };
            vosu_status::try_from(raw_status).unwrap_or(vosu_status::VOSU_ERR_BACKEND)
        }
    }

    #[test]
    fn concurrent_plan_memory_reads_the_synchronous_result_without_calling_on_message() {
        let _guard = super::TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let vt = vosu_plugin_internal::vtable_for_concurrent::<ConcurrentTestPlugin>();
        let noop = noop_host_services();
        let init = init_bytes();
        // SAFETY: `noop` is a local, well-formed `vosu_host_services` that
        // outlives the plugin instance; `init` is a valid encoded `Init`.
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
        let result = PlanMemory::new()
            .read(&host, 9, 1, PlanBase::Absolute(0x2000), vec![])
            .expect("synchronous read succeeds");
        assert_eq!(result.plan, 9);
        assert_eq!(result.results, vec![ReadResult::Ok(vec![5, 6])]);
        assert_eq!(ON_MESSAGE_CALLS.load(Ordering::SeqCst), calls_before);

        // An unrelated asynchronous result still reaches `on_message`.
        let unrelated = Message::ReadPlanResult(vosu_plugin_api::ReadPlanResult {
            plan: 10,
            results: vec![],
        })
        .encode_payload()
        .expect("encode");
        let message = vosu_message {
            opcode: vosu_plugin_api::vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
            flags: 0,
            correlation_id: 0,
            payload: vosu_bytes {
                data: unrelated.as_ptr(),
                len: unrelated.len() as u32,
            },
        };
        // SAFETY: `plugin_ctx` is live until the `destroy` below; `message`
        // is a well-formed, live `vosu_message`.
        let status = unsafe { (vt.dispatch.unwrap())(plugin_ctx, &message as *const vosu_message) };
        assert_eq!(status, vosu_status::VOSU_OK as u32);
        assert_eq!(ON_MESSAGE_CALLS.load(Ordering::SeqCst), calls_before + 1);

        // SAFETY: `plugin_ctx` is destroyed exactly once, here.
        unsafe { (vt.destroy.unwrap())(plugin_ctx) };
    }
}
