//! Diagnostic decomposition: identical native RPM work versus flat execution,
//! SDK framing, result validation and the SDK's warmed result-buffer copy.
use super::{ExecutorHost, Modules, lazer_fixture};
use criterion::{BenchmarkId, Criterion};
use std::{
    hint::black_box,
    sync::Arc,
    time::{Duration, Instant},
};
use vosu_core::plugin::execute_into;
use vosu_osu::memory::PlanProcessMemory;
use vosu_osu_reader::memory::lazer_offsets::LazerOffsets;
use vosu_plugin_api::{
    Message, PlanBase, PlanOp, ReadPlanSubmit, Stage, StageBase, VOSU_READ_PLAN_SYNC,
    validate_read_plan_result,
};
use vosu_sys::process::{ProcessBitness, ProcessMemory, ReadRequest};

pub(super) fn bench(c: &mut Criterion) {
    let offsets = LazerOffsets::embedded().unwrap();
    let fixture = Arc::new(lazer_fixture::Fixture::active_lazer(&offsets));
    let mut group = c.benchmark_group("plan_costs");
    for (count, length) in [(1, 8), (4, 8), (18, 8), (1, 256), (18, 256)] {
        let label = format!("{count}x{length}");
        let root = PlanBase::Absolute(fixture.game_base as u64);
        let ops = vec![PlanOp { offset: 0, length }; count];
        let submit = Message::ReadPlanSubmit(ReadPlanSubmit {
            plan: 1,
            handle: 1,
            root,
            stages: vec![Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops,
            }],
            sync: true,
        })
        .encode_payload()
        .unwrap();
        let mut output = Vec::new();
        execute_into(
            &submit,
            VOSU_READ_PLAN_SYNC,
            fixture.as_ref(),
            ProcessBitness::X64,
            &Modules,
            Instant::now() + Duration::from_secs(1),
            &mut output,
        )
        .unwrap();
        validate_read_plan_result(&output).unwrap();
        eprintln!(
            "cost_shape={label} submit_bytes={} result_bytes={}",
            submit.len(),
            output.len()
        );
        let mut destinations = vec![vec![0; length as usize]; count];
        let mut requests: Vec<_> = destinations
            .iter_mut()
            .map(|dst| ReadRequest::new(fixture.game_base, dst))
            .collect();
        group.bench_with_input(BenchmarkId::new("native", &label), &(), |b, _| {
            b.iter(|| {
                fixture.read_many(&mut requests).unwrap();
                black_box(&requests);
            })
        });
        group.bench_with_input(BenchmarkId::new("execute", &label), &(), |b, _| {
            b.iter(|| {
                execute_into(
                    &submit,
                    VOSU_READ_PLAN_SYNC,
                    fixture.as_ref(),
                    ProcessBitness::X64,
                    &Modules,
                    Instant::now() + Duration::from_secs(1),
                    &mut output,
                )
                .unwrap();
                black_box(&output);
            })
        });
        let flat = PlanProcessMemory::new(
            ExecutorHost::with_memory(ProcessBitness::X64, fixture.clone()),
            1,
            std::process::id(),
            ProcessBitness::X64,
        );
        group.bench_with_input(BenchmarkId::new("flat_roundtrip", &label), &(), |b, _| {
            b.iter(|| {
                flat.read_many(&mut requests).unwrap();
                black_box(&requests);
            })
        });
        group.bench_with_input(BenchmarkId::new("validate_result", &label), &(), |b, _| {
            b.iter(|| validate_read_plan_result(black_box(&output)).unwrap())
        });
        let mut copy = output.clone();
        group.bench_with_input(BenchmarkId::new("result_copy", &label), &(), |b, _| {
            b.iter(|| {
                copy.clear();
                copy.extend_from_slice(black_box(&output));
                black_box(&copy);
            })
        });
    }
    group.bench_function("absolute_deadline_clock", |b| {
        b.iter(|| black_box(Instant::now() + Duration::from_secs(1)))
    });
    group.finish();
}
