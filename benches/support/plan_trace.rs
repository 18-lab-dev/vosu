//! One untimed real-executor capture; never wraps a timed benchmark transport.
use super::{ExecutorHost, lazer_fixture};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vosu_osu::memory::PlanProcessMemory;
use vosu_osu_reader::{
    memory::{
        LazerMemory, LazerPattern, MemoryReader, ReaderSynchronization, SynchronizedMemoryReader,
        lazer_offsets::LazerOffsets,
    },
    process::ProcessHandle,
};
use vosu_plugin_api::memory::ProcessBitness;
use vosu_plugin_api::{
    HostServices, Message, read_flat_header, vosu_opcode, vosu_plan_op,
    vosu_read_plan_submit_header, vosu_stage_header, vosu_status,
};
struct TraceHost {
    inner: ExecutorHost,
    phase: Arc<AtomicUsize>,
}
impl HostServices for TraceHost {
    fn send(&self, message: &Message, correlation: u64) -> vosu_status {
        self.inner.send(message, correlation)
    }
    fn send_flat(
        &self,
        opcode: vosu_opcode,
        flags: u32,
        correlation: u64,
        payload: &[u8],
    ) -> vosu_status {
        let status = self.inner.send_flat(opcode, flags, correlation, payload);
        let header: vosu_read_plan_submit_header = read_flat_header(payload).unwrap();
        let mut ops = 0;
        let mut requested = 0;
        for index in 0..header.stage_count as usize {
            let start = header.stages_offset as usize + index * size_of::<vosu_stage_header>();
            let stage: vosu_stage_header = read_flat_header(&payload[start..]).unwrap();
            ops += stage.op_count;
            for index in 0..stage.op_count as usize {
                let start = stage.ops_offset as usize + index * size_of::<vosu_plan_op>();
                let op: vosu_plan_op = read_flat_header(&payload[start..]).unwrap();
                requested += op.length;
            }
        }
        eprintln!(
            "fixture_phase={} stages={} ops={ops} requested={requested} submit={} result={}",
            self.phase.load(Ordering::Relaxed),
            header.stage_count,
            payload.len(),
            self.inner.output.lock().unwrap().len()
        );
        status
    }
}
pub(super) fn capture() {
    let offsets = Arc::new(LazerOffsets::embedded().unwrap());
    let fixture = Arc::new(lazer_fixture::Fixture::active_lazer(&offsets));
    let sync = ReaderSynchronization {
        game_base: Some(fixture.game_base),
        current_screen: Some(fixture.player),
    };
    let phase = Arc::new(AtomicUsize::new(0));
    let memory = PlanProcessMemory::new(
        TraceHost {
            inner: ExecutorHost::with_memory(ProcessBitness::X64, fixture),
            phase: phase.clone(),
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
    reader.synchronize_precise(sync, 3);
    phase.store(1, Ordering::Relaxed);
    let global = reader.global_precise().unwrap();
    phase.store(2, Ordering::Relaxed);
    let keys = reader.key_overlay(3).unwrap();
    phase.store(3, Ordering::Relaxed);
    let errors = reader.hit_errors(0).unwrap();
    std::hint::black_box((global, keys, errors));
}
