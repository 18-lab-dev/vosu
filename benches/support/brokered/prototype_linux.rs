//! The Linux prototype variants of `docs/v2/brokered-reads.md`, unchanged in
//! protocol and method: `uds`, `shm` and `naive` fork an unprivileged
//! "plugin host" child that times its own round trips; core (this process)
//! serves every plan with [`serve`]. CPU is `getrusage`: `RUSAGE_SELF` for
//! core and `RUSAGE_CHILDREN` for the reaped child, both over the whole run
//! (warm-up included) divided by the timed sample count, as the historical
//! numbers were measured.

use std::io::{Read, Write};
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::ptr;
use std::time::Instant;

use super::plans::{Plan, ReadOp, Source};
use super::stats::{Percentiles, percentiles};
use super::target::Target;
use super::wire::serve;

/// One prototype variant's result.
pub struct Outcome {
    pub timing: Percentiles,
    /// Core and plugin CPU, nanoseconds per timed sample.
    pub core_cpu_ns: f64,
    pub plugin_cpu_ns: f64,
    /// The first complete plan response core produced, for
    /// `result_sha256`.
    pub first_response: Vec<u8>,
}

/// Flattens stage/op order into the sequence a naive per-read caller would
/// issue, each tagged with its own `(stage, index)` so later ops can name it
/// as a dependency.
fn flattened(plan: &Plan) -> Vec<((usize, usize), ReadOp)> {
    plan.stages
        .iter()
        .enumerate()
        .flat_map(|(stage_index, stage)| {
            stage
                .iter()
                .enumerate()
                .map(move |(op_index, op)| ((stage_index, op_index), *op))
        })
        .collect()
}

fn write_frame(stream: &mut UnixStream, payload: &[u8]) {
    stream
        .write_all(&(payload.len() as u32).to_le_bytes())
        .expect("write frame length");
    stream.write_all(payload).expect("write frame payload");
}

fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut len_bytes = [0u8; 4];
    stream
        .read_exact(&mut len_bytes)
        .expect("read frame length");
    let len = u32::from_le_bytes(len_bytes) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).expect("read frame payload");
    payload
}

// --- CPU accounting ---

fn cpu_now() -> libc::rusage {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `RUSAGE_SELF` with a correctly sized, zero-initialized out
    // pointer is the documented `getrusage` contract; the kernel only writes
    // through it.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(result, 0, "getrusage(RUSAGE_SELF) failed");
    // SAFETY: getrusage succeeded, so every field is initialized.
    unsafe { usage.assume_init() }
}

fn cpu_children() -> libc::rusage {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: same contract as `cpu_now`, with `RUSAGE_CHILDREN` aggregating
    // every waited-for child; called only after `waitpid` reaps the child.
    let result = unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, usage.as_mut_ptr()) };
    assert_eq!(result, 0, "getrusage(RUSAGE_CHILDREN) failed");
    // SAFETY: getrusage succeeded, so every field is initialized.
    unsafe { usage.assume_init() }
}

fn cpu_total_ns(usage: &libc::rusage) -> u128 {
    let user =
        usage.ru_utime.tv_sec as u128 * 1_000_000_000 + usage.ru_utime.tv_usec as u128 * 1_000;
    let sys =
        usage.ru_stime.tv_sec as u128 * 1_000_000_000 + usage.ru_stime.tv_usec as u128 * 1_000;
    user + sys
}

// --- fork/exit helpers shared by the brokered variants ---

/// # Safety
/// Caller must fork before starting any extra threads in this process (true
/// for every call site below: the bench never spawns Rust threads) so the
/// child inherits a single-threaded, consistent runtime.
unsafe fn fork() -> i32 {
    // SAFETY: documented by the function; no locks are held across the fork
    // boundary because nothing here has started a second thread.
    unsafe { libc::fork() }
}

fn waitpid(pid: i32) {
    let mut status = 0i32;
    // SAFETY: `pid` names a real child of this process from a prior `fork`.
    let result = unsafe { libc::waitpid(pid, &mut status, 0) };
    assert_eq!(result, pid, "waitpid for forked plugin host");
}

