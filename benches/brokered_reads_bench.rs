//! Brokered memory-read cost measurement for ADR 0019 (`process`-runtime plugins).
//!
//! ## Plan shape derivation
//!
//! ADR 0019 brokers a `process`-runtime plugin's game-memory reads through core:
//! the plugin sends one batched read plan (base address, offset chain, pointer
//! dereferences, byte lengths); core executes it against the matched process and
//! returns the bytes. This bench measures that round trip's cost against direct
//! in-process reads, using two plan shapes derived from the 1.x readers:
//!
//! - **Precise** (polled every gameplay tick): `src/memory/lazer/reader.rs`'s
//!   `key_overlay` and `global_precise` chase a deep, mostly-sequential pointer
//!   chain (game base -> HUD overlay -> input-count controller -> trigger list
//!   -> up to 18 per-key trigger objects -> count/flag fields), matching
//!   `benches/memory_reader_bench.rs`'s `Fixture::active_lazer`. Running that
//!   bench (`cargo bench --locked --bench memory_reader_bench`) measured, for one
//!   precise cycle (global + 18-key overlay + hit errors):
//!   `exact_reads_per_cycle=24`, `batch_reads_per_cycle=2`,
//!   `process_bytes_per_cycle=912` (26 total native calls). This bench's
//!   `precise_plan()` models the same shape structurally: one round trip per
//!   sequential pointer hop, plus two wide batched stages for the per-key reads
//!   (`src/memory/lazer/global_batch.rs`'s `Stage<N>` groups independent reads
//!   the same way), for 8 stages / 43 reads / 358 bytes.
//! - **Regular** (polled once per published snapshot): `src/state/readers.rs`'s
//!   `capture_regular` fans out to independent decoder calls (`global`, `menu`,
//!   `mp3_length`, `settings`, conditionally `gameplay`/`leaderboard`), each of
//!   which batches its own fields in one or two `Stage<N>` calls per
//!   `src/memory/lazer/global_batch.rs` (seen there up to `Stage::<13>`). This
//!   bench's `regular_plan()` models one pointer-resolution stage fetching four
//!   independent decoder targets, then one batched field read per target, for 5
//!   stages / 8 reads / 116 bytes — wide and shallow, unlike the precise plan's
//!   narrow and deep chain, which is why the two shapes diverge most in round
//!   trip count rather than byte count.
//!
//! Both shapes are synthetic (see `examples/brokered_reads_target.rs`), not a
//! byte-for-byte replay of the real decoders; the comparison that matters here is
//! IPC transport cost for a given round-trip/op shape, not decoder fidelity.
//!
//! ## Variants
//!
//! - `direct`: the bench process reads the target over `process_vm_readv`
//!   itself (`vosu::process::open_process`), as a 1.x / in-process plugin does.
//! - `uds`: a forked, unprivileged "plugin host" child sends the whole encoded
//!   plan to the parent ("core") over a `UnixStream` pair and gets back one
//!   response frame with every stage's bytes; core executes the dependent
//!   multi-stage chain locally, so this is one round trip per sample regardless
//!   of stage count.
//! - `shm`: identical protocol, transported over a `MAP_SHARED` region with two
//!   `eventfd`s for request/response wakeups instead of a socket.
//! - `naive`: the plugin host does the pointer chasing itself, issuing one
//!   request/response round trip per individual read instruction (so 43 round
//!   trips/sample for the precise plan) — the case the brokered plan message
//!   exists to avoid.
//!
//! Run with `taskset -c <idle-cpus>` so the bench and its forked children do not
//! share cores with anything else; CPUs reserved for a VM should be excluded.
//! Reproduce: `taskset -c 2-5 cargo bench --locked --bench brokered_reads_bench`.

#[cfg(target_os = "linux")]
mod linux_impl {
    use std::io::{Read, Write};
    use std::os::fd::RawFd;
    use std::os::unix::net::UnixStream;
    use std::process::{Child, Command, Stdio};
    use std::ptr;
    use std::sync::Arc;
    use std::time::Instant;

    use vosu_plugin_api::memory::{MemoryError, ProcessMemory, ReadRequest};
    use vosu_sys::process::open_process;

    const SAMPLES: usize = 5_000;
    const WARMUP: usize = 200;

