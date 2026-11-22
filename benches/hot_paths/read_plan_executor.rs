//! `read_plan_executor` bench group: split out of `hot_paths.rs` so that
//! file stays under the maintained-file line limit
//! (`docs/architecture/source-map.md`).

use super::*;

struct NoModules;
impl ModuleResolver for NoModules {
    fn resolve(&self, _module: &str) -> Option<u64> {
        None
    }
}

fn single_scalar_submit(address: u64) -> ReadPlanSubmit {
    ReadPlanSubmit {
        plan: 1,
        handle: 1,
        root: PlanBase::Absolute(address),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![PlanOp {
                offset: 0,
                length: 8,
            }],
        }],
        sync: false,
    }
}

fn sibling_fields_submit(address: u64, count: u32) -> ReadPlanSubmit {
    ReadPlanSubmit {
        plan: 2,
        handle: 1,
        root: PlanBase::Absolute(address),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: (0..count)
                .map(|index| PlanOp {
                    offset: i64::from(index) * 8,
                    length: 8,
                })
                .collect(),
        }],
        sync: false,
    }
}

/// Compares the read-plan executor against a direct `read_many` call over
/// the same fake memory, for both a single scalar field (D17's zero-
/// allocation hot path) and a stage of 13 sibling fields (P1's "one
/// `read_many` per stage, not one syscall per op" batching fix).
pub(super) fn bench_read_plan_executor(c: &mut Criterion) {
    let base = 0x2000;
    let bytes = vec![0x5a_u8; 4096];
    let memory = SliceMemory {
        base,
        bytes: bytes.into(),
    };
    let modules = NoModules;
    let deadline = || std::time::Instant::now() + std::time::Duration::from_secs(60);

    let mut group = c.benchmark_group("read_plan_executor");

    group.throughput(Throughput::Bytes(8));
    let single_payload = Message::ReadPlanSubmit(single_scalar_submit(base as u64))
        .encode_payload()
        .unwrap();
    let mut out = Vec::new();
    execute_into(
        &single_payload,
        0,
        &memory,
        ProcessBitness::X64,
        &modules,
        deadline(),
        &mut out,
    )
    .unwrap();
    group.bench_function("single_scalar_via_executor", |b| {
        b.iter(|| {
            execute_into(
                black_box(&single_payload),
                0,
                &memory,
                ProcessBitness::X64,
                &modules,
                deadline(),
                black_box(&mut out),
            )
            .unwrap();
        });
    });
    group.bench_function("single_scalar_direct_read_many", |b| {
        b.iter(|| {
            let mut destination = [0_u8; 8];
            let mut requests = [ReadRequest::new(
                black_box(base),
                black_box(&mut destination),
            )];
            black_box(memory.read_many(black_box(&mut requests))).unwrap();
            black_box(destination)
        });
    });

    const SIBLING_COUNT: u32 = 13;
    group.throughput(Throughput::Bytes(8 * u64::from(SIBLING_COUNT)));
    let siblings_payload =
        Message::ReadPlanSubmit(sibling_fields_submit(base as u64, SIBLING_COUNT))
            .encode_payload()
            .unwrap();
    execute_into(
        &siblings_payload,
        0,
        &memory,
        ProcessBitness::X64,
        &modules,
        deadline(),
        &mut out,
    )
    .unwrap();
    group.bench_function("thirteen_siblings_via_executor", |b| {
        b.iter(|| {
            execute_into(
                black_box(&siblings_payload),
                0,
                &memory,
                ProcessBitness::X64,
                &modules,
                deadline(),
                black_box(&mut out),
            )
            .unwrap();
        });
    });
    group.bench_function("thirteen_siblings_direct_read_many", |b| {
        b.iter(|| {
            let mut destinations = [[0_u8; 8]; SIBLING_COUNT as usize];
            let mut requests: Vec<ReadRequest<'_>> = destinations
                .iter_mut()
                .enumerate()
                .map(|(index, dst)| ReadRequest::new(base + index * 8, dst))
                .collect();
            black_box(memory.read_many(black_box(&mut requests))).unwrap();
            black_box(destinations)
        });
    });

    group.finish();
}
