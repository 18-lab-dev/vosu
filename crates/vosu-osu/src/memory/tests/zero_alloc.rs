//! Proves `PlanProcessMemory::read_exact`/`read_many` allocate nothing
//! once warm (D17; P3 review item 2): copies a successful op's bytes
//! straight into the caller's `dst`, and reuses thread-local scratch
//! (`SyncReadBuffers`, `read_many`'s `Vec<PlanOp>`) across calls.
//!
//! `ByteEchoHost` answers a `VOSU_READ_PLAN_SYNC` submission by re-entering
//! the SDK's generated `dispatch` synchronously, exactly as
//! `vosu-plugin-api/tests/plan_memory_alloc.rs` does — and, like that
//! test, never builds an owned `Message`/`ReadPlanSubmit` from the
//! payload (only `Copy` header reads) and answers through its own reused
//! `result_buf`, so nothing on this call stack can hide an allocation
//! this test is supposed to catch.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use vosu_osu_reader::process::{ProcessBitness, ProcessMemory, ReadRequest};
use vosu_plugin_api::{
    HostServices, Init, Message, Plugin, PluginError, VtableHostServices, read_flat_header,
    vosu_bytes, vosu_host_services, vosu_message, vosu_opcode, vosu_plan_op,
    vosu_read_plan_result_header, vosu_read_plan_submit_header, vosu_read_result_flat,
    vosu_read_result_kind, vosu_stage_header, vosu_status,
};

use crate::memory::PlanProcessMemory;

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

struct NoopPlugin;

impl Plugin for NoopPlugin {
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
        struct_size: size_of::<vosu_host_services>() as u32,
        abi_version: vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR,
        send: Some(noop_send),
    }
}

fn init_bytes() -> Vec<u8> {
    vosu_plugin_api::encode(&Init {
        plugin_id: "test.zero_alloc".to_owned(),
        settings: vosu_plugin_api::CborValue::Null,
        data_dir: std::path::PathBuf::from("/tmp"),
    })
    .expect("encode Init")
}

/// Echoes real bytes from `memory` for every op, building the raw
/// `VOSU_OP_READ_PLAN_RESULT` payload directly into its own reused
/// `result_buf` — never through `Message`/CBOR, which would allocate on
/// its own and swamp the count this test takes of `PlanProcessMemory`
/// itself.
struct ByteEchoHost {
    plugin_ctx: Cell<*mut c_void>,
    dispatch: Cell<Option<unsafe extern "C" fn(*mut c_void, *const vosu_message) -> u32>>,
    memory: Vec<u8>,
    base: usize,
    expected_stages: u32,
    result_buf: RefCell<Vec<u8>>,
}

// SAFETY: `plugin_ctx`/`dispatch` are a raw context and function pointer
// from a `NoopPlugin::create` call, documented callable from any thread
// for the plugin instance's whole lifetime (mirrors
// `vosu-plugin-api/tests/plan_memory_alloc.rs`'s identical `FakeHost`).
// Every other field is `Send + Sync` on its own, and this test drives
// every call from a single thread regardless.
unsafe impl Send for ByteEchoHost {}
// SAFETY: see `Send` above.
unsafe impl Sync for ByteEchoHost {}

impl ByteEchoHost {
    /// `base` is this `memory` buffer's own first byte's absolute
    /// address: a read for address `a` is served from
    /// `memory[a - base..]`.
    fn new(memory: Vec<u8>, base: usize) -> Self {
        let vt = vosu_plugin_api::__vtable_for_serialized::<NoopPlugin>();
        let noop = noop_host_services();
        let init = init_bytes();
        // SAFETY: `noop`/`init` are valid for this call's duration; `vt`
        // is a freshly built vtable for `NoopPlugin`.
        let plugin_ctx = unsafe {
            (vt.create.expect("vtable always sets create"))(
                &noop as *const vosu_host_services,
                core::ptr::null_mut(),
                vosu_bytes {
                    data: init.as_ptr(),
                    len: init.len() as u32,
                },
            )
        };
        assert!(!plugin_ctx.is_null());
        Self {
            plugin_ctx: Cell::new(plugin_ctx),
            dispatch: Cell::new(vt.dispatch),
            memory,
            base,
            expected_stages: 1,
            result_buf: RefCell::new(Vec::new()),
        }
    }
}

impl HostServices for ByteEchoHost {
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
        assert_eq!(
            header.stage_count, self.expected_stages,
            "the transport must retain the expected dependency shape"
        );
        let stage_size = size_of::<vosu_stage_header>();
        let stage_at = |index| -> vosu_stage_header {
            read_flat_header(&payload[header.stages_offset as usize + index * stage_size..])
                .expect("stage header")
        };
        let op_size = size_of::<vosu_plan_op>();
        let op_count = (0..header.stage_count as usize)
            .map(|index| stage_at(index).op_count as usize)
            .sum::<usize>();

