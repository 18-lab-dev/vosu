//! P1's production arm, core side: `production_sync` and
//! `production_async` (M5 D17 contract). Core is this bench process's real
//! `PluginHost` (`benches/support/production`); the submitting side is the
//! bench plugin in a real `vosu plugin-host` child
//! (`examples/brokered_reads_plugin/p1.rs`), which runs the same frozen
//! plans against the same real target as every other variant, through the
//! real relay, ring, ingress, broker workers and executor. Core opens and
//! reads the target itself, as for any matched session.
//!
//! Windows of measurement, each side measuring itself as the candidates
//! do:
//!
//! - the plugin times every sample and Criterion batch, and reports its
//!   process's CPU, context switches and library allocations plus the
//!   ring census over those loops only;
//! - core's CPU, switches and allocations cover the sample command (the
//!   reference response, warm-up and timed samples), divided by the plans
//!   it submitted; Criterion's own analysis runs in core afterwards and is
//!   excluded.
//!
//! [`run_executor`] adds the diagnostic `executor` variant, which is not a
//! D17 variant: core's real executor on the same ABI plan and target in
//! this process, with no transport, so `production_sync` splits into
//! executor work and everything the process boundary adds.

use std::time::{Duration, Instant};

use vosu_core::plugin::{ModuleResolver, execute_sync_into};
use vosu_plugin_api::memory::ProcessBitness;
use vosu_plugin_api::{Bitness, Message, ReadResult, VOSU_READ_PLAN_SYNC, vosu_opcode};

use super::abi;
use super::candidate::Settings;
use super::os::{self, Usage};
use super::plans::Plan;
use super::stats::{self, Percentiles, metric, percentiles};
use super::target::Target;
use super::wire::sha256_hex;
use crate::production::protocol::Fields;
use crate::production::{Instance, ProductionHost};

/// The name core's matcher sees for the target (`[match] process_names`).
const TARGET_NAME: &str = "brokered_reads_target";

/// A production broker variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Production {
    Sync,
    Async,
}

impl Production {
    pub fn name(self) -> &'static str {
        match self {
            Self::Sync => "production_sync",
            Self::Async => "production_async",
        }
    }

    fn mode(self) -> &'static str {
        match self {
            Self::Sync => "sync",
            Self::Async => "async",
        }
    }

    /// Both variants in run order; `reverse` flips it for paired rounds.
    pub fn all(reverse: bool) -> [Self; 2] {
        if reverse {
            [Self::Async, Self::Sync]
        } else {
            [Self::Sync, Self::Async]
        }
    }
}

fn info(instance: &Instance<'_>) -> Fields {
    instance.command(&Fields::new().with("command", "info"))
}

/// What a production session is checked and compared against, from the
/// same run: `direct`'s result hash and the D17 denominator (`shm` on
/// Linux, `section` on Windows) with its p50.
pub struct Baseline<'a> {
    pub direct_sha: &'a str,
    pub reference: (&'static str, f64),
}

/// The historical figure reported beside the same-run ratio, which alone
/// gates: twice brokered-reads.md's prototype `shm` p50 on Linux (the M5
/// bar), twice the quiet candidate `section` p50 on Windows.
fn historical_ns(shape: &str) -> f64 {
    match (shape, cfg!(windows)) {
        ("precise", false) => 21_540.0,
        ("regular", false) => 13_126.0,
        ("precise", true) => 167_200.0,
        ("regular", true) => 129_700.0,
        _ => unreachable!("frozen shapes only"),
    }
}