    #[derive(Clone, Copy, Debug)]
    enum Source {
        Root,
        Prev(usize, usize),
    }

    #[derive(Clone, Copy, Debug)]
    struct ReadOp {
        source: Source,
        offset: i64,
        len: u32,
        as_pointer: bool,
    }

    impl ReadOp {
        const fn new(source: Source, offset: i64, len: u32, as_pointer: bool) -> Self {
            Self {
                source,
                offset,
                len,
                as_pointer,
            }
        }
    }

    struct Plan {
        stages: Vec<Vec<ReadOp>>,
    }

    impl Plan {
        fn reads(&self) -> usize {
            self.stages.iter().map(Vec::len).sum()
        }

        fn bytes(&self) -> usize {
            self.stages.iter().flatten().map(|op| op.len as usize).sum()
        }

        /// Flattens stage/op order into the sequence a naive per-read caller would
        /// issue, each tagged with its own `(stage, index)` so later ops can name it
        /// as a dependency.
        fn flattened(&self) -> Vec<((usize, usize), ReadOp)> {
            self.stages
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
    }

    /// Models `key_overlay`/`global_precise`'s pointer chain; see module doc.
    fn precise_plan() -> Plan {
        let overlay = ReadOp::new(Source::Root, 0, 8, true);
        let controller = ReadOp::new(Source::Prev(0, 0), 0, 8, true);
        let trigger_list = ReadOp::new(Source::Prev(1, 0), 0, 8, true);
        let list_count = ReadOp::new(Source::Prev(2, 0), 0x10, 4, false);
        let trigger_array = ReadOp::new(Source::Prev(2, 0), 8, 8, true);
        let per_key_pointer = (0..18)
            .map(|index| ReadOp::new(Source::Prev(3, 1), 0x10 + i64::from(index) * 8, 8, true))
            .collect::<Vec<_>>();
        let per_key_fields = (0..18)
            .map(|index| ReadOp::new(Source::Prev(4, index), 0x14, 9, false))
            .collect::<Vec<_>>();
        let clock_source = ReadOp::new(Source::Root, 8, 8, true);
        let clock_time = ReadOp::new(Source::Prev(6, 0), 0, 8, false);

        Plan {
            stages: vec![
                vec![overlay],
                vec![controller],
                vec![trigger_list],
                vec![list_count, trigger_array],
                per_key_pointer,
                per_key_fields,
                vec![clock_source],
                vec![clock_time],
            ],
        }
    }

    /// Models `capture_regular`'s fan-out across independent decoder calls; see
    /// module doc.
    fn regular_plan() -> Plan {
        let beatmap = ReadOp::new(Source::Root, 0x40, 8, true);
        let settings = ReadOp::new(Source::Root, 0x48, 8, true);
        let menu = ReadOp::new(Source::Root, 0x50, 8, true);
        let mp3 = ReadOp::new(Source::Root, 0x58, 8, true);
        let beatmap_fields = ReadOp::new(Source::Prev(0, 0), 0x20, 8, false);
        let settings_fields = ReadOp::new(Source::Prev(0, 1), 0x40, 64, false);
        let menu_field = ReadOp::new(Source::Prev(0, 2), 0x10, 8, false);
        let mp3_field = ReadOp::new(Source::Prev(0, 3), 8, 4, false);

        Plan {
            stages: vec![
                vec![beatmap, settings, menu, mp3],
                vec![beatmap_fields],
                vec![settings_fields],
                vec![menu_field],
                vec![mp3_field],
            ],
        }
    }