// --- Variant: brokered over a Unix domain socket ---

pub fn run_uds(target: &Target, plan_bytes: &[u8], warmup: usize, samples: usize) -> Outcome {
    let (mut plugin_end, mut core_end) = UnixStream::pair().expect("create socketpair");
    let self_cpu_before = cpu_now();
    let children_cpu_before = cpu_children();

    // SAFETY: see `fork`'s contract; this process has started no threads.
    let pid = unsafe { fork() };
    assert!(pid >= 0, "fork brokered-uds plugin host");
    if pid == 0 {
        drop(core_end);
        for _ in 0..warmup {
            write_frame(&mut plugin_end, plan_bytes);
            read_frame(&mut plugin_end);
        }
        let mut timings = Vec::with_capacity(samples);
        for _ in 0..samples {
            let start = Instant::now();
            write_frame(&mut plugin_end, plan_bytes);
            read_frame(&mut plugin_end);
            timings.push(start.elapsed().as_nanos());
        }
        let encoded = encode_percentiles(&percentiles(timings));
        write_frame(&mut plugin_end, &encoded);
        std::process::exit(0);
    }

    drop(plugin_end);
    let mut first_response = None;
    loop {
        let request = read_frame(&mut core_end);
        if request.len() == 24 {
            // A 24-byte frame is the child's encoded percentiles, not a plan.
            let summary = decode_percentiles(&request);
            waitpid(pid);
            let core_cpu = cpu_total_ns(&cpu_now()) - cpu_total_ns(&self_cpu_before);
            let plugin_cpu = cpu_total_ns(&cpu_children()) - cpu_total_ns(&children_cpu_before);
            return Outcome {
                timing: summary,
                core_cpu_ns: core_cpu as f64 / samples as f64,
                plugin_cpu_ns: plugin_cpu as f64 / samples as f64,
                first_response: first_response.expect("uds served a plan"),
            };
        }
        let response = serve(target.process.as_ref(), target.root, &request);
        write_frame(&mut core_end, &response);
        first_response.get_or_insert(response);
    }
}

fn encode_percentiles(percentiles: &Percentiles) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(24);
    bytes.extend(percentiles.p50_ns.to_le_bytes());
    bytes.extend(percentiles.p99_ns.to_le_bytes());
    bytes.extend(percentiles.mean_ns.to_le_bytes());
    bytes
}

fn decode_percentiles(bytes: &[u8]) -> Percentiles {
    Percentiles {
        p50_ns: f64::from_le_bytes(bytes[0..8].try_into().unwrap()),
        p99_ns: f64::from_le_bytes(bytes[8..16].try_into().unwrap()),
        mean_ns: f64::from_le_bytes(bytes[16..24].try_into().unwrap()),
    }
}

// --- Variant: brokered over shared memory with eventfd wakeups ---

const SHM_SLOT_BYTES: usize = 64 * 1024;

struct SharedRegion {
    pointer: *mut u8,
}

// SAFETY: the region is only ever accessed strictly alternately by the two
// processes sharing it (request write -> signal -> response write -> signal),
// enforced by the eventfd read/write syscalls below, so no two accesses race.
unsafe impl Send for SharedRegion {}
// SAFETY: see the comment above; the same strict-alternation protocol rules
// out any concurrent access this `Sync` impl would otherwise have to justify.
unsafe impl Sync for SharedRegion {}

