//! Proves `execute_into` is allocation-free once its output buffer has
//! been warmed to size, for both a single-stage and a dependent-stage
//! plan (the D17 M1 performance gate, mirroring
//! `tests/performance_invariants.rs::scalar_readers_allocate_nothing`'s
//! own pattern one crate down, since that integration test cannot see
//! this crate's private executor). Split out of `tests.rs` so that module
//! stays under the maintained-file line limit
//! (`docs/architecture/source-map.md`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use vosu_plugin_api::{Bitness, Message, PlanMemory, StageRef, SyncReadBuffers};

use super::helpers::{FakeMemory, NoModules, far_deadline, op};
use super::*;

struct TrackingAllocator;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method below forwards unchanged to `std::alloc::System`,
// a valid `GlobalAlloc` implementation; this wrapper only adds
// allocation counting around the delegated calls.
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

#[test]
fn execute_into_is_allocation_free_once_warmed_single_stage() {
    let memory = FakeMemory::new(vec![(0x1000, vec![1, 2, 3, 4, 5, 6, 7, 8])]);
    let submit = ReadPlanSubmit {
        plan: 200,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 8)],
        }],
        sync: false,
    };
    let message = Message::ReadPlanSubmit(submit);
    let payload = message.encode_payload().unwrap();
    let deadline = far_deadline();
    let mut out = Vec::new();

    // Warm: grow `out` to its steady-state capacity and warm the
    // thread-local allocator-tracking state, both outside the
    // measured region.
    execute_into(
        &payload,
        0,
        &memory,
        ProcessBitness::X64,
        &NoModules,
        deadline,
        &mut out,
    )
    .unwrap();
    let _ = allocations_during(|| ());

    let (_, allocations) = allocations_during(|| {
        execute_into(
            black_box(&payload),
            0,
            &memory,
            ProcessBitness::X64,
            &NoModules,
            black_box(deadline),
            black_box(&mut out),
        )
        .unwrap();
    });
    assert_eq!(out.len(), 16 + 24 + 8);
    assert_eq!(
        allocations, 0,
        "a warmed single-stage read plan must allocate nothing"
    );
}

#[test]
fn execute_into_is_allocation_free_once_warmed_dependent_stage() {
    // stage 0 reads an 8-byte pointer at 0x1000 pointing at 0x3000;
    // stage 1 reads 4 bytes at 0x3000 + 8.
    let mut pointer_bytes = 0x3000u64.to_le_bytes().to_vec();
    pointer_bytes.extend([0u8; 8]);
    let memory = FakeMemory::new(vec![
        (0x1000, pointer_bytes),
        (0x3000, vec![0; 8].into_iter().chain([9, 9, 9, 9]).collect()),
    ]);
    let submit = ReadPlanSubmit {
        plan: 201,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![
            Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![op(0, 8)],
            },
            Stage {
                base: StageBase::Dependent {
                    ref_stage: 0,
                    ref_op: 0,
                },
                base_offset: 0,
                ops: vec![op(8, 4)],
            },
        ],
        sync: false,
    };
    let message = Message::ReadPlanSubmit(submit);
    let payload = message.encode_payload().unwrap();
    let deadline = far_deadline();
    let mut out = Vec::new();

    execute_into(
        &payload,
        0,
        &memory,
        ProcessBitness::X64,
        &NoModules,
        deadline,
        &mut out,
    )
    .unwrap();
    let _ = allocations_during(|| ());

    let (_, allocations) = allocations_during(|| {
        execute_into(
            black_box(&payload),
            0,
            &memory,
            ProcessBitness::X64,
            &NoModules,
            black_box(deadline),
            black_box(&mut out),
        )
        .unwrap();
    });
    assert_eq!(out.len(), 16 + 24 * 2 + 8 + 4);
    assert_eq!(
        allocations, 0,
        "a warmed dependent-stage read plan must allocate nothing"
    );
}

#[test]
fn execute_into_is_allocation_free_once_warmed_multi_op_stage() {
    // One stage, 13 sibling ops (the F1 regression case: more than a
    // handful of participants, enough that the old `Vec`-based
    // `participants`/`requests` would have grown past its initial
    // capacity at least once).
    const SIBLINGS: usize = 13;
    let region: Vec<u8> = (0..(SIBLINGS as u8 * 4)).collect();
    let memory = FakeMemory::new(vec![(0x1000, region)]);
    let submit = ReadPlanSubmit {
        plan: 202,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: (0..SIBLINGS as i64).map(|i| op(i * 4, 4)).collect(),
        }],
        sync: false,
    };
    let message = Message::ReadPlanSubmit(submit);
    let payload = message.encode_payload().unwrap();
    let deadline = far_deadline();
    let mut out = Vec::new();

    execute_into(
        &payload,
        0,
        &memory,
        ProcessBitness::X64,
        &NoModules,
        deadline,
        &mut out,
    )
    .unwrap();
    let _ = allocations_during(|| ());

    let (_, allocations) = allocations_during(|| {
        execute_into(
            black_box(&payload),
            0,
            &memory,
            ProcessBitness::X64,
            &NoModules,
            black_box(deadline),
            black_box(&mut out),
        )
        .unwrap();
    });
    assert_eq!(out.len(), 16 + 24 * SIBLINGS + 4 * SIBLINGS);
    assert_eq!(
        allocations, 0,
        "a warmed 13-sibling multi-op stage must allocate nothing"
    );
}

