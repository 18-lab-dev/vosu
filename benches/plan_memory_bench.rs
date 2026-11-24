//! D17: reader decoding through PlanProcessMemory, public core executor and SDK interception.
//! Release benches cannot use test-harness; PluginHost routing is intentionally excluded.
use criterion::{Criterion, criterion_group, criterion_main};
use std::{
    ffi::c_void,
    hint::black_box,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use vosu_core::plugin::{ModuleResolver, execute_into};
use vosu_osu::memory::PlanProcessMemory;
use vosu_osu_reader::{
    memory::{LazerMemory, LazerPattern, MemoryReader, StableMemory, lazer_offsets::LazerOffsets},
    process::{BatchScanResult, Pattern, ProcessHandle, ScanBackend},
};
use vosu_plugin_api::{
    HostServices, Init, Message, Plugin, PluginError, VtableHostServices, vosu_bytes,
    vosu_host_services, vosu_message, vosu_opcode, vosu_status,
};
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};
#[path = "support/stable_fixture.rs"]
mod stable_fixture;
use stable_fixture::patterns;
struct SliceMemory(Vec<u8>);
#[cfg(windows)]
fn native_fixture() -> &'static vosu_sys::process::WindowsProcessMemory {
    static PROCESS: std::sync::LazyLock<vosu_sys::process::WindowsProcessMemory> =
        std::sync::LazyLock::new(|| {
            vosu_sys::process::WindowsProcessMemory::open(std::process::id()).unwrap()
        });
    &PROCESS
}
impl ProcessMemory for SliceMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let source = address
            .checked_add(dst.len())
            .and_then(|end| self.0.get(address..end))
            .ok_or_else(|| MemoryError::Backend {
                operation: "bench",
                message: "outside fixture".into(),
            })?;
        #[cfg(not(windows))]
        dst.copy_from_slice(source);
        #[cfg(windows)]
        native_fixture().read_exact(source.as_ptr() as usize, dst)?;
        Ok(())
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        #[cfg(not(windows))]
        {
            for read in reads {
                self.read_exact(read.address, read.dst)?;
            }
            Ok(())
        }
        #[cfg(windows)]
        {
            for read in reads.iter() {
                if read.address.checked_add(read.dst.len()).is_none_or(|end| end > self.0.len()) {
                    return Err(MemoryError::Backend { operation: "bench", message: "outside fixture".into() });
                }
            }
            let base = self.0.as_ptr() as usize;
            for read in reads.iter_mut() { read.address += base; }
            let result = native_fixture().read_many(reads);
            for read in reads.iter_mut() { read.address -= base; }
            result
        }
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0,
            size: self.0.len(),
        }])
    }
    fn reads_pin_process_identity(&self) -> bool { cfg!(windows) }
}
struct Modules;
impl ModuleResolver for Modules {
    fn resolve(&self, _: &str) -> Option<u64> {
        None
    }
}
struct Intercept;
impl Plugin for Intercept {
    fn create(_: VtableHostServices, _: Init) -> Result<Self, PluginError> {
        Ok(Self)
    }
    fn on_message(&mut self, _: Message, _: u64) -> Result<(), PluginError> {
        panic!("SYNC must be intercepted")
    }
}
unsafe extern "C" fn noop(_: *mut c_void, _: *const vosu_message) -> u32 {
    vosu_status::VOSU_OK as u32
}
struct ExecutorHost {
    context: *mut c_void,
    vtable: vosu_plugin_api::vosu_plugin_vtable,
    memory: SliceMemory,
    bitness: ProcessBitness,
    output: Mutex<Vec<u8>>,
}
// SAFETY: the SDK permits synchronous result interception from any submitting thread;
// context lives until Drop, and output is serialized. No user callback is dispatched.
unsafe impl Send for ExecutorHost {}
// SAFETY: same invariants as Send; the generated SDK owns synchronization of interception.
unsafe impl Sync for ExecutorHost {}
impl ExecutorHost {
    fn new(bitness: ProcessBitness) -> Self {
        let vtable = vosu_plugin_api::__vtable_for_serialized::<Intercept>();
        static HOST: vosu_host_services = vosu_host_services {
            struct_size: size_of::<vosu_host_services>() as u32,
            abi_version: vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR,
            send: Some(noop),
        };
        let init = vosu_plugin_api::encode(&Init {
            plugin_id: "vosu.bench.plan".into(),
            settings: vosu_plugin_api::CborValue::Null,
            data_dir: std::env::temp_dir(),
        })
        .unwrap();
        // SAFETY: matching generated create function, valid host and init buffers for this call.
        let context = unsafe {
            vtable.create.unwrap()(
                &HOST,
                std::ptr::null_mut(),
                vosu_bytes {
                    data: init.as_ptr(),
                    len: init.len() as u32,
                },
            )
        };
        assert!(!context.is_null());
        Self {
            context,
            vtable,
            memory: SliceMemory(if bitness == ProcessBitness::X86 {
                stable_fixture::bytes()
            } else {
                vec![0; 64 * 1024]
            }),
            bitness,
            output: Mutex::new(Vec::new()),
        }
    }
}
impl HostServices for ExecutorHost {
    fn send(&self, _: &Message, _: u64) -> vosu_status {
        panic!("bench uses flat submissions only")
    }
    fn send_flat(
        &self,
        opcode: vosu_opcode,
        flags: u32,
        correlation_id: u64,
        payload: &[u8],
    ) -> vosu_status {
        assert_eq!(opcode, vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT);
        let mut output = self.output.lock().unwrap();
        execute_into(
            payload,
            flags,
            &self.memory,
            self.bitness,
            &Modules,
            Instant::now() + Duration::from_secs(1),
            &mut output,
        )
        .unwrap();
        let message = vosu_message {
            opcode: vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
            flags: 0,
            correlation_id,
            payload: vosu_bytes {
                data: output.as_ptr(),
                len: output.len() as u32,
            },
        };
        // SAFETY: matching live context, complete flat result, output held through dispatch;
        // SYNC interception does not invoke plugin code or re-enter this host.
        vosu_status::try_from(unsafe { self.vtable.dispatch.unwrap()(self.context, &message) })
            .unwrap()
    }
}
impl Drop for ExecutorHost {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner of the context from matching create; all calls finished.
        unsafe { self.vtable.destroy.unwrap()(self.context) };
    }
}

