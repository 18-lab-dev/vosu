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
//! Every variant submits the same encoded plans and must return the same
//! bytes: `result_sha256` must equal both `direct`'s and the hash the target
//! computes over its own memory (`EXPECT` lines), or the run fails.
//!
//! - `direct` (both OSes): this process reads the target natively itself
//!   (`process_vm_readv` / `ReadProcessMemory`), as an in-process plugin does.
//! - Linux prototypes, unchanged in protocol and method
//!   (`support/brokered/prototype_linux.rs`): `uds` (socketpair), `shm`
//!   (anonymous shared mapping plus two eventfds) and `naive` (one round trip
//!   per read) fork a plugin-host child that times its own round trips.
//! - P1 candidates (`support/brokered/candidate.rs`): a dedicated plugin-side
//!   helper (this executable re-run with `--p1-child`) inherits only its
//!   endpoint and submits one `READ_PLAN_SUBMIT` frame per sample; core serves
//!   it and answers with one `READ_PLAN_RESULT` frame. Frames, frame-policy
//!   admission, aligned receive buffers, one write per frame, no flush and
//!   blocking waits are identical for every candidate; only the byte carrier
//!   differs:
//!   - `ring` (Linux) and `section` (Windows): T0's production ring
//!     (`vosu_core::plugin::process::channel` over
//!     `vosu_sys::plugin_process::RingObjects`: sealed memfd + eventfd, or an
//!     unnamed section + auto-reset event per direction). `ring` is the Linux
//!     reference for `section`.
//!   - `pipe` (Windows): the measurement-only named pipe
//!     (`support/brokered/pipe_windows.rs`).
//!
//! The production broker variants (`production_sync`, `production_async`)
//! belong to M5's production arm (B0/E0) and are not implemented here.
//!
//! ## Metrics
//!
//! `METRIC {shape}_{variant}_{metric}=value` lines. Every variant prints
//! `p50_ns`, `p99_ns`, `mean_ns` (fixed samples timed by the submitting side),
//! `core_cpu_ns`, `plugin_cpu_ns`, `round_trips` (logical submissions per
//! sample), `reads`, `bytes`, `wire_frames`, `control_frames` and
//! `result_sha256`. `direct` and the candidates also print Criterion's median
//! with its 95% interval (`criterion_median_ns`,
//! `criterion_median_ci95_{low,high}_ns`), `wakeups` (context switches;
//! see `support/brokered/os.rs`), `allocations`, `copies` and `clock_reads`
//! (transport-internal), split by side where both sides work. Candidates are
//! measured per side over measured loops only (timed samples and Criterion
//! batches, never warm-up or Criterion's analysis); core over the whole
//! session between `READY` and `DESTROYED`. `load_other_busy_cpus` is the
//! busy CPU time of the run's CPUs (Linux: those in this process's affinity;
//! Windows: the whole machine) not explained by core and helper, divided by
//! wall time. Cold startup is `cold_{variant}_startup_{p50,p99}_ns`: helper
//! spawn to core receiving its `READY`. `run_affinity` is checked equal in
//! the target and every helper, and on Linux each helper's descriptors must
//! be exactly stdio plus its endpoint.
//!
//! ## Arguments
//!
//! The runner is `harness = false` and takes no Criterion flags. Supported:
//!
//! - `--bench` (passed by `cargo bench`): full measurement. Without it the
//!   runner is a quick correctness pass (20 warm-up, 200 samples, 3 cold
//!   starts, no Criterion).
//! - `--warmup N`, `--samples N`, `--startup-samples N`: override the counts
//!   (defaults 200, 5,000 and 50).
//! - `--criterion-ms N`: Criterion measurement time per case (default 3,000;
//!   `0` disables Criterion).
//! - `--round N`: a paired-round label; even rounds run the candidates in
//!   reverse order.
//!
//! Anything else is an error. `--p1-child ...` is internal.
//!
//! ## Reproduce
//!
//! Build the target first (`cargo bench` does not build examples):
//!
//! ```sh
//! cargo build --locked --release --example brokered_reads_target
//! taskset -c 0-7,16-23 cargo bench --locked --bench brokered_reads_bench
//! ```
//!
//! On Windows run the built executable directly under an explicit affinity
//! launcher (children inherit it), e.g.
//! `cmd /c start /b /wait /affinity FFFF target\release\deps\brokered_reads_bench-HASH.exe --bench`.

#[cfg(any(target_os = "linux", windows))]
#[path = "support/brokered/mod.rs"]
mod brokered;

#[cfg(any(target_os = "linux", windows))]
#[global_allocator]
static ALLOCATOR: brokered::os::CountingAllocator = brokered::os::CountingAllocator;