        let mut buf = self.result_buf.borrow_mut();
        buf.clear();
        let result_header_size = core::mem::size_of::<vosu_read_plan_result_header>();
        buf.resize(result_header_size, 0);
        let entry_size = core::mem::size_of::<vosu_read_result_flat>();
        let results_offset = buf.len() as u32;
        let reserved_len = buf.len() + entry_size * op_count;
        buf.resize(reserved_len, 0);

        let mut op_index = 0;
        for stage_index in 0..header.stage_count as usize {
            let stage = stage_at(stage_index);
            let root = if stage.base.kind
                == vosu_plugin_api::vosu_stage_base_kind::VOSU_STAGE_BASE_ROOT as u32
            {
                header.root.address
            } else {
                let reference = (0..stage.base.ref_stage as usize)
                    .map(|index| stage_at(index).op_count as usize)
                    .sum::<usize>()
                    + stage.base.ref_op as usize;
                let previous: vosu_read_result_flat =
                    read_flat_header(&buf[results_offset as usize + reference * entry_size..])
                        .unwrap();
                let mut pointer = [0; 8];
                pointer[..previous.bytes_len as usize].copy_from_slice(
                    &buf[previous.bytes_offset as usize..][..previous.bytes_len as usize],
                );
                u64::from_le_bytes(pointer)
            };
            for local_index in 0..stage.op_count as usize {
                let op: vosu_plan_op =
                    read_flat_header(&payload[stage.ops_offset as usize + local_index * op_size..])
                        .expect("well-formed op header");
                let address =
                    i128::from(root) + i128::from(stage.base.offset) + i128::from(op.offset)
                        - i128::try_from(self.base).expect("test base fits i128");
                let len = op.length as usize;
                let entry = match usize::try_from(address).ok().and_then(|address| {
                    address
                        .checked_add(len)
                        .filter(|&end| end <= self.memory.len())
                        .map(|end| (address, end))
                }) {
                    Some((start, end)) => {
                        let bytes_offset = buf.len() as u32;
                        buf.extend_from_slice(&self.memory[start..end]);
                        vosu_read_result_flat {
                            kind: vosu_read_result_kind::VOSU_READ_OK as u32,
                            bytes_offset,
                            bytes_len: len as u32,
                            error_code: 0,
                            error_message_offset: 0,
                            error_message_len: 0,
                        }
                    }
                    None => vosu_read_result_flat {
                        kind: vosu_read_result_kind::VOSU_READ_ERROR as u32,
                        bytes_offset: 0,
                        bytes_len: 0,
                        error_code: 0,
                        error_message_offset: 0,
                        error_message_len: 0,
                    },
                };
                let entry_region = results_offset as usize + op_index * entry_size;
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
                op_index += 1;
            }
        }

        let result_header = vosu_read_plan_result_header {
            plan: header.plan,
            results_offset,
            result_count: op_count as u32,
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
        // dispatch function, and `raw` stays valid for this call (`buf`,
        // the borrowed `result_buf`, outlives it).
        let raw_status = unsafe { dispatch(self.plugin_ctx.get(), &raw as *const vosu_message) };
        vosu_status::try_from(raw_status).unwrap_or(vosu_status::VOSU_ERR_BACKEND)
    }
}

fn fill(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| seed.wrapping_add(i as u8)).collect()
}

#[test]
fn read_exact_is_allocation_free_once_warmed() {
    let base = 0x1000;
    let bytes = fill(64, 5);
    let host = ByteEchoHost::new(bytes.clone(), base);
    let process = PlanProcessMemory::new(host, 1, 123, ProcessBitness::X64);
    let mut out = [0u8; 64];

    // Warm: grow both `SYNC_READ_BUFFERS` and the fake host's own
    // `result_buf` to their steady-state capacity, outside the measured
    // region.
    process
        .read_exact(base, &mut out)
        .expect("warm-up read succeeds");
    assert_eq!(out, bytes[..]);

    let (result, allocations) = allocations_during(|| process.read_exact(base, &mut out));
    result.expect("measured read succeeds");
    assert_eq!(out, bytes[..]);
    assert_eq!(allocations, 0, "a warmed-up read_exact must not allocate");
}