impl SharedRegion {
    fn new() -> Self {
        // SAFETY: `MAP_SHARED | MAP_ANONYMOUS` with a null address, no fd, and
        // a non-zero length is the documented contract for a mapping that
        // stays shared and valid across a later `fork`.
        let pointer = unsafe {
            libc::mmap(
                ptr::null_mut(),
                SHM_SLOT_BYTES * 2,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(pointer, libc::MAP_FAILED, "mmap shared region");
        Self {
            pointer: pointer.cast(),
        }
    }

    fn write_slot(&self, slot: usize, payload: &[u8]) {
        assert!(
            payload.len() + 4 <= SHM_SLOT_BYTES,
            "payload fits in shm slot"
        );
        // SAFETY: `slot` is 0 or 1 (request/response), each slot is
        // `SHM_SLOT_BYTES` and the mapping covers two slots; the eventfd
        // protocol guarantees the other side is not reading this slot now.
        unsafe {
            let base = self.pointer.add(slot * SHM_SLOT_BYTES);
            ptr::copy_nonoverlapping((payload.len() as u32).to_le_bytes().as_ptr(), base, 4);
            ptr::copy_nonoverlapping(payload.as_ptr(), base.add(4), payload.len());
        }
    }

    fn read_slot(&self, slot: usize) -> Vec<u8> {
        // SAFETY: same slot/protocol contract as `write_slot`.
        unsafe {
            let base = self.pointer.add(slot * SHM_SLOT_BYTES);
            let mut len_bytes = [0u8; 4];
            ptr::copy_nonoverlapping(base, len_bytes.as_mut_ptr(), 4);
            let len = u32::from_le_bytes(len_bytes) as usize;
            let mut payload = vec![0u8; len];
            ptr::copy_nonoverlapping(base.add(4), payload.as_mut_ptr(), len);
            payload
        }
    }
}

fn eventfd_new() -> RawFd {
    // SAFETY: `eventfd` has no memory-safety preconditions. The fd is owned
    // by the caller and survives `fork`; close-on-exec keeps it out of the
    // candidate helpers spawned later.
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
    assert!(fd >= 0, "create eventfd");
    fd
}

fn eventfd_signal(fd: RawFd) {
    let value: u64 = 1;
    // SAFETY: `fd` is a live eventfd and `value` is an 8-byte buffer, the
    // exact write contract eventfd documents.
    let result = unsafe { libc::write(fd, (&raw const value).cast(), 8) };
    assert_eq!(result, 8, "signal eventfd");
}

fn eventfd_wait(fd: RawFd) {
    let mut value: u64 = 0;
    // SAFETY: `fd` is a live eventfd and `value` is an 8-byte buffer, the
    // exact read contract eventfd documents; it blocks until signaled.
    let result = unsafe { libc::read(fd, (&raw mut value).cast(), 8) };
    assert_eq!(result, 8, "wait on eventfd");
}

pub fn run_shm(target: &Target, plan_bytes: &[u8], warmup: usize, samples: usize) -> Outcome {
    let region = SharedRegion::new();
    let request_ready = eventfd_new();
    let response_ready = eventfd_new();
    let (mut plugin_summary, mut core_summary) = UnixStream::pair().expect("create summary pipe");
    let self_cpu_before = cpu_now();
    let children_cpu_before = cpu_children();

    // SAFETY: see `fork`'s contract; this process has started no threads.
    let pid = unsafe { fork() };
    assert!(pid >= 0, "fork brokered-shm plugin host");
    if pid == 0 {
        drop(core_summary);
        for _ in 0..warmup {
            region.write_slot(0, plan_bytes);
            eventfd_signal(request_ready);
            eventfd_wait(response_ready);
            region.read_slot(1);
        }
        let mut timings = Vec::with_capacity(samples);
        for _ in 0..samples {
            let start = Instant::now();
            region.write_slot(0, plan_bytes);
            eventfd_signal(request_ready);
            eventfd_wait(response_ready);
            region.read_slot(1);
            timings.push(start.elapsed().as_nanos());
        }
        let encoded = encode_percentiles(&percentiles(timings));
        write_frame(&mut plugin_summary, &encoded);
        std::process::exit(0);
    }

    drop(plugin_summary);
    let mut first_response = None;
    for _ in 0..warmup + samples {
        eventfd_wait(request_ready);
        let response = serve(target.process.as_ref(), target.root, &region.read_slot(0));
        region.write_slot(1, &response);
        eventfd_signal(response_ready);
        first_response.get_or_insert(response);
    }
    let summary = decode_percentiles(&read_frame(&mut core_summary));
    waitpid(pid);
    let core_cpu = cpu_total_ns(&cpu_now()) - cpu_total_ns(&self_cpu_before);
    let plugin_cpu = cpu_total_ns(&cpu_children()) - cpu_total_ns(&children_cpu_before);
    for fd in [request_ready, response_ready] {
        // SAFETY: both eventfds are owned here and no longer used.
        unsafe { libc::close(fd) };
    }
    Outcome {
        timing: summary,
        core_cpu_ns: core_cpu as f64 / samples as f64,
        plugin_cpu_ns: plugin_cpu as f64 / samples as f64,
        first_response: first_response.expect("shm served a plan"),
    }
}

// --- Variant: naive per-read RPC (no plan batching) ---

pub fn run_naive(target: &Target, plan: &Plan, warmup: usize, samples: usize) -> Outcome {
    let flattened = flattened(plan);
    let (mut plugin_end, mut core_end) = UnixStream::pair().expect("create socketpair");
    let self_cpu_before = cpu_now();
    let children_cpu_before = cpu_children();

    // SAFETY: see `fork`'s contract; this process has started no threads.
    let pid = unsafe { fork() };
    assert!(pid >= 0, "fork naive-per-read plugin host");
    if pid == 0 {
        drop(core_end);
        let run_once = |plugin_end: &mut UnixStream| {
            let mut resolved: std::collections::HashMap<(usize, usize), u64> =
                std::collections::HashMap::new();
            for (key, op) in &flattened {
                let base = match op.source {
                    Source::Root => 0u64,
                    Source::Prev(stage, index) => resolved[&(stage, index)],
                };
                let mut request = Vec::with_capacity(13);
                request.push(u8::from(matches!(op.source, Source::Root)));
                request.extend(base.to_le_bytes());
                request.extend(op.offset.to_le_bytes());
                request.extend(op.len.to_le_bytes());
                write_frame(plugin_end, &request);
                let response = read_frame(plugin_end);
                if op.as_pointer {
                    resolved.insert(*key, u64::from_le_bytes(response[..8].try_into().unwrap()));
                }
            }
        };
        for _ in 0..warmup {
            run_once(&mut plugin_end);
        }
        let mut timings = Vec::with_capacity(samples);
        for _ in 0..samples {
            let start = Instant::now();
            run_once(&mut plugin_end);
            timings.push(start.elapsed().as_nanos());
        }
        let encoded = encode_percentiles(&percentiles(timings));
        write_frame(&mut plugin_end, &encoded);
        std::process::exit(0);
    }

    drop(plugin_end);
    // The first plan's worth of per-read responses, concatenated.
    let mut first_response = Vec::with_capacity(plan.bytes());
    let mut first_reads = 0;
    loop {
        let request = read_frame(&mut core_end);
        if request.len() == 24 {
            let summary = decode_percentiles(&request);
            waitpid(pid);
            let core_cpu = cpu_total_ns(&cpu_now()) - cpu_total_ns(&self_cpu_before);
            let plugin_cpu = cpu_total_ns(&cpu_children()) - cpu_total_ns(&children_cpu_before);
            return Outcome {
                timing: summary,
                core_cpu_ns: core_cpu as f64 / samples as f64,
                plugin_cpu_ns: plugin_cpu as f64 / samples as f64,
                first_response,
            };
        }
        let is_root = request[0] != 0;
        let base = u64::from_le_bytes(request[1..9].try_into().unwrap());
        let offset = i64::from_le_bytes(request[9..17].try_into().unwrap());
        let len = u32::from_le_bytes(request[17..21].try_into().unwrap());
        let root_or_base = if is_root { target.root } else { base };
        let address = root_or_base.wrapping_add_signed(offset) as usize;
        let mut buffer = vec![0u8; len as usize];
        target
            .process
            .read_exact(address, &mut buffer)
            .expect("core executes naive single read");
        write_frame(&mut core_end, &buffer);
        if first_reads < plan.reads() {
            first_response.extend_from_slice(&buffer);
            first_reads += 1;
        }
    }
}
