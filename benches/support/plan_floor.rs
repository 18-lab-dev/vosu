//! Prebuilt real reader graph: excludes adapter construction and reader decoding.
//! This is a diagnostic lower bound, not the full-reader D17 acceptance metric.
use super::{ExecutorHost, lazer_fixture, populated_precise};
use criterion::Criterion;
use std::{
    hint::black_box,
    sync::{Arc, Mutex},
};
use vosu_osu::memory::PlanProcessMemory;
use vosu_osu_reader::{
    memory::{LazerMemory, LazerPattern, ReaderSynchronization, lazer_offsets::LazerOffsets},
    process::ProcessHandle,
};
use vosu_plugin_api::{
    HostServices, Message, PlanMemory, ReadPlanSubmit, StageRef, SyncReadBuffers, vosu_opcode,
    vosu_status,
};
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};

struct RecordingMemory {
    inner: Arc<lazer_fixture::Fixture>,
    reads: Arc<Mutex<Vec<(usize, usize)>>>,
}
impl ProcessMemory for RecordingMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.inner.read_exact(address, dst)?;
        self.reads.lock().unwrap().push((address, dst.len()));
        Ok(())
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.inner.read_many(reads)?;
        self.reads
            .lock()
            .unwrap()
            .extend(reads.iter().map(|read| (read.address, read.dst.len())));
        Ok(())
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        self.inner.readable_regions()
    }
}
struct RecordingHost {
    inner: ExecutorHost,
    plans: Arc<Mutex<Vec<ReadPlanSubmit>>>,
}
impl HostServices for RecordingHost {
    fn send(&self, _: &Message, _: u64) -> vosu_status {
        unreachable!("capture uses flat submissions")
    }
    fn send_flat(
        &self,
        opcode: vosu_opcode,
        flags: u32,
        correlation: u64,
        payload: &[u8],
    ) -> vosu_status {
        let Message::ReadPlanSubmit(plan) = Message::decode(opcode, flags, payload).unwrap() else {
            unreachable!("capture submits read plans only")
        };
        self.plans.lock().unwrap().push(plan);
        self.inner.send_flat(opcode, flags, correlation, payload)
    }
}

pub(super) fn bench(c: &mut Criterion) {
    let offsets = Arc::new(LazerOffsets::embedded().unwrap());
    let fixture = Arc::new(lazer_fixture::Fixture::active_lazer(&offsets));
    let synchronization = ReaderSynchronization {
        game_base: Some(fixture.game_base),
        current_screen: Some(fixture.player),
    };
    let plans = Arc::new(Mutex::new(Vec::new()));
    let reads = Arc::new(Mutex::new(Vec::new()));
    let memory = PlanProcessMemory::new(
        RecordingHost {
            inner: ExecutorHost::with_memory(
                ProcessBitness::X64,
                Arc::new(RecordingMemory {
                    inner: fixture.clone(),
                    reads: reads.clone(),
                }),
            ),
            plans: plans.clone(),
        },
        1,
        std::process::id(),
        ProcessBitness::X64,
    );
    let mut reader = LazerMemory::new(
        ProcessHandle::new(Arc::new(memory), ProcessBitness::X64),
        offsets,
        LazerPattern::source_default(),
    );
    populated_precise(&mut reader, synchronization);
    let plans = std::mem::take(&mut *plans.lock().unwrap());
    let reads = std::mem::take(&mut *reads.lock().unwrap());
    drop(reader);
    let stages: Vec<Vec<_>> = plans
        .iter()
        .map(|plan| {
            plan.stages
                .iter()
                .map(|stage| StageRef {
                    base: stage.base,
                    base_offset: stage.base_offset,
                    ops: &stage.ops,
                })
                .collect()
        })
        .collect();
    let requested: usize = reads.iter().map(|(_, len)| len).sum();
    eprintln!(
        "frozen_floor submits={} ops={} requested={requested}",
        plans.len(),
        reads.len()
    );
    let mut destinations: Vec<_> = reads.iter().map(|(_, len)| vec![0; *len]).collect();
    let mut group = c.benchmark_group("plan_floor");
    group.bench_function("native", |b| {
        b.iter(|| {
            for ((address, _), dst) in reads.iter().zip(&mut destinations) {
                fixture.read_exact(*address, dst).unwrap();
                black_box(&*dst);
            }
        })
    });
    let submissions: Vec<_> = plans
        .iter()
        .map(|plan| Message::ReadPlanSubmit(plan.clone()).encode_payload().unwrap())
        .collect();
    let mut output = Vec::new();
    let mut replies = Vec::new();
    for submit in &submissions {
        vosu_core::plugin::execute_sync_into(
            submit, vosu_plugin_api::VOSU_READ_PLAN_SYNC, fixture.as_ref(),
            ProcessBitness::X64, &super::Modules, std::time::Duration::from_secs(1),
            &mut output,
        ).unwrap();
        replies.push(output.clone());
    }
    group.bench_function("execute", |b| {
        b.iter(|| {
            for submit in &submissions {
                vosu_core::plugin::execute_sync_into(
                    black_box(submit), vosu_plugin_api::VOSU_READ_PLAN_SYNC, fixture.as_ref(),
                    ProcessBitness::X64, &super::Modules, std::time::Duration::from_secs(1),
                    &mut output,
                ).unwrap();
                black_box(&output);
            }
        })
    });
    let mut encoded = Vec::new();
    group.bench_function("encode", |b| {
        b.iter(|| {
            for (plan, stages) in plans.iter().zip(&stages) {
                vosu_plugin_api::message::flat::encode_submit_into(
                    &mut encoded, plan.plan, plan.handle, &plan.root, stages,
                );
                black_box(&encoded);
            }
        })
    });
    group.bench_function("validate_results", |b| {
        b.iter(|| {
            for reply in &replies {
                vosu_plugin_api::validate_read_plan_result(black_box(reply)).unwrap();
            }
        })
    });
    let mut copied = Vec::new();
    group.bench_function("copy_results", |b| {
        b.iter(|| {
            for reply in &replies {
                copied.clear();
                copied.extend_from_slice(black_box(reply));
                black_box(&copied);
            }
        })
    });
    let mut buffers = SyncReadBuffers::new();
    let flat = ExecutorHost::with_memory(ProcessBitness::X64, fixture.clone());
    group.bench_function("flat", |b| {
        b.iter(|| {
            replay(&flat, &plans, &stages, &mut buffers);
        })
    });
    #[cfg(windows)]
    {
        let attachment = super::routed::Attachment::new(
            super::routed::native_record(std::process::id()).unwrap(),
            "vosu.bench.frozen-floor",
        )
        .unwrap();
        group.bench_function("routed", |b| {
            b.iter(|| {
                let (host, handle) = attachment.as_ref();
                replay_with_handle(host, *handle, &plans, &stages, &mut buffers);
            })
        });
    }
    group.finish();
}
fn replay(
    host: &impl HostServices,
    plans: &[ReadPlanSubmit],
    stages: &[Vec<StageRef<'_>>],
    buffers: &mut SyncReadBuffers,
) {
    replay_with_handle(host, 1, plans, stages, buffers);
}
fn replay_with_handle(
    host: &impl HostServices,
    handle: u64,
    plans: &[ReadPlanSubmit],
    stages: &[Vec<StageRef<'_>>],
    buffers: &mut SyncReadBuffers,
) {
    for (plan, stages) in plans.iter().zip(stages) {
        let result = PlanMemory::new()
            .read_flat(host, plan.plan, handle, plan.root.clone(), stages, buffers)
            .unwrap();
        for index in 0..result.len() {
            black_box(result.get(index));
        }
    }
}