#[test]
fn read_many_is_allocation_free_once_warmed() {
    let base = 0x2000;
    let bytes = fill(300, 1);
    let host = ByteEchoHost::new(bytes.clone(), base);
    let process = PlanProcessMemory::new(host, 1, 123, ProcessBitness::X64);
    let mut a = [0u8; 100];
    let mut b = [0u8; 100];
    let mut c = [0u8; 100];

    // Warm: grow `SYNC_READ_BUFFERS`, the thread-local `Vec<PlanOp>`
    // scratch, and the fake host's own `result_buf`.
    process
        .read_many(&mut [
            ReadRequest::new(base, &mut a),
            ReadRequest::new(base + 100, &mut b),
            ReadRequest::new(base + 200, &mut c),
        ])
        .expect("warm-up read succeeds");
    assert_eq!(a, bytes[0..100]);

    let (result, allocations) = allocations_during(|| {
        process.read_many(&mut [
            ReadRequest::new(base, &mut a),
            ReadRequest::new(base + 100, &mut b),
            ReadRequest::new(base + 200, &mut c),
        ])
    });
    result.expect("measured read succeeds");
    assert_eq!(a, bytes[0..100]);
    assert_eq!(b, bytes[100..200]);
    assert_eq!(c, bytes[200..300]);
    assert_eq!(allocations, 0, "a warmed-up read_many must not allocate");
}

#[test]
fn read_chain_is_allocation_free_once_warmed() {
    let mut bytes = vec![0; 96];
    bytes[..8].copy_from_slice(&32u64.to_le_bytes());
    bytes[36..44].copy_from_slice(&64u64.to_le_bytes());
    bytes[72..76].copy_from_slice(&123u32.to_le_bytes());
    let mut host = ByteEchoHost::new(bytes, 0);
    host.expected_stages = 3;
    let process = PlanProcessMemory::new(host, 1, 123, ProcessBitness::X64);
    let mut dst = [0; 4];
    process
        .read_chain(0, &[4, 8], ProcessBitness::X64, &mut dst)
        .unwrap();
    let (result, allocations) =
        allocations_during(|| process.read_chain(0, &[4, 8], ProcessBitness::X64, &mut dst));
    result.unwrap();
    assert_eq!(u32::from_le_bytes(dst), 123);
    assert_eq!(allocations, 0, "dependent stages must remain stack-only");
}

#[test]
fn indirect_many_is_allocation_free_once_warmed() {
    let mut bytes = vec![0; 96];
    bytes[..8].copy_from_slice(&32u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&64u64.to_le_bytes());
    bytes[36..40].copy_from_slice(&123u32.to_le_bytes());
    bytes[72..76].copy_from_slice(&456u32.to_le_bytes());
    let mut host = ByteEchoHost::new(bytes, 0);
    host.expected_stages = 3;
    let process = PlanProcessMemory::new(host, 1, 123, ProcessBitness::X64);
    let (mut a, mut b) = ([0; 4], [0; 4]);
    let mut read = || {
        process.read_indirect_many(
            &mut [
                vosu_osu_reader::process::IndirectReadRequest::new(0, 4, &mut a),
                vosu_osu_reader::process::IndirectReadRequest::new(8, 8, &mut b),
            ],
            ProcessBitness::X64,
        )
    };
    read().unwrap();
    let (result, allocations) = allocations_during(read);
    result.unwrap();
    assert_eq!(u32::from_le_bytes(a), 123);
    assert_eq!(u32::from_le_bytes(b), 456);
    assert_eq!(allocations, 0, "dependent batches must remain stack-only");
}

#[test]
fn mixed_chains_are_allocation_free_once_warmed() {
    let mut bytes = vec![0; 96];
    bytes[..8].copy_from_slice(&32u64.to_le_bytes());
    bytes[36..44].copy_from_slice(&64u64.to_le_bytes());
    bytes[72..76].copy_from_slice(&123u32.to_le_bytes());
    let mut host = ByteEchoHost::new(bytes, 0);
    host.expected_stages = 4;
    let process = PlanProcessMemory::new(host, 1, 123, ProcessBitness::X64);
    let (mut direct, mut dependent) = ([0; 4], [0; 4]);
    let mut resolved = 0;
    let mut read = || {
        process.read_chains(
            &mut [
                vosu_osu_reader::process::ChainReadRequest::new(72, &[], &mut direct),
                vosu_osu_reader::process::ChainReadRequest::new(0, &[4, 8], &mut dependent)
                    .with_resolved_address(&mut resolved),
            ],
            ProcessBitness::X64,
        )
    };
    read().unwrap();
    let (result, allocations) = allocations_during(read);
    result.unwrap();
    assert_eq!(u32::from_le_bytes(direct), 123);
    assert_eq!(u32::from_le_bytes(dependent), 123);
    assert_eq!(resolved, 72);
    assert_eq!(
        allocations, 0,
        "mixed dependency graphs must remain stack-only"
    );
}
