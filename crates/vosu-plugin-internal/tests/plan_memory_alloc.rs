//! Proves `PlanMemory::read_flat` (D17) is allocation-free once its own
//! `SyncReadBuffers` — and the fake host's own scratch buffer below it —
//! have been warmed to their steady-state capacity, for both a 1-op and an
//! 11-op plan (the plan's own F4 acceptance criterion).
//!
//! `FakeHost` re-enters the SDK's own generated `dispatch` synchronously
//! from inside `HostServices::send_flat`, exactly as an in-process host's
//! real `VOSU_READ_PLAN_SYNC` delivery does (`docs/v2/plugin-abi.md` §4,
//! D5): it never decodes the submitted plan into an owned `ReadPlanSubmit`
//! (only `vosu_read_plan_submit_header`/`vosu_stage_header` `Copy` reads,
//! via the public `read_flat_header`), and answers through its own reused
//! `result_buf`, so the whole round trip — encode, submit, host-side
//! "execute", dispatch back, intercept — has no allocation left anywhere
//! on this call stack to hide one.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::hint::black_box;

use vosu_plugin_api::{
    HostServices, Init, Message, PlanBase, PlanMemory, PlanOp, Plugin, PluginError, StageBase,
    StageRef, SyncReadBuffers, read_flat_header, vosu_bytes, vosu_host_services, vosu_message,
    vosu_opcode, vosu_read_plan_result_header, vosu_read_plan_submit_header, vosu_read_result_flat,
    vosu_read_result_kind, vosu_stage_header, vosu_status,
};

struct TrackingAllocator;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method below forwards unchanged to `std::alloc::System`, a
// valid `GlobalAlloc` implementation; this wrapper only adds allocation
// counting around the delegated calls.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `layout` unchanged to `System::alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `layout` unchanged to `System::alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `ptr`, `layout`, and `new_size` unchanged to
        // `System::realloc`, which has the same safety contract as this
        // method.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwards `ptr` and `layout` unchanged to `System::dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: TrackingAllocator = TrackingAllocator;

fn allocations_during<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|count| count.set(0));
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
    let result = operation();
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
    let allocations = ALLOCATIONS.with(Cell::get);
    (result, allocations)
}

/// A plugin that must never see the synchronous `VOSU_OP_READ_PLAN_RESULT`
/// in its own `on_message`: `PlanMemory::read_flat`'s own intercept must
/// swallow it first, exactly like `PlanMemory::read`'s (`sdk.rs`'s own
/// `plan_memory_tests` module).
struct TestPlugin;

impl Plugin for TestPlugin {
    fn create(
        _host: vosu_plugin_api::VtableHostServices,
        _init: Init,
    ) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        panic!("a synchronous VOSU_OP_READ_PLAN_RESULT must never reach on_message: {message:?}");
    }
}

/// A minimal `vosu_host_services`; only `create`/`destroy` need it to be
/// well-formed — every exchange in this test goes through `FakeHost`'s own
/// `send_flat`, never the real vtable `send`.
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
        plugin_id: "test.plan_memory_alloc".to_owned(),
        settings: vosu_plugin_api::CborValue::Null,
        data_dir: std::path::PathBuf::from("/tmp"),
    })
    .expect("encode Init")
}

fn push_struct<T: Copy>(buf: &mut Vec<u8>, value: &T) {
    // SAFETY: every `T` this is called with is a `#[repr(C)]` flat header
    // struct of plain integers with no padding this byte read could expose
    // uninitialized memory from.
    let bytes = unsafe {
        core::slice::from_raw_parts((value as *const T).cast::<u8>(), core::mem::size_of::<T>())
    };
    buf.extend_from_slice(bytes);
}

/// Re-enters the given plugin's own generated `dispatch` synchronously
/// from inside `send_flat`, exactly as an in-process host's real
/// `VOSU_READ_PLAN_SYNC` delivery does (D5): never builds an owned
/// `ReadPlanSubmit`/`Message` from the submitted payload, only reads its
/// `vosu_read_plan_submit_header`/`vosu_stage_header`s (`Copy`, via the
/// public `read_flat_header`) to learn how many ops to answer, then writes
/// a flat `VOSU_OP_READ_PLAN_RESULT` straight into its own reused
/// `result_buf` — one fixed 8-byte `VOSU_READ_OK` value per op.
struct FakeHost {
    plugin_ctx: Cell<*mut c_void>,
    dispatch: Cell<Option<unsafe extern "C" fn(*mut c_void, *const vosu_message) -> u32>>,
    result_buf: RefCell<Vec<u8>>,
}

impl HostServices for FakeHost {
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
        let stage_size = core::mem::size_of::<vosu_stage_header>();
        let mut total_ops: u32 = 0;
        for stage_index in 0..header.stage_count as usize {
            let stage: vosu_stage_header = read_flat_header(
                &payload[header.stages_offset as usize + stage_index * stage_size..],
            )
            .expect("well-formed stage header");
            total_ops += stage.op_count;
        }