#[test]
fn execute_into_is_allocation_free_once_warmed_multi_op_dependent_stage() {
    // Stage 0: a single op reads the 8-byte pointer. Stage 1 depends on
    // it and batches 13 sibling ops against the resolved base — the F1
    // regression case one level into a dependent chain.
    const SIBLINGS: usize = 13;
    let pointer_bytes = 0x3000u64.to_le_bytes().to_vec();
    let region: Vec<u8> = (0..(SIBLINGS as u8 * 4)).collect();
    let memory = FakeMemory::new(vec![(0x1000, pointer_bytes), (0x3000, region)]);
    let submit = ReadPlanSubmit {
        plan: 203,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![
            Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![op(0, 8)],
            },
            Stage {
                base: StageBase::Dependent {
                    ref_stage: 0,
                    ref_op: 0,
                },
                base_offset: 0,
                ops: (0..SIBLINGS as i64).map(|i| op(i * 4, 4)).collect(),
            },
        ],
        sync: false,
    };
    let message = Message::ReadPlanSubmit(submit);
    let payload = message.encode_payload().unwrap();
    let deadline = far_deadline();
    let mut out = Vec::new();

    execute_into(
        &payload,
        0,
        &memory,
        ProcessBitness::X64,
        &NoModules,
        deadline,
        &mut out,
    )
    .unwrap();
    let _ = allocations_during(|| ());

    let (_, allocations) = allocations_during(|| {
        execute_into(
            black_box(&payload),
            0,
            &memory,
            ProcessBitness::X64,
            &NoModules,
            black_box(deadline),
            black_box(&mut out),
        )
        .unwrap();
    });
    assert_eq!(out.len(), 16 + 24 * (1 + SIBLINGS) + 8 + 4 * SIBLINGS);
    assert_eq!(
        allocations, 0,
        "a warmed 13-sibling dependent multi-op stage must allocate nothing"
    );
}