    /// Executes every stage of `plan` against `process`, resolving `Source::Prev`
    /// references from the previous stages' bytes. Each stage becomes exactly one
    /// `read_many` call (one native batch / one `process_vm_readv`).
    fn execute_plan(
        process: &dyn ProcessMemory,
        root: u64,
        plan: &Plan,
    ) -> Result<Vec<Vec<u8>>, MemoryError> {
        let mut resolved: Vec<Vec<u64>> = Vec::with_capacity(plan.stages.len());
        let mut all_bytes: Vec<Vec<u8>> = Vec::with_capacity(plan.reads());

        for stage in &plan.stages {
            let mut buffers: Vec<Vec<u8>> =
                stage.iter().map(|op| vec![0u8; op.len as usize]).collect();
            let addresses: Vec<u64> = stage
                .iter()
                .map(|op| {
                    let base = match op.source {
                        Source::Root => root,
                        Source::Prev(stage_index, op_index) => resolved[stage_index][op_index],
                    };
                    base.wrapping_add_signed(op.offset)
                })
                .collect();
            let mut requests: Vec<ReadRequest<'_>> = addresses
                .iter()
                .zip(buffers.iter_mut())
                .map(|(address, buffer)| ReadRequest::new(*address as usize, buffer.as_mut_slice()))
                .collect();
            process.read_many(&mut requests)?;
            drop(requests);

            let mut pointers = Vec::with_capacity(stage.len());
            for (op, buffer) in stage.iter().zip(&buffers) {
                pointers.push(if op.as_pointer {
                    u64::from_le_bytes(buffer[..8].try_into().expect("pointer read is 8 bytes"))
                } else {
                    0
                });
            }
            resolved.push(pointers);
            all_bytes.extend(buffers);
        }
        Ok(all_bytes)
    }

    fn validate(process: &dyn ProcessMemory, root: u64, plan: &Plan, is_precise: bool) {
        let results = execute_plan(process, root, plan).expect("validate plan executes cleanly");
        if is_precise {
            let count = i32::from_le_bytes(results[3][..4].try_into().unwrap());
            assert_eq!(count, 18, "precise plan trigger list count");
            let time = f64::from_le_bytes(results[42][..8].try_into().unwrap());
            assert_eq!(time, 321.5, "precise plan clock time");
            for index in 0..18usize {
                let field = &results[23 + index];
                let value = i32::from_le_bytes(field[..4].try_into().unwrap());
                assert_eq!(value, (index * 10) as i32, "precise plan per-key count");
            }
        } else {
            let beatmap = i32::from_le_bytes(results[4][..4].try_into().unwrap());
            assert_eq!(beatmap, 42, "regular plan beatmap field");
            let mp3 = i32::from_le_bytes(results[7][..4].try_into().unwrap());
            assert_eq!(mp3, 180_000, "regular plan mp3 length");
        }
    }

    // --- Wire encoding (message-based: no pointer crosses the IPC boundary) ---

    fn encode_plan(plan: &Plan) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend((plan.stages.len() as u32).to_le_bytes());
        for stage in &plan.stages {
            bytes.extend((stage.len() as u32).to_le_bytes());
            for op in stage {
                match op.source {
                    Source::Root => {
                        bytes.push(0);
                        bytes.extend(0u32.to_le_bytes());
                        bytes.extend(0u32.to_le_bytes());
                    }
                    Source::Prev(stage_index, op_index) => {
                        bytes.push(1);
                        bytes.extend((stage_index as u32).to_le_bytes());
                        bytes.extend((op_index as u32).to_le_bytes());
                    }
                }
                bytes.extend(op.offset.to_le_bytes());
                bytes.extend(op.len.to_le_bytes());
                bytes.push(u8::from(op.as_pointer));
            }
        }
        bytes
    }