        let mut buf = self.result_buf.borrow_mut();
        buf.clear();
        let result_header_size = core::mem::size_of::<vosu_read_plan_result_header>();
        buf.resize(result_header_size, 0);
        let entry_size = core::mem::size_of::<vosu_read_result_flat>();
        let results_offset = buf.len() as u32;
        let reserved_len = buf.len() + entry_size * total_ops as usize;
        buf.resize(reserved_len, 0);
        for op_index in 0..total_ops {
            let bytes_offset = buf.len() as u32;
            push_struct(&mut buf, &u64::from(op_index).to_le_bytes());
            let entry = vosu_read_result_flat {
                kind: vosu_read_result_kind::VOSU_READ_OK as u32,
                bytes_offset,
                bytes_len: 8,
                error_code: 0,
                error_message_offset: 0,
                error_message_len: 0,
            };
            let entry_region = results_offset as usize + op_index as usize * entry_size;
            // SAFETY: `vosu_read_result_flat` is a `#[repr(C)]` flat
            // header struct of plain integers with no padding this byte
            // read could expose uninitialized memory from.
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    (&entry as *const vosu_read_result_flat).cast::<u8>(),
                    entry_size,
                )
            };
            buf[entry_region..entry_region + entry_size].copy_from_slice(bytes);
        }
        let result_header = vosu_read_plan_result_header {
            plan: header.plan,
            results_offset,
            result_count: total_ops,
        };
        // SAFETY: `vosu_read_plan_result_header` is a `#[repr(C)]` flat
        // header struct of plain integers with no padding this byte read
        // could expose uninitialized memory from.
        let header_bytes = unsafe {
            core::slice::from_raw_parts(
                (&result_header as *const vosu_read_plan_result_header).cast::<u8>(),
                result_header_size,
            )
        };
        buf[..result_header_size].copy_from_slice(header_bytes);

        let raw = vosu_message {
            opcode: vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
            flags: 0,
            correlation_id: 0,
            payload: vosu_bytes {
                data: buf.as_ptr(),
                len: buf.len() as u32,
            },
        };
        let dispatch = self.dispatch.get().expect("dispatch set before use");
        // SAFETY: `self.plugin_ctx` is a live context from a matching
        // `create` call, `dispatch` is that same plugin's own generated
        // `dispatch` function, and `raw` stays valid for this call (`buf`,
        // the borrowed `result_buf`, outlives it).
        let raw_status = unsafe { dispatch(self.plugin_ctx.get(), &raw as *const vosu_message) };
        vosu_status::try_from(raw_status).unwrap_or(vosu_status::VOSU_ERR_BACKEND)
    }
}

fn run_zero_alloc_case(op_count: usize) {
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

    let host = FakeHost {
        plugin_ctx: Cell::new(plugin_ctx),
        dispatch: Cell::new(vt.dispatch),
        result_buf: RefCell::new(Vec::new()),
    };
    let plan_memory = PlanMemory::new();
    let mut buffers = SyncReadBuffers::new();
    let ops: Vec<PlanOp> = (0..op_count)
        .map(|i| PlanOp {
            offset: i as i64 * 8,
            length: 8,
        })
        .collect();
    let stages = [StageRef {
        base: StageBase::Root,
        base_offset: 0,
        ops: &ops,
    }];

    // Warm: grow both `buffers` and the fake host's own `result_buf` to
    // their steady-state capacity, outside the measured region.
    {
        let result = plan_memory
            .read_flat(
                &host,
                7,
                1,
                PlanBase::Absolute(0x1000),
                &stages,
                &mut buffers,
            )
            .expect("warm-up read succeeds");
        assert_eq!(result.len(), op_count);
    }
    let _ = allocations_during(|| ());

    let (result, allocations) = allocations_during(|| {
        plan_memory
            .read_flat(
                black_box(&host),
                black_box(7),
                black_box(1),
                black_box(PlanBase::Absolute(0x1000)),
                black_box(&stages),
                black_box(&mut buffers),
            )
            .expect("measured read succeeds")
    });
    assert_eq!(result.len(), op_count);
    for index in 0..op_count {
        let (kind, bytes) = result.get(index);
        assert_eq!(kind, vosu_read_result_kind::VOSU_READ_OK);
        assert_eq!(bytes, (index as u64).to_le_bytes());
    }
    assert_eq!(
        allocations, 0,
        "a warmed PlanMemory::read_flat call ({op_count} ops) must allocate nothing"
    );

    // SAFETY: `plugin_ctx` was created by `vt.create` above and is not
    // used again after this.
    unsafe { (vt.destroy.unwrap())(plugin_ctx) };
}

#[test]
fn read_flat_is_allocation_free_once_warmed_single_op() {
    run_zero_alloc_case(1);
}

#[test]
fn read_flat_is_allocation_free_once_warmed_eleven_ops() {
    run_zero_alloc_case(11);
}