/// F4's own core-level acceptance criterion: a real `PluginHost`, F3's
/// `test_support` memory seam standing in for a real OS process, and a
/// bundled SDK plugin ([`vosu_plugin_testkit::read_plan::ReadPlanPlugin`])
/// — `PlanMemory::read_flat` called directly on this test thread against
/// that plugin's own `VtableHostServices`
/// ([`vosu_plugin_testkit::read_plan::host_services`]), exercising the
/// real pipeline `vosu-plugin-api`'s own `plan_memory_alloc.rs` (a fake
/// host) cannot reach: `PluginHostHandler::handle_read_plan_sync` ->
/// `PluginHost::handle_read_plan_submit_raw` -> `execute_into` into its
/// own per-thread reused buffer -> `dispatch_raw` -> the SDK's
/// `try_intercept_sync_raw` -> `read_flat`'s own `SyncReadBuffers`.
#[test]
fn in_process_sync_round_trip_allocates_nothing_after_warmup() {
    struct DummyStorage;
    impl crate::plugin::settings::SettingsStorage for DummyStorage {
        fn load(
            &self,
            _plugin_id: &str,
        ) -> Result<
            serde_json::Map<String, serde_json::Value>,
            crate::plugin::settings::SettingsError,
        > {
            Ok(serde_json::Map::new())
        }
        fn store(
            &self,
            _plugin_id: &str,
            _values: &serde_json::Map<String, serde_json::Value>,
        ) -> Result<(), crate::plugin::settings::SettingsError> {
            Ok(())
        }
    }
    struct SilentConsole;
    impl crate::logging::ConsoleSink for SilentConsole {
        fn is_tty(&self) -> bool {
            false
        }
        fn write_log(&self, _text: &str) -> std::io::Result<()> {
            Ok(())
        }
        fn clear_progress(&self, _lines: usize) -> std::io::Result<()> {
            Ok(())
        }
        fn write_progress(&self, _text: &str) -> std::io::Result<()> {
            Ok(())
        }
    }

    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let log_dir = tempfile::tempdir().unwrap();
    let logger = crate::logging::Logger::new(
        log_dir.path(),
        crate::logging::LoggerOptions {
            version: "0.1.0".to_owned(),
            debug_log: false,
            queue_capacity: 64,
        },
        Arc::new(SilentConsole),
    )
    .unwrap();
    let settings = Arc::new(crate::plugin::settings::PluginSettingsService::new(
        Arc::new(DummyStorage),
    ));
    let host = crate::plugin::PluginHost::new_for_tests(
        logger,
        settings,
        runtime.handle().clone(),
        Arc::new(crate::plugin::SyntheticForegroundSource::new(None)),
        crate::plugin::IDENTIFICATION_TIMEOUT,
    );

    let plugin_id = "testkit.readplan.coreflat";
    let (events, _commands) = vosu_plugin_testkit::read_plan::register(plugin_id);
    let manifest = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=['proc-coreflat']\n[[provides]]\nid='game:fixture'\nversion='*'\n"
    );
    let loaded = crate::plugin::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[crate::plugin::BundledPlugin {
            manifest: &manifest,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: vosu_plugin_testkit::read_plan::vosu_plugin_vtable,
        }],
    )
    .expect("loading the read-plan testkit plugin");
    let data_dir = tempfile::tempdir().unwrap();
    host.create_instance(
        crate::plugin::PluginInstanceId::new(1),
        &loaded[0],
        data_dir.path().to_path_buf(),
        None,
    )
    .expect("creating the read-plan testkit instance");

    let pid = 910_300_200;
    host.process_appeared(crate::plugin::ProcessRecord {
        pid,
        name: "proc-coreflat".to_owned(),
        path: "/games/proc-coreflat".to_owned(),
        command_line: "proc-coreflat".to_owned(),
        bitness: Bitness::X64,
        start_time: 1,
        argv: None,
    });
    let handle = match events.recv_timeout(Duration::from_secs(5)) {
        Ok(vosu_plugin_testkit::read_plan::ReadPlanEvent::Attached { handle }) => handle,
        other => panic!("expected SESSION_ATTACHED, got {other:?}"),
    };

    // F3's seam: a byte buffer stands in for `pid`'s real process memory,
    // so this test never needs a real OS process behind it.
    let bytes: Vec<u8> = (0..16).collect();
    crate::plugin::test_support::set_session_memory(
        pid,
        Arc::new(FakeMemory::new(vec![(0x2000, bytes.clone())])),
        ProcessBitness::X64,
    );

    let plan_memory = PlanMemory::new();
    let host_services = vosu_plugin_testkit::read_plan::host_services(plugin_id);
    let ops = [op(0, 8), op(8, 8)];
    let stages = [StageRef {
        base: StageBase::Root,
        base_offset: 0,
        ops: &ops,
    }];
    let mut buffers = SyncReadBuffers::new();

    // Warm: grow `buffers` to steady-state capacity, and warm the
    // thread-local allocator-tracking state, both outside the measured
    // region.
    let warm = plan_memory
        .read_flat(
            &host_services,
            900,
            handle,
            PlanBase::Absolute(0x2000),
            &stages,
            &mut buffers,
        )
        .expect("warm-up read_flat call");
    assert_eq!(warm.len(), 2);
    let _ = allocations_during(|| ());

    let (results, allocations) = allocations_during(|| {
        plan_memory
            .read_flat(
                black_box(&host_services),
                black_box(900),
                black_box(handle),
                black_box(PlanBase::Absolute(0x2000)),
                black_box(&stages),
                black_box(&mut buffers),
            )
            .expect("warmed read_flat call")
    });
    assert_eq!(results.len(), 2);
    let (kind0, bytes0) = results.get(0);
    let (kind1, bytes1) = results.get(1);
    assert_eq!(kind0, vosu_read_result_kind::VOSU_READ_OK);
    assert_eq!(kind1, vosu_read_result_kind::VOSU_READ_OK);
    assert_eq!(bytes0, &bytes[0..8]);
    assert_eq!(bytes1, &bytes[8..16]);
    assert_eq!(
        allocations, 0,
        "a warmed in-process VOSU_READ_PLAN_SYNC round trip must allocate nothing"
    );

    crate::plugin::test_support::clear_session_memory(pid);
}

/// Measures borrowed wire decoding only, not K4 routing or K3 persistent-tree
/// commits (which allocate by design and are measured by M2/X2).
#[test]
fn borrowed_publish_batch_decode_allocates_nothing() {
    use vosu_plugin_api::{PublishBatchWriter, PublishValueRef, decode_publish_batch_ref};
    let mut writer = PublishBatchWriter::new();
    writer.begin(8410);
    writer.push("vosu.play", "/combo", PublishValueRef::U64(42));
    let mut payload = Vec::new();
    writer.finish_into(&mut payload).unwrap();
    let _ = allocations_during(|| ());
    let (value, allocations) = allocations_during(|| {
        let batch = decode_publish_batch_ref(black_box(&payload)).unwrap();
        let entry = batch.entries().next().unwrap();
        black_box((entry.ns, entry.pointer, entry.value))
    });
    assert_eq!(
        allocations, 0,
        "borrowed host decode must not allocate strings or values"
    );
    assert_eq!(value.0, "vosu.play");
    assert_eq!(value.1, "/combo");
    assert!(matches!(value.2, PublishValueRef::U64(42)));
    let (_, counted) = allocations_during(|| black_box(vec![0_u8; black_box(64)]));
    assert!(counted > 0, "the counter must detect a real allocation");
}