/// One measured session of `variant` for `shape`; prints its metrics.
/// Panics (failing the run) on a result hash that differs from the target's
/// own or `direct`'s, a response mismatch, a submission that is not one
/// plan frame and one result frame, an unverified child affinity, or a
/// missing child CPU figure.
pub fn run_session(
    host: &ProductionHost,
    variant: Production,
    shape: &str,
    plan: &Plan,
    target: &Target,
    settings: &Settings,
    baseline: &Baseline<'_>,
) {
    let direct_sha = baseline.direct_sha;
    let prefix = format!("{shape}_{}", variant.name());
    let pid = target.child.id();
    let loaded = host.package(
        &format!("bench.p1.{shape}.{}", variant.mode()),
        &[TARGET_NAME],
    );
    let (instance, _) = host.start(&loaded);
    host.offer(pid, TARGET_NAME, Bitness::X64);
    instance.wait_attached(pid);

    let own_cpus = os::own_affinity();
    let before = info(&instance);
    let child_pid = before.number("pid") as u32;
    assert_eq!(
        before.text("affinity"),
        os::cpu_list(&own_cpus),
        "{prefix}: the plugin host reports another affinity than core's"
    );
    assert_eq!(
        os::pid_affinity(child_pid),
        own_cpus,
        "{prefix}: plugin host {child_pid}'s affinity differs from core's"
    );

    let host_before = os::host_busy_ns(&own_cpus);
    let wall = Instant::now();
    let usage_before = Usage::begin();
    let samples = instance.command(
        &Fields::new()
            .with("command", "p1")
            .with("shape", shape)
            .with("mode", variant.mode())
            .with("pid", pid)
            .with("root", format_args!("{:x}", target.root))
            .with("warmup", settings.warmup)
            .with("samples", settings.samples),
    );
    let core = Usage::end().since(usage_before);
    let wall_ns = wall.elapsed().as_nanos() as u64;
    let host_busy = os::host_busy_ns(&own_cpus).saturating_sub(host_before);
    let after = info(&instance);
    let child_session_ns = after
        .number("cpu_ns")
        .saturating_sub(before.number("cpu_ns"));

    let key = format!("{shape}/{}", variant.mode());
    if let Some(measurement) = settings.criterion {
        let mut criterion = stats::criterion(&settings.output, measurement);
        criterion.bench_function(&prefix, |bencher| {
            bencher.iter_custom(|iterations| {
                let batch = instance.command(
                    &Fields::new()
                        .with("command", "p1-batch")
                        .with("key", &key)
                        .with("iterations", iterations),
                );
                Duration::from_nanos(batch.number("elapsed_ns"))
            });
        });
    }
    let report = instance.command(&Fields::new().with("command", "p1-report").with("key", &key));
    instance.stop();
    host.exited(pid);

    let sha = report.text("sha256");
    assert_eq!(
        sha,
        target.expected_sha256(shape),
        "{prefix}: response differs from the target's expected bytes; run invalid"
    );
    assert_eq!(sha, direct_sha, "{prefix}: response differs from direct");
    assert_eq!(
        report.number("mismatches"),
        0,
        "{prefix}: response mismatches"
    );
    assert_eq!(
        (samples.number("reads"), samples.number("bytes")),
        (plan.reads() as u64, plan.bytes() as u64),
        "{prefix}: the submitted plan is not the frozen shape"
    );
    let round_trips = samples.number("window_round_trips");
    assert_eq!(
        (
            samples.number("window_submit_frames"),
            samples.number("window_result_frames")
        ),
        (round_trips, round_trips),
        "{prefix}: a sample was not one plan frame and one result frame"
    );
    let loops = report.number("loop_round_trips");
    let child_cpu = if cfg!(windows) {
        report.number("loop_cpu_cycles")
    } else {
        report.number("loop_cpu_ns")
    };
    assert!(
        child_cpu > 0 && loops > 0,
        "{prefix}: no plugin-host CPU was measured"
    );

    let per = |value: u64, count: u64| value as f64 / count as f64;
    let submissions = samples.number("submissions");
    stats::print_percentiles(
        &prefix,
        &Percentiles {
            p50_ns: samples.float("p50_ns"),
            p99_ns: samples.float("p99_ns"),
            mean_ns: samples.float("mean_ns"),
        },
    );
    let (reference, reference_p50) = baseline.reference;
    metric(&prefix, "ratio_reference", reference);
    metric(
        &prefix,
        "p50_over_reference",
        format_args!("{:.3}", samples.float("p50_ns") / reference_p50),
    );
    metric(
        &prefix,
        "historical_reference_ns",
        format_args!("{:.0}", historical_ns(shape)),
    );
    if settings.criterion.is_some() {
        stats::print_criterion(&prefix, &stats::criterion_median(&settings.output, &prefix));
    }
    let core_wakeups = per(core.context_switches, submissions);
    let plugin_wakeups = per(report.number("loop_switches"), loops);
    let core_allocations = per(core.allocations, submissions);
    let plugin_allocations = per(report.number("loop_allocations"), loops);
    metric(
        &prefix,
        "core_cpu_ns",
        format_args!("{:.1}", per(core.cpu_ns, submissions)),
    );
    metric(
        &prefix,
        "plugin_cpu_ns",
        format_args!("{:.1}", per(report.number("loop_cpu_ns"), loops)),
    );
    if cfg!(windows) {
        metric(
            &prefix,
            "core_cpu_cycles",
            format_args!("{:.0}", per(core.cpu_cycles, submissions)),
        );
        metric(
            &prefix,
            "plugin_cpu_cycles",
            format_args!("{:.0}", per(report.number("loop_cpu_cycles"), loops)),
        );
    }
    metric(
        &prefix,
        "round_trips",
        per(report.number("loop_submit_frames"), loops),
    );
    metric(&prefix, "reads", plan.reads());
    metric(&prefix, "bytes", plan.bytes());
    metric(&prefix, "abi_stages", samples.number("abi_stages"));
    metric(
        &prefix,
        "wire_frames",
        format_args!("{:.2}", per(report.number("loop_wire_frames"), loops)),
    );
    metric(
        &prefix,
        "plugin_to_host_frames",
        format_args!(
            "{:.2}",
            per(report.number("loop_plugin_to_host_frames"), loops)
        ),
    );
    metric(
        &prefix,
        "host_to_plugin_frames",
        format_args!(
            "{:.2}",
            per(report.number("loop_host_to_plugin_frames"), loops)
        ),
    );
    metric(
        &prefix,
        "control_frames",
        format_args!("{:.2}", per(report.number("loop_control_frames"), loops)),
    );
    metric(
        &prefix,
        "wakeups",
        format_args!("{:.2}", core_wakeups + plugin_wakeups),
    );
    metric(&prefix, "core_wakeups", format_args!("{core_wakeups:.2}"));
    metric(
        &prefix,
        "plugin_wakeups",
        format_args!("{plugin_wakeups:.2}"),
    );
    metric(
        &prefix,
        "allocations",
        format_args!("{:.2}", core_allocations + plugin_allocations),
    );
    metric(
        &prefix,
        "core_allocations",
        format_args!("{core_allocations:.2}"),
    );
    metric(
        &prefix,
        "plugin_allocations",
        format_args!("{plugin_allocations:.2}"),
    );
    // Production code has no copy or clock counters; the bench does not
    // patch them in (docs/v2/brokered-reads.md, "Production arm").
    metric(&prefix, "copies", "unmeasured");
    metric(&prefix, "clock_reads", "unmeasured");
    metric(&prefix, "result_sha256", sha);
    metric(&prefix, "result_mismatches", report.number("mismatches"));
    metric(&prefix, "submissions", report.number("submissions"));
    // Busy CPUs of the run's CPUs not explained by core and plugin host,
    // over the sample command.
    let others = host_busy
        .saturating_sub(core.cpu_ns)
        .saturating_sub(child_session_ns);
    metric(
        &prefix,
        "load_other_busy_cpus",
        format_args!("{:.3}", others as f64 / wall_ns as f64),
    );
    metric(&prefix, "affinity_plugin", after.text("affinity"));
}