    fn decode_plan(bytes: &[u8]) -> Plan {
        let mut cursor = 0usize;
        let mut take = |len: usize| {
            let slice = &bytes[cursor..cursor + len];
            cursor += len;
            slice
        };
        let stage_count = u32::from_le_bytes(take(4).try_into().unwrap()) as usize;
        let mut stages = Vec::with_capacity(stage_count);
        for _ in 0..stage_count {
            let op_count = u32::from_le_bytes(take(4).try_into().unwrap()) as usize;
            let mut ops = Vec::with_capacity(op_count);
            for _ in 0..op_count {
                let tag = take(1)[0];
                let prev_stage = u32::from_le_bytes(take(4).try_into().unwrap()) as usize;
                let prev_index = u32::from_le_bytes(take(4).try_into().unwrap()) as usize;
                let offset = i64::from_le_bytes(take(8).try_into().unwrap());
                let len = u32::from_le_bytes(take(4).try_into().unwrap());
                let as_pointer = take(1)[0] != 0;
                let source = if tag == 0 {
                    Source::Root
                } else {
                    Source::Prev(prev_stage, prev_index)
                };
                ops.push(ReadOp::new(source, offset, len, as_pointer));
            }
            stages.push(ops);
        }
        Plan { stages }
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

    // --- Percentiles ---

    struct Percentiles {
        p50_ns: f64,
        p99_ns: f64,
        mean_ns: f64,
    }

    fn percentiles(mut samples: Vec<u128>) -> Percentiles {
        samples.sort_unstable();
        let len = samples.len();
        let p50 = samples[len / 2] as f64;
        let p99 = samples[(len * 99 / 100).min(len - 1)] as f64;
        let mean = samples.iter().sum::<u128>() as f64 / len as f64;
        Percentiles {
            p50_ns: p50,
            p99_ns: p99,
            mean_ns: mean,
        }
    }

    struct Report {
        variant: &'static str,
        plan: &'static str,
        reads: usize,
        bytes: usize,
        round_trips_per_sample: usize,
        timing: Percentiles,
        core_cpu_ns_per_sample: f64,
        plugin_cpu_ns_per_sample: f64,
    }

    fn print_report(report: &Report) {
        println!(
            "METRIC {}_{}_p50_ns={:.1}",
            report.plan, report.variant, report.timing.p50_ns
        );
        println!(
            "METRIC {}_{}_p99_ns={:.1}",
            report.plan, report.variant, report.timing.p99_ns
        );
        println!(
            "METRIC {}_{}_mean_ns={:.1}",
            report.plan, report.variant, report.timing.mean_ns
        );
        println!(
            "METRIC {}_{}_core_cpu_ns={:.1}",
            report.plan, report.variant, report.core_cpu_ns_per_sample
        );
        println!(
            "METRIC {}_{}_plugin_cpu_ns={:.1}",
            report.plan, report.variant, report.plugin_cpu_ns_per_sample
        );
        println!(
            "METRIC {}_{}_round_trips={}",
            report.plan, report.variant, report.round_trips_per_sample
        );
        println!(
            "METRIC {}_{}_reads={}",
            report.plan, report.variant, report.reads
        );
        println!(
            "METRIC {}_{}_bytes={}",
            report.plan, report.variant, report.bytes
        );
    }

    // --- Synthetic target process ---

    struct Target {
        child: Child,
        root: u64,
        process: Arc<dyn ProcessMemory>,
    }

    /// `CARGO_BIN_EXE_<name>` only covers `[[bin]]` targets, not `[[example]]`;
    /// this bench's own executable sits at `target/<profile>/deps/<hash>`, one
    /// level below the examples directory, so the helper built by
    /// `cargo build --release --locked --example brokered_reads_target` is found
    /// relative to that.
    fn target_binary_path() -> std::path::PathBuf {
        let mut dir = std::env::current_exe()
            .expect("locate this bench's own executable")
            .parent()
            .expect("bench executable has a parent directory")
            .to_path_buf();
        dir.pop();
        dir.push("examples");
        dir.push("brokered_reads_target");
        assert!(
            dir.is_file(),
            "missing {dir:?}; run `cargo build --release --locked --example brokered_reads_target` first"
        );
        dir
    }

    impl Target {
        fn spawn() -> Self {
            let mut child = Command::new(target_binary_path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn synthetic target process");
            let pid = child.id();
            let mut stdout = child.stdout.take().expect("target stdout is piped");
            let mut line = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                stdout.read_exact(&mut byte).expect("read target base line");
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            let text = String::from_utf8(line).expect("target base line is UTF-8");
            let hex = text
                .strip_prefix("BASE ")
                .expect("target reports BASE <hex>");
            let root =
                u64::from_str_radix(hex.trim_start_matches("0x"), 16).expect("parse base address");

            let memory = open_process(pid).expect("open synthetic target via process_vm_readv");
            Self {
                child,
                root,
                process: Arc::new(memory),
            }
        }
    }

    impl Drop for Target {
        fn drop(&mut self) {
            drop(self.child.stdin.take());
            let _ = self.child.wait();
        }
    }

    // --- Variant: direct in-process reads ---

    fn run_direct(target: &Target, plan: &Plan) -> Percentiles {
        for _ in 0..WARMUP {
            execute_plan(target.process.as_ref(), target.root, plan).expect("warm up direct reads");
        }
        let mut samples = Vec::with_capacity(SAMPLES);
        for _ in 0..SAMPLES {
            let start = Instant::now();
            execute_plan(target.process.as_ref(), target.root, plan).expect("direct read sample");
            samples.push(start.elapsed().as_nanos());
        }
        percentiles(samples)
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

    fn run_uds(target: &Target, plan_bytes: &[u8]) -> (Percentiles, f64, f64) {
        let (mut plugin_end, mut core_end) = UnixStream::pair().expect("create socketpair");
        let self_cpu_before = cpu_now();
        let children_cpu_before = cpu_children();

        // SAFETY: see `fork`'s contract; this process has started no threads.
        let pid = unsafe { fork() };
        assert!(pid >= 0, "fork brokered-uds plugin host");
        if pid == 0 {
            drop(core_end);
            for _ in 0..WARMUP {
                write_frame(&mut plugin_end, plan_bytes);
                read_frame(&mut plugin_end);
            }
            let mut samples = Vec::with_capacity(SAMPLES);
            for _ in 0..SAMPLES {
                let start = Instant::now();
                write_frame(&mut plugin_end, plan_bytes);
                read_frame(&mut plugin_end);
                samples.push(start.elapsed().as_nanos());
            }
            let encoded = encode_percentiles(&percentiles(samples));
            write_frame(&mut plugin_end, &encoded);
            std::process::exit(0);
        }

        drop(plugin_end);
        loop {
            let request = read_frame(&mut core_end);
            if request.len() == 24 {
                // A 24-byte frame is the child's encoded percentiles, not a plan.
                let summary = decode_percentiles(&request);
                waitpid(pid);
                let core_cpu = cpu_total_ns(&cpu_now()) - cpu_total_ns(&self_cpu_before);
                let plugin_cpu = cpu_total_ns(&cpu_children()) - cpu_total_ns(&children_cpu_before);
                return (
                    summary,
                    core_cpu as f64 / SAMPLES as f64,
                    plugin_cpu as f64 / SAMPLES as f64,
                );
            }
            let requested = decode_plan(&request);
            let results = execute_plan(target.process.as_ref(), target.root, &requested)
                .expect("core executes brokered plan");
            let response: Vec<u8> = results.into_iter().flatten().collect();
            write_frame(&mut core_end, &response);
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
        // SAFETY: `eventfd(0, 0)` with no special flags is always valid; the
        // returned fd is owned by the caller and inherited across `fork`.
        let fd = unsafe { libc::eventfd(0, 0) };
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

    fn run_shm(target: &Target, plan_bytes: &[u8]) -> (Percentiles, f64, f64) {
        let region = SharedRegion::new();
        let request_ready = eventfd_new();
        let response_ready = eventfd_new();
        let (mut plugin_summary, mut core_summary) =
            UnixStream::pair().expect("create summary pipe");
        let self_cpu_before = cpu_now();
        let children_cpu_before = cpu_children();

        // SAFETY: see `fork`'s contract; this process has started no threads.
        let pid = unsafe { fork() };
        assert!(pid >= 0, "fork brokered-shm plugin host");
        if pid == 0 {
            drop(core_summary);
            for _ in 0..WARMUP {
                region.write_slot(0, plan_bytes);
                eventfd_signal(request_ready);
                eventfd_wait(response_ready);
                region.read_slot(1);
            }
            let mut samples = Vec::with_capacity(SAMPLES);
            for _ in 0..SAMPLES {
                let start = Instant::now();
                region.write_slot(0, plan_bytes);
                eventfd_signal(request_ready);
                eventfd_wait(response_ready);
                region.read_slot(1);
                samples.push(start.elapsed().as_nanos());
            }
            let encoded = encode_percentiles(&percentiles(samples));
            write_frame(&mut plugin_summary, &encoded);
            std::process::exit(0);
        }

        drop(plugin_summary);
        for _ in 0..WARMUP + SAMPLES {
            eventfd_wait(request_ready);
            let requested = decode_plan(&region.read_slot(0));
            let results = execute_plan(target.process.as_ref(), target.root, &requested)
                .expect("core executes brokered shm plan");
            let response: Vec<u8> = results.into_iter().flatten().collect();
            region.write_slot(1, &response);
            eventfd_signal(response_ready);
        }
        let summary = decode_percentiles(&read_frame(&mut core_summary));
        waitpid(pid);
        let core_cpu = cpu_total_ns(&cpu_now()) - cpu_total_ns(&self_cpu_before);
        let plugin_cpu = cpu_total_ns(&cpu_children()) - cpu_total_ns(&children_cpu_before);
        (
            summary,
            core_cpu as f64 / SAMPLES as f64,
            plugin_cpu as f64 / SAMPLES as f64,
        )
    }

    // --- Variant: naive per-read RPC (no plan batching) ---

    fn run_naive(target: &Target, plan: &Plan) -> (Percentiles, f64, f64) {
        let flattened = plan.flattened();
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
                        resolved
                            .insert(*key, u64::from_le_bytes(response[..8].try_into().unwrap()));
                    }
                }
            };
            for _ in 0..WARMUP {
                run_once(&mut plugin_end);
            }
            let mut samples = Vec::with_capacity(SAMPLES);
            for _ in 0..SAMPLES {
                let start = Instant::now();
                run_once(&mut plugin_end);
                samples.push(start.elapsed().as_nanos());
            }
            let encoded = encode_percentiles(&percentiles(samples));
            write_frame(&mut plugin_end, &encoded);
            std::process::exit(0);
        }

        drop(plugin_end);
        loop {
            let request = read_frame(&mut core_end);
            if request.len() == 24 {
                let summary = decode_percentiles(&request);
                waitpid(pid);
                let core_cpu = cpu_total_ns(&cpu_now()) - cpu_total_ns(&self_cpu_before);
                let plugin_cpu = cpu_total_ns(&cpu_children()) - cpu_total_ns(&children_cpu_before);
                return (
                    summary,
                    core_cpu as f64 / SAMPLES as f64,
                    plugin_cpu as f64 / SAMPLES as f64,
                );
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
        }
    }

    fn run_plan(target: &Target, plan_name: &'static str, plan: &Plan, is_precise: bool) {
        validate(target.process.as_ref(), target.root, plan, is_precise);
        let plan_bytes = encode_plan(plan);

        let direct = run_direct(target, plan);
        print_report(&Report {
            variant: "direct",
            plan: plan_name,
            reads: plan.reads(),
            bytes: plan.bytes(),
            round_trips_per_sample: plan.stages.len(),
            timing: direct,
            core_cpu_ns_per_sample: 0.0,
            plugin_cpu_ns_per_sample: 0.0,
        });

        let (uds, uds_core_cpu, uds_plugin_cpu) = run_uds(target, &plan_bytes);
        print_report(&Report {
            variant: "uds",
            plan: plan_name,
            reads: plan.reads(),
            bytes: plan.bytes(),
            round_trips_per_sample: 1,
            timing: uds,
            core_cpu_ns_per_sample: uds_core_cpu,
            plugin_cpu_ns_per_sample: uds_plugin_cpu,
        });

        let (shm, shm_core_cpu, shm_plugin_cpu) = run_shm(target, &plan_bytes);
        print_report(&Report {
            variant: "shm",
            plan: plan_name,
            reads: plan.reads(),
            bytes: plan.bytes(),
            round_trips_per_sample: 1,
            timing: shm,
            core_cpu_ns_per_sample: shm_core_cpu,
            plugin_cpu_ns_per_sample: shm_plugin_cpu,
        });

        let (naive, naive_core_cpu, naive_plugin_cpu) = run_naive(target, plan);
        print_report(&Report {
            variant: "naive",
            plan: plan_name,
            reads: plan.reads(),
            bytes: plan.bytes(),
            round_trips_per_sample: plan.reads(),
            timing: naive,
            core_cpu_ns_per_sample: naive_core_cpu,
            plugin_cpu_ns_per_sample: naive_plugin_cpu,
        });
    }

    pub(crate) fn run() {
        let target = Target::spawn();
        run_plan(&target, "precise", &precise_plan(), true);
        run_plan(&target, "regular", &regular_plan(), false);
    }
}

#[cfg(target_os = "linux")]
fn main() {
    linux_impl::run();
}

/// The brokered-reads IPC transport (`fork`, `eventfd`, Unix domain sockets,
/// `process_vm_readv`) is Linux-only; this bench is a no-op stub elsewhere.
#[cfg(not(target_os = "linux"))]
fn main() {}