// Raw zero-fixture captures can retry signature resolution. Keep that region
// walk identical to hot_paths; this bench isolates read-plan, not scan-broker cost.
struct FixtureScan(ProcessHandle);
impl ScanBackend for FixtureScan {
    fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        self.0.scan_all(pattern)
    }
    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        self.0.scan_batch(patterns)
    }
}
fn counted_process(bitness: ProcessBitness) -> (ProcessHandle, Arc<std::sync::atomic::AtomicU64>) {
    let memory = PlanProcessMemory::new(ExecutorHost::new(bitness), 1, 910_600_102, bitness);
    let counter = memory.plan_counter();
    (
        ProcessHandle::with_scan_backend(
            Arc::new(memory),
            bitness,
            Arc::new(FixtureScan(ProcessHandle::new(
                Arc::new(SliceMemory(vec![0; 64 * 1024])),
                bitness,
            ))),
        ),
        counter,
    )
}
fn process(bitness: ProcessBitness) -> ProcessHandle {
    counted_process(bitness).0
}
struct Sequential(ProcessHandle);
impl ProcessMemory for Sequential {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.0.read_exact(address, dst)
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.0.read_many(reads)
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        self.0.readable_regions()
    }
}
fn bench(c: &mut Criterion) {
    let mut stable = StableMemory::new(process(ProcessBitness::X86), patterns());
    let mut lazer = LazerMemory::new(
        process(ProcessBitness::X64),
        Arc::new(LazerOffsets::embedded().unwrap()),
        LazerPattern::source_default(),
    );
    lazer.set_resolved_state(0x1000, 0x2000);
    // Assert successful, non-default decoding before measuring either transport.
    let native = |bitness| {
        ProcessHandle::new(
            Arc::new(SliceMemory(if bitness == ProcessBitness::X86 {
                stable_fixture::bytes()
            } else {
                vec![0; 64 * 1024]
            })),
            bitness,
        )
    };
    let mut native_stable = StableMemory::new(native(ProcessBitness::X86), patterns());
    stable_fixture::assert_capture(&mut stable);
    stable_fixture::assert_capture(&mut native_stable);
    assert_eq!(
        format!("{:?}", stable_fixture::capture(&mut stable)),
        format!("{:?}", stable_fixture::capture(&mut native_stable)),
    );
    let mut native_lazer = LazerMemory::new(
        native(ProcessBitness::X64),
        Arc::new(LazerOffsets::embedded().unwrap()),
        LazerPattern::source_default(),
    );
    native_lazer.set_resolved_state(0x1000, 0x2000);
    assert_eq!(
        format!("{:?}", stable.global()),
        format!("{:?}", native_stable.global())
    );
    assert_eq!(
        format!("{:?}", stable.menu("")),
        format!("{:?}", native_stable.menu(""))
    );
    assert_eq!(
        format!(
            "{:?}",
            stable.gameplay(stable_fixture::PLAY_TIME, stable_fixture::FIRST_OBJECT_TIME)
        ),
        format!(
            "{:?}",
            native_stable.gameplay(stable_fixture::PLAY_TIME, stable_fixture::FIRST_OBJECT_TIME)
        )
    );
    assert_eq!(
        format!("{:?}", lazer.global()),
        format!("{:?}", native_lazer.global())
    );
    assert_eq!(
        format!("{:?}", lazer.menu("")),
        format!("{:?}", native_lazer.menu(""))
    );
    assert_eq!(
        format!("{:?}", lazer.gameplay(0.0, None)),
        format!("{:?}", native_lazer.gameplay(0.0, None))
    );
    let precise = stable.global_precise().unwrap();
    let native_precise = native_stable.global_precise().unwrap();
    assert_eq!(format!("{precise:?}"), format!("{native_precise:?}"));
    let vosu_sys::process::ReadStatus::Ready(value) = precise else {
        panic!("populated precise fixture must be ready");
    };
    assert_eq!(value.status, 3);
    assert_eq!(value.time, 0.0);
    let mut group = c.benchmark_group("plan_memory");
    #[cfg(windows)]
    for (name, reader) in [("native", &mut native_stable), ("plan", &mut stable)] {
        let _ = reader.global_precise().unwrap();
        let _ = vosu_sys::process::WindowsProcessMemory::take_bench_counts();
        let _ = reader.global_precise().unwrap();
        eprintln!("WP_FIXTURE {name} precise {:?}", vosu_sys::process::WindowsProcessMemory::take_bench_counts());
        let _ = vosu_sys::process::WindowsProcessMemory::take_bench_counts();
        let _ = reader.global();
        let _ = reader.menu("");
        let _ = reader.gameplay(stable_fixture::PLAY_TIME, stable_fixture::FIRST_OBJECT_TIME);
        eprintln!("WP_FIXTURE {name} regular {:?}", vosu_sys::process::WindowsProcessMemory::take_bench_counts());
    }
    for sequential in [true, false] {
        let (process, counter) = counted_process(ProcessBitness::X86);
        let process = if sequential {
            ProcessHandle::new(Arc::new(Sequential(process)), ProcessBitness::X86)
        } else {
            process
        };
        let mut reader = StableMemory::new(process, patterns());
        let before = counter.load(std::sync::atomic::Ordering::Relaxed);
        let _ = reader.global();
        let global = counter.load(std::sync::atomic::Ordering::Relaxed);
        let _ = reader.menu("");
        let menu = counter.load(std::sync::atomic::Ordering::Relaxed);
        let _ = reader.gameplay(stable_fixture::PLAY_TIME, stable_fixture::FIRST_OBJECT_TIME);
        let gameplay = counter.load(std::sync::atomic::Ordering::Relaxed);
        eprintln!(
            "sequential={sequential}: global={} menu={} gameplay={}",
            global - before,
            menu - global,
            gameplay - menu
        );
    }
    let floor = process(ProcessBitness::X86);
    group.bench_function("floor_one_op", |b| {
        b.iter(|| black_box(floor.read_u32(0).unwrap()))
    });
    let mut destinations = [[0; 4]; 18];
    group.bench_function("floor_eighteen_independent_ops", |b| {
        b.iter(|| {
            let mut requests = destinations.each_mut().map(|dst| ReadRequest::new(0, dst));
            floor.read_many(&mut requests).unwrap();
            black_box(&requests);
        })
    });
    let mut many_destinations = [[0; 4]; 56];
    group.bench_function("floor_fifty_six_independent_ops", |b| {
        b.iter(|| {
            let mut requests = many_destinations
                .each_mut()
                .map(|dst| ReadRequest::new(0, dst));
            floor.read_many(&mut requests).unwrap();
            black_box(&requests);
        })
    });
    let mut sequential_stable = StableMemory::new(
        ProcessHandle::new(
            Arc::new(Sequential(process(ProcessBitness::X86))),
            ProcessBitness::X86,
        ),
        patterns(),
    );
    group.bench_function("stable_precise_native", |b| {
        b.iter(|| black_box(native_stable.global_precise().unwrap()))
    });
    group.bench_function("stable_precise_plan", |b| {
        b.iter(|| black_box(stable.global_precise().unwrap()))
    });
    group.bench_function("stable_native", |b| {
        b.iter(|| {
            let _ = black_box(native_stable.global());
            let _ = black_box(native_stable.menu(""));
            let _ = black_box(
                native_stable
                    .gameplay(stable_fixture::PLAY_TIME, stable_fixture::FIRST_OBJECT_TIME),
            );
        })
    });
    group.bench_function("stable_unfused_plans", |b| {
        b.iter(|| {
            let _ = black_box(sequential_stable.global());
            let _ = black_box(sequential_stable.menu(""));
            let _ = black_box(
                sequential_stable
                    .gameplay(stable_fixture::PLAY_TIME, stable_fixture::FIRST_OBJECT_TIME),
            );
        })
    });
    group.bench_function("stable_regular_fields", |b| {
        b.iter(|| {
            let _ = black_box(stable.global());
            let _ = black_box(stable.menu(""));
            let _ = black_box(
                stable.gameplay(stable_fixture::PLAY_TIME, stable_fixture::FIRST_OBJECT_TIME),
            );
        })
    });
    group.bench_function("lazer_regular_fields", |b| {
        b.iter(|| {
            let _ = black_box(lazer.global());
            let _ = black_box(lazer.menu(""));
            let _ = black_box(lazer.gameplay(0.0, None));
        })
    });
    group.finish();
}
criterion_group!(benches, bench);
criterion_main!(benches);