/// Cold startup: `create_instance` to `READY` (spawn, `CREATE`, the
/// child's confinement and library load, `create`), `count` times.
pub fn run_startup(host: &ProductionHost, count: usize) -> Percentiles {
    let loaded = host.package("bench.p1.startup", &[TARGET_NAME]);
    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        let (instance, startup) = host.start(&loaded);
        samples.push(startup.as_nanos());
        instance.stop();
    }
    percentiles(samples)
}

/// The target names no module: its plans use absolute roots.
struct NoModules;

impl ModuleResolver for NoModules {
    fn resolve(&self, _module: &str) -> Option<u64> {
        None
    }
}

/// The diagnostic `executor` variant (module doc): what a broker worker
/// runs for one `SYNC` plan (`execute_sync_into`, the validated executor
/// `handle_read_plan_submit_raw` calls), timed in this process against the
/// real target. Its result must hash like every other variant's.
pub fn run_executor(
    shape: &str,
    plan: &Plan,
    target: &Target,
    settings: &Settings,
    direct_sha: &str,
) {
    let prefix = format!("{shape}_executor");
    let payload = Message::ReadPlanSubmit(abi::submission(plan, 1, 1, target.root, true))
        .encode_payload()
        .expect("encode the ABI plan");
    let mut out = Vec::new();
    let execute = |out: &mut Vec<u8>| {
        execute_sync_into(
            &payload,
            VOSU_READ_PLAN_SYNC,
            target.process.as_ref(),
            ProcessBitness::X64,
            &NoModules,
            Duration::from_secs(2),
            out,
        )
        .expect("core's executor runs the ABI plan");
    };
    execute(&mut out);
    let Message::ReadPlanResult(result) =
        Message::decode(vosu_opcode::VOSU_OP_READ_PLAN_RESULT, 0, &out).expect("a flat result")
    else {
        panic!("{prefix}: not a read-plan result");
    };
    let bytes: Vec<u8> = result
        .results
        .iter()
        .flat_map(|read| match read {
            ReadResult::Ok(bytes) => bytes.iter().copied(),
            other => panic!("{prefix}: {other:?}"),
        })
        .collect();
    let sha = sha256_hex(&bytes);
    assert_eq!(sha, direct_sha, "{prefix}: response differs from direct");
    for _ in 0..settings.warmup {
        execute(&mut out);
    }
    let mut samples = Vec::with_capacity(settings.samples);
    let usage = Usage::begin();
    for _ in 0..settings.samples {
        let start = Instant::now();
        execute(&mut out);
        samples.push(start.elapsed().as_nanos());
    }
    let usage = Usage::end().since(usage);
    stats::print_percentiles(&prefix, &percentiles(samples));
    if let Some(measurement) = settings.criterion {
        let mut criterion = stats::criterion(&settings.output, measurement);
        criterion.bench_function(&prefix, |bencher| bencher.iter(|| execute(&mut out)));
        stats::print_criterion(&prefix, &stats::criterion_median(&settings.output, &prefix));
    }
    let per = |value: u64| value as f64 / settings.samples as f64;
    metric(
        &prefix,
        "core_cpu_ns",
        format_args!("{:.1}", per(usage.cpu_ns)),
    );
    metric(
        &prefix,
        "allocations",
        format_args!("{:.2}", per(usage.allocations)),
    );
    metric(&prefix, "abi_stages", abi::stages(plan).len());
    metric(&prefix, "reads", plan.reads());
    metric(&prefix, "bytes", plan.bytes());
    metric(&prefix, "result_sha256", sha);
}
