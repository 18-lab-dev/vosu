//! Real-process native/PluginHost diagnostic. Windows (no test-harness required):
//! `$env:VOSU_READ_PLAN_PROFILE='1'; cargo run --release --locked --features read-plan-profile --example plan_process_bench -- --pid 1234 --cycles 1000`
//! Optional `--address 0x...` overrides the first readable region. The synthetic
//! stable/lazer widths repeat that readable address, not arbitrary pointer chases.
//! Profiling adds two QPC reads per span; application/profiler clocks are separate.
//! Without `--pid`, debug `--features test-harness` runs the populated child fixture.
use std::sync::Arc;
#[cfg(feature = "test-harness")]
use std::{
    hint::black_box,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::sync_channel,
    time::{Duration, Instant},
};
#[cfg(feature = "test-harness")]
use vosu_osu_reader::{
    memory::{MemoryReader, StableMemory},
    process::ProcessHandle,
};
use vosu_sys::process::ProcessMemory;
#[cfg(feature = "test-harness")]
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessBitness, ReadRequest};
#[path = "plan_process_bench/pid.rs"]
mod pid;
#[path = "live_reader_bench/plan.rs"]
pub mod plan;
#[cfg(feature = "test-harness")]
#[path = "../benches/support/stable_fixture.rs"]
mod stable_fixture;

type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

#[cfg(feature = "test-harness")]
struct ChildGuard(Child);
#[cfg(feature = "test-harness")]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// Fixture pointers are x86 offsets. Translating only at the OS boundary preserves
#[cfg(feature = "test-harness")]
// the identical populated graph, batching and pointer-chase shape on a 64-bit child.
struct Translated {
    process: Arc<dyn ProcessMemory>,
    base: usize,
}
#[cfg(feature = "test-harness")]
impl ProcessMemory for Translated {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> std::result::Result<(), MemoryError> {
        self.process.read_exact(self.base + address, dst)
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> std::result::Result<(), MemoryError> {
        for read in reads.iter_mut() {
            read.address += self.base;
        }
        let result = self.process.read_many(reads);
        for read in reads {
            read.address -= self.base;
        }
        result
    }
    fn readable_regions(&self) -> std::result::Result<Vec<MemoryRegion>, MemoryError> {
        // No signature scans are part of this known-address fixture diagnostic.
        Err(MemoryError::Backend {
            operation: "fixture regions",
            message: "diagnostic uses resolved fixture addresses".into(),
        })
    }
    fn is_alive(&self) -> bool {
        self.process.is_alive()
    }
    fn reads_pin_process_identity(&self) -> bool {
        self.process.reads_pin_process_identity()
    }
}

#[cfg(target_os = "linux")]
fn open(pid: u32) -> Result<Arc<dyn ProcessMemory>> {
    Ok(Arc::new(vosu_sys::process::open_process(pid)?))
}
#[cfg(windows)]
fn open(pid: u32) -> Result<Arc<dyn ProcessMemory>> {
    Ok(Arc::new(vosu_sys::process::WindowsProcessMemory::open(
        pid,
    )?))
}

#[cfg(feature = "test-harness")]
fn child() -> Result<()> {
    let bytes = stable_fixture::bytes();
    println!("{}", bytes.as_ptr() as usize);
    std::io::stdout().flush()?;
    let (tx, rx) = sync_channel(1);
    std::thread::spawn(move || {
        let result = std::io::stdin().read_exact(&mut [0]);
        let _ = tx.send(result);
    });
    // Keep the allocation alive, but never leave an orphaned diagnostic child.
    let _ = rx.recv_timeout(Duration::from_secs(120));
    black_box(bytes);
    Ok(())
}

#[cfg(feature = "test-harness")]
fn capture(shape: &str, handle: &ProcessHandle, reader: &mut StableMemory) {
    match shape {
        "one_op" => {
            black_box(handle.read_u32(0).unwrap());
        }
        "precise_nonplaying" => {
            black_box(reader.global_precise().unwrap());
        }
        "regular" => {
            black_box(stable_fixture::capture(reader));
        }
        _ => unreachable!(),
    }
}

#[cfg(feature = "test-harness")]
fn fixture_run() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--child") {
        return child();
    }
    let mut child = ChildGuard(
        Command::new(std::env::current_exe()?)
            .arg("--child")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?,
    );
    let stdout = child.0.stdout.take().ok_or("missing child stdout")?;
    let (tx, rx) = sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = tx.send(result);
    });
    let base = rx.recv_timeout(Duration::from_secs(5))??.trim().parse()?;
    let pid = child.0.id();
    let memory: Arc<dyn ProcessMemory> = Arc::new(Translated {
        process: open(pid)?,
        base,
    });
    let native = ProcessHandle::new(Arc::clone(&memory), ProcessBitness::X86);
    vosu_core::plugin::test_support::set_session_memory(pid, memory, ProcessBitness::X86);
    struct Cleanup(u32);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            vosu_core::plugin::test_support::clear_session_memory(self.0);
        }
    }
    let _cleanup = Cleanup(pid);
    let mut record = plan::native_record(pid)?;
    record.bitness = vosu_plugin_api::Bitness::X86;
    let attachment = plan::Attachment::new(record, "vosu.bench.real-process")?;
    let mut native_reader = StableMemory::new(native.clone(), stable_fixture::patterns());
    let mut plan_reader = StableMemory::new(attachment.process.clone(), stable_fixture::patterns());
    stable_fixture::assert_capture(&mut native_reader);
    stable_fixture::assert_capture(&mut plan_reader);
    const SAMPLES: usize = 2_000;
    for shape in ["one_op", "precise_nonplaying", "regular"] {
        let mut native_times = Vec::with_capacity(SAMPLES);
        let mut plan_times = Vec::with_capacity(SAMPLES);
        for round in 0..SAMPLES + 100 {
            let plan_first = round % 2 == 0;
            for is_plan in [plan_first, !plan_first] {
                let start = Instant::now();
                if is_plan {
                    capture(shape, &attachment.process, &mut plan_reader);
                } else {
                    capture(shape, &native, &mut native_reader);
                }
                let elapsed = start.elapsed().as_nanos();
                if round >= 100 {
                    if is_plan {
                        plan_times.push(elapsed);
                    } else {
                        native_times.push(elapsed);
                    }
                }
            }
        }
        native_times.sort_unstable();
        plan_times.sort_unstable();
        let native_ns = native_times[SAMPLES / 2];
        let plan_ns = plan_times[SAMPLES / 2];
        println!(
            "{shape}: native_ns={native_ns} plan_ns={plan_ns} ratio={:.3}",
            plan_ns as f64 / native_ns as f64
        );
        if vosu_plugin_api::read_plan_profile::COMPILED_IN
            && std::env::var_os("VOSU_READ_PLAN_PROFILE").is_some_and(|value| value == "1")
        {
            vosu_plugin_api::read_plan_profile::take_thread_snapshot();
            capture(shape, &attachment.process, &mut plan_reader);
            let clocks = vosu_plugin_api::read_plan_profile::take_thread_snapshot();
            println!(
                "  plan_clock_reads_per_capture: precise={} coarse={} profiler={}",
                clocks.precise_clock_reads, clocks.coarse_clock_reads, clocks.profiler_clock_reads
            );
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--pid") {
        return pid::run(std::env::args().skip(1));
    }
    #[cfg(feature = "test-harness")]
    return fixture_run();
    #[cfg(not(feature = "test-harness"))]
    Err("pass --pid PID, or enable test-harness for the populated child fixture".into())
}