#[cfg(any(target_os = "linux", windows))]
mod runner {
    use std::path::Path;
    use std::time::{Duration, Instant};

    use super::brokered::candidate::{self, Candidate, Settings};
    use super::brokered::os::{self, Usage};
    use super::brokered::plans::{Plan, shapes};
    use super::brokered::stats::{self, metric, percentiles};
    use super::brokered::target::Target;
    use super::brokered::wire::{execute_plan, sha256_hex};

    /// The frozen D17 shapes: `(stages, reads, bytes)`.
    const FROZEN: [(&str, usize, usize, usize); 2] =
        [("precise", 8, 43, 358), ("regular", 5, 8, 116)];

    struct Options {
        bench: bool,
        warmup: Option<usize>,
        samples: Option<usize>,
        startup_samples: Option<usize>,
        criterion_ms: Option<u64>,
        round: u32,
    }

    fn parse(args: &[String]) -> Options {
        let mut options = Options {
            bench: false,
            warmup: None,
            samples: None,
            startup_samples: None,
            criterion_ms: None,
            round: 1,
        };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let mut number = || -> u64 {
                args.next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or_else(|| panic!("{arg} needs a non-negative integer"))
            };
            match arg.as_str() {
                "--bench" => options.bench = true,
                "--warmup" => options.warmup = Some(number() as usize),
                "--samples" => options.samples = Some(number() as usize),
                "--startup-samples" => options.startup_samples = Some(number() as usize),
                "--criterion-ms" => options.criterion_ms = Some(number()),
                "--round" => options.round = number() as u32,
                other => panic!(
                    "unsupported argument {other:?}; see benches/brokered_reads_bench.rs \
                     (`--bench`, `--warmup`, `--samples`, `--startup-samples`, \
                     `--criterion-ms`, `--round`)"
                ),
            }
        }
        options
    }

    /// The direct variant: this process reads the target itself.
    fn run_direct(target: &Target, shape: &str, plan: &Plan, settings: &Settings) -> String {
        let prefix = format!("{shape}_direct");
        let read = || {
            execute_plan(target.process.as_ref(), target.root, plan).expect("direct read sample")
        };
        let response: Vec<u8> = read().into_iter().flatten().collect();
        let sha = sha256_hex(&response);
        assert_eq!(
            sha,
            target.expected_sha256(shape),
            "{prefix}: native reads differ from the target's own bytes; run invalid"
        );
        for _ in 0..settings.warmup {
            read();
        }
        let mut samples = Vec::with_capacity(settings.samples);
        let usage = Usage::begin();
        for _ in 0..settings.samples {
            let start = Instant::now();
            let result = read();
            samples.push(start.elapsed().as_nanos());
            drop(result);
        }
        let usage = Usage::end().since(usage);
        let per = |value: u64| value as f64 / settings.samples as f64;
        stats::print_percentiles(&prefix, &percentiles(samples));
        metric(
            &prefix,
            "core_cpu_ns",
            format_args!("{:.1}", per(usage.cpu_ns)),
        );
        if cfg!(windows) {
            metric(
                &prefix,
                "core_cpu_cycles",
                format_args!("{:.0}", per(usage.cpu_cycles)),
            );
        }
        metric(&prefix, "plugin_cpu_ns", 0);
        metric(&prefix, "round_trips", 0);
        metric(&prefix, "native_batches", plan.stages.len());
        metric(&prefix, "reads", plan.reads());
        metric(&prefix, "bytes", plan.bytes());
        metric(&prefix, "wire_frames", 0);
        metric(&prefix, "control_frames", 0);
        metric(
            &prefix,
            "wakeups",
            format_args!("{:.2}", per(usage.context_switches)),
        );
        metric(
            &prefix,
            "allocations",
            format_args!("{:.2}", per(usage.allocations)),
        );
        metric(&prefix, "copies", 0);
        metric(&prefix, "clock_reads", 0);
        metric(&prefix, "result_sha256", &sha);
        sha
    }

    /// Criterion for `direct`, after every forking variant has finished
    /// (Criterion's analysis may start threads).
    fn criterion_direct(target: &Target, output: &Path, measurement: Duration) {
        let mut criterion = stats::criterion(output, measurement);
        for (shape, plan) in shapes() {
            let id = format!("{shape}_direct");
            criterion.bench_function(&id, |bencher| {
                bencher.iter(|| {
                    execute_plan(target.process.as_ref(), target.root, &plan)
                        .expect("direct read sample")
                });
            });
            stats::print_criterion(&id, &stats::criterion_median(output, &id));
        }
    }

    #[cfg(target_os = "linux")]
    fn run_prototypes(
        target: &Target,
        shape: &str,
        plan: &Plan,
        settings: &Settings,
        direct: &str,
    ) {
        use super::brokered::prototype_linux::{run_naive, run_shm, run_uds};
        use super::brokered::wire::encode_plan;

        let plan_bytes = encode_plan(plan);
        let (warmup, samples) = (settings.warmup, settings.samples);
        let outcomes = [
            ("uds", 1, run_uds(target, &plan_bytes, warmup, samples)),
            ("shm", 1, run_shm(target, &plan_bytes, warmup, samples)),
            (
                "naive",
                plan.reads(),
                run_naive(target, plan, warmup, samples),
            ),
        ];
        for (variant, round_trips, outcome) in outcomes {
            let prefix = format!("{shape}_{variant}");
            let sha = sha256_hex(&outcome.first_response);
            assert_eq!(
                sha, direct,
                "{prefix}: response differs from direct; run invalid"
            );
            stats::print_percentiles(&prefix, &outcome.timing);
            metric(
                &prefix,
                "core_cpu_ns",
                format_args!("{:.1}", outcome.core_cpu_ns),
            );
            metric(
                &prefix,
                "plugin_cpu_ns",
                format_args!("{:.1}", outcome.plugin_cpu_ns),
            );
            metric(&prefix, "round_trips", round_trips);
            metric(&prefix, "reads", plan.reads());
            metric(&prefix, "bytes", plan.bytes());
            metric(&prefix, "wire_frames", 2 * round_trips);
            metric(&prefix, "control_frames", 0);
            metric(&prefix, "result_sha256", sha);
        }
    }

    #[cfg(windows)]
    fn run_prototypes(_: &Target, _: &str, _: &Plan, _: &Settings, _: &str) {}

    pub fn run(args: &[String]) {
        let options = parse(args);
        let output = tempfile::tempdir().expect("Criterion and summary directory");
        let criterion_ms = options
            .criterion_ms
            .unwrap_or(if options.bench { 3_000 } else { 0 });
        let settings = Settings {
            warmup: options
                .warmup
                .unwrap_or(if options.bench { 200 } else { 20 }),
            samples: options
                .samples
                .unwrap_or(if options.bench { 5_000 } else { 200 }),
            startup_samples: options
                .startup_samples
                .unwrap_or(if options.bench { 50 } else { 3 }),
            criterion: (criterion_ms > 0).then(|| Duration::from_millis(criterion_ms)),
            output: output.path().to_path_buf(),
        };
        assert!(
            settings.samples > 0 && settings.startup_samples > 0,
            "counts must be nonzero"
        );
        let reverse = options.round.is_multiple_of(2);
        let own_cpus = os::own_affinity();
        println!(
            "METRIC run_mode={}",
            if options.bench { "bench" } else { "test" }
        );
        println!("METRIC run_round={}", options.round);
        println!("METRIC run_os={}", std::env::consts::OS);
        println!("METRIC run_affinity={}", os::cpu_list(&own_cpus));
        println!(
            "METRIC run_candidate_order={}",
            Candidate::all(reverse)
                .iter()
                .map(|candidate| candidate.name())
                .collect::<Vec<_>>()
                .join(",")
        );

        let target = Target::spawn();
        assert_eq!(
            os::child_affinity(&target.child),
            own_cpus,
            "target affinity differs from the runner's"
        );
        for (shape, plan) in shapes() {
            let frozen = FROZEN
                .iter()
                .find(|frozen| frozen.0 == shape)
                .expect("frozen shape");
            assert_eq!(
                (plan.stages.len(), plan.reads(), plan.bytes()),
                (frozen.1, frozen.2, frozen.3),
                "{shape} plan shape is frozen by the D17 contract"
            );
            let direct = run_direct(&target, shape, &plan, &settings);
            run_prototypes(&target, shape, &plan, &settings, &direct);
            for candidate in Candidate::all(reverse) {
                candidate::run_session(candidate, shape, &plan, &target, &settings, &direct);
            }
        }
        for candidate in Candidate::all(reverse) {
            let startup = candidate::run_startup(candidate, settings.startup_samples);
            let prefix = format!("cold_{}", candidate.name());
            metric(
                &prefix,
                "startup_p50_ns",
                format_args!("{:.1}", startup.p50_ns),
            );
            metric(
                &prefix,
                "startup_p99_ns",
                format_args!("{:.1}", startup.p99_ns),
            );
            metric(&prefix, "startup_samples", settings.startup_samples);
        }
        if let Some(measurement) = settings.criterion {
            criterion_direct(&target, output.path(), measurement);
        }
    }
}

#[cfg(any(target_os = "linux", windows))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--p1-child") {
        brokered::candidate::child_main(&args);
    } else {
        runner::run(&args);
    }
}

/// The brokered-reads transports are Linux and Windows only.
#[cfg(not(any(target_os = "linux", windows)))]
fn main() {}
