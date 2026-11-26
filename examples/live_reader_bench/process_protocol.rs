//! Control and receipts for the release-only measurement path; not an ABI extension.
use serde::{Deserialize, Serialize};
use vosu_osu::bench_capture::Lane;

pub const OSU: &str = "vosu.osu";
pub const CONTENDER: &str = "bench.contender";
pub const HOLD: &str = "bench.hold";
pub const RELEASE: &str = "bench.release";
pub const FLOOD: &str = "bench.flood";
pub const SNAPSHOT: &str = "bench.snapshot";
pub const BEGIN: &str = "bench.begin";
pub const FIXTURE: &str = "fixture-base";
pub const HELD: &str = "dispatch-held";
pub const FLOOD_FRAMES: usize = 63;
pub const FLOOD_BYTES: usize = 1000 * 1024;

#[derive(Debug, Deserialize, Serialize)]
pub struct Snapshot {
    pub pid: u32,
    pub cpu_ns: u64,
    pub lanes: [Lane; 2],
    pub precise_demand: bool,
    pub held: bool,
    pub flood_frames: u64,
    pub flood_bytes: u64,
    pub contender_reads: u64,
    pub wire_frames: u64,
    pub control_frames: u64,
    pub wire_read_submissions: u64,
    pub wire_read_results: u64,
}

pub fn receipt(n: u64) -> String {
    format!("snapshot-{n}.json")
}
