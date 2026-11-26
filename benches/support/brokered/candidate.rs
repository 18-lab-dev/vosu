//! The P1 candidate-transport harness: one harness, two byte carriers.
//!
//! Core (this process) creates the transport objects, spawns a dedicated
//! plugin-side helper (this same bench executable re-run with
//! `--p1-child`), hands it the endpoint only by inheritance, and serves
//! every submitted plan with [`serve`] against the real target. The helper
//! drives the round trips and times them. Per sample, every candidate does
//! exactly the same thing:
//!
//! - one `READ_PLAN_SUBMIT` frame (`VOSU_READ_PLAN_SYNC`, a fresh
//!   correlation) carrying the encoded plan, written in one operation;
//! - one `READ_PLAN_RESULT` frame carrying the flat response;
//! - receive-side frame-policy admission before any payload copy, into a
//!   reusable 8-byte-aligned buffer; no flush; blocking waits only.
//!
//! Startup and teardown use the A5.2 private `READY` and `DESTROYED` frames,
//! outside every measured window.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use vosu_core::plugin::process::frame::{FrameHeader, control};
use vosu_plugin_api::{VOSU_READ_PLAN_SYNC, vosu_opcode};

use super::os::{self, Usage};
use super::plans::{Plan, shapes};
use super::stats::{self, Percentiles, metric, percentiles};
use super::target::Target;
use super::wire::{encode_plan, serve, sha256_hex};

const SUBMIT: u32 = vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT as u32;
const RESULT: u32 = vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32;

/// One side of a candidate transport, frame by frame.
pub trait FrameIo {
    fn send(&mut self, header: &FrameHeader, payload: &[u8]);
    /// Waits for the next frame; its payload is then [`payload`](Self::payload).
    fn recv(&mut self) -> FrameHeader;
    fn payload(&self) -> &[u8];
    /// This side's running totals.
    fn counters(&self) -> TransportCounters;
}

/// One side's transport work: frames it sent or received, payload copies it
/// made (user or kernel), and clock reads inside the transport.
#[derive(Clone, Copy, Debug, Default)]
pub struct TransportCounters {
    pub frames: u64,
    pub copies: u64,
    pub clock_reads: u64,
}

impl TransportCounters {
    fn since(self, earlier: Self) -> Self {
        Self {
            frames: self.frames - earlier.frames,
            copies: self.copies - earlier.copies,
            clock_reads: self.clock_reads - earlier.clock_reads,
        }
    }

    fn add(&mut self, delta: Self) {
        self.frames += delta.frames;
        self.copies += delta.copies;
        self.clock_reads += delta.clock_reads;
    }
}

/// A candidate byte carrier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Candidate {
    /// T0's ring: memfd + eventfd on Linux, unnamed section + auto-reset
    /// events on Windows.
    Ring,
    /// The measurement-only named pipe.
    #[cfg(windows)]
    Pipe,
}

impl Candidate {
    /// The variant name in `METRIC` lines.
    pub fn name(self) -> &'static str {
        match self {
            #[cfg(target_os = "linux")]
            Self::Ring => "ring",
            #[cfg(windows)]
            Self::Ring => "section",
            #[cfg(windows)]
            Self::Pipe => "pipe",
        }
    }

    /// Every candidate on this OS, in run order; `reverse` flips it so
    /// paired rounds can alternate which candidate goes first.
    pub fn all(reverse: bool) -> Vec<Self> {
        let mut all = vec![
            Self::Ring,
            #[cfg(windows)]
            Self::Pipe,
        ];
        if reverse {
            all.reverse();
        }
        all
    }

    fn from_name(name: &str) -> Self {
        Self::all(false)
            .into_iter()
            .find(|candidate| candidate.name() == name)
            .unwrap_or_else(|| panic!("unknown candidate {name:?}"))
    }

    fn prepare(self) -> Parent {
        match self {
            Self::Ring => Parent::Ring(super::ring::prepare()),
            #[cfg(windows)]
            Self::Pipe => Parent::Pipe(super::pipe_windows::prepare()),
        }
    }

    fn attach_child(self, endpoint: &str) -> Box<dyn FrameIo> {
        match self {
            Self::Ring => Box::new(super::ring::attach_child(endpoint)),
            #[cfg(windows)]
            Self::Pipe => Box::new(super::pipe_windows::attach_child(endpoint)),
        }
    }
}

enum Parent {
    Ring(super::ring::Parent),
    #[cfg(windows)]
    Pipe(super::pipe_windows::Parent),
}

impl Parent {
    fn configure(&self, command: &mut Command) -> String {
        match self {
            Self::Ring(parent) => parent.configure(command),
            #[cfg(windows)]
            Self::Pipe(parent) => parent.configure(command),
        }
    }

    fn into_core(self) -> Box<dyn FrameIo> {
        match self {
            Self::Ring(parent) => Box::new(parent.into_core()),
            #[cfg(windows)]
            Self::Pipe(parent) => Box::new(parent.into_core()),
        }
    }
}

/// Runner settings shared by core and its helpers.
pub struct Settings {
    pub warmup: usize,
    pub samples: usize,
    pub startup_samples: usize,
    /// Criterion measurement time; `None` skips Criterion (test mode).
    pub criterion: Option<Duration>,
    /// Criterion and summary files.
    pub output: PathBuf,
}

/// Spawns a helper for `candidate` with the endpoint inherited, returning
/// it with core's side of the transport.
fn spawn_helper(
    candidate: Candidate,
    extra: &[String],
) -> (std::process::Child, Box<dyn FrameIo>, Instant, String) {
    let parent = candidate.prepare();
    let mut command = Command::new(std::env::current_exe().expect("own executable"));
    let endpoint = parent.configure(&mut command);
    command
        .arg("--p1-child")
        .arg(candidate.name())
        .arg("--endpoint")
        .arg(&endpoint)
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let spawned_at = Instant::now();
    let child = command.spawn().expect("spawn plugin-side helper");
    (child, parent.into_core(), spawned_at, endpoint)
}

fn recv_expect(io: &mut dyn FrameIo, opcode: u32) -> FrameHeader {
    let header = io.recv();
    assert_eq!(header.opcode, opcode, "unexpected frame {header:?}");
    header
}

/// Cold startup: spawn to `READY` received, `count` times.
pub fn run_startup(candidate: Candidate, count: usize) -> Percentiles {
    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        let (mut child, mut io, spawned_at, _) =
            spawn_helper(candidate, &["--ready-only".to_owned()]);
        recv_expect(io.as_mut(), control::READY);
        samples.push(spawned_at.elapsed().as_nanos());
        assert!(
            child.wait().expect("reap helper").success(),
            "helper failed"
        );
    }
    percentiles(samples)
}

/// One measured session of `candidate` for `shape`; prints its metrics and
/// returns its p50 (on Windows, `section`'s is the D17 denominator).
/// Panics (failing the run) on any result-hash disagreement.
pub fn run_session(
    candidate: Candidate,
    shape: &str,
    plan: &Plan,
    target: &Target,
    settings: &Settings,
    direct_sha: &str,
) -> f64 {
    let variant = candidate.name();
    let prefix = format!("{shape}_{variant}");
    let summary_path = settings.output.join(format!("summary-{prefix}.txt"));
    let criterion_ms = settings.criterion.map_or(0, |time| time.as_millis());
    let extra = [
        "--shape".to_owned(),
        shape.to_owned(),
        "--warmup".to_owned(),
        settings.warmup.to_string(),
        "--samples".to_owned(),
        settings.samples.to_string(),
        "--criterion-ms".to_owned(),
        criterion_ms.to_string(),
        "--out".to_owned(),
        settings.output.display().to_string(),
    ];
    let own_cpus = os::own_affinity();
    let (mut child, mut io, _, endpoint) = spawn_helper(candidate, &extra);
    let child_cpus = os::child_affinity(&child);

    recv_expect(io.as_mut(), control::READY);
    let host_before = os::host_busy_ns(&own_cpus);
    let wall = Instant::now();
    let usage_before = Usage::begin();
    let counters_before = io.counters();
    let mut counters_last = counters_before;
    let mut served = 0u64;
    let mut private_frames = 0u64;
    loop {
        let header = io.recv();
        match header.opcode {
            SUBMIT => {
                assert_eq!(header.flags, VOSU_READ_PLAN_SYNC, "SYNC submission");
                let response = serve(target.process.as_ref(), target.root, io.payload());
                io.send(
                    &FrameHeader {
                        opcode: RESULT,
                        flags: 0,
                        correlation: header.correlation,
                    },
                    &response,
                );
                served += 1;
                counters_last = io.counters();
            }
            control::DESTROYED => break,
            _ if header.is_private() => {
                private_frames += 1;
            }
            other => panic!("unexpected frame opcode {other:#x}"),
        }
    }
    let core = Usage::end().since(usage_before);
    let wall_ns = wall.elapsed().as_nanos() as u64;
    let host_busy = os::host_busy_ns(&own_cpus).saturating_sub(host_before);
    let core_transport = counters_last.since(counters_before);
    assert!(
        child.wait().expect("reap helper").success(),
        "helper failed"
    );
    drop(io);

    let summary = Summary::read(&summary_path);
    let first_sha = summary.text("first_sha256");
    assert_eq!(
        first_sha,
        summary.text("last_sha256"),
        "{prefix}: response changed during the run"
    );
    assert_eq!(
        summary.number("mismatches"),
        0,
        "{prefix}: response mismatches"
    );
    assert_eq!(
        first_sha,
        target.expected_sha256(shape),
        "{prefix}: response differs from the target's expected bytes; run invalid"
    );
    assert_eq!(
        first_sha, direct_sha,
        "{prefix}: response differs from direct"
    );
    let submissions = summary.number("submissions");
    assert_eq!(
        served, submissions,
        "{prefix}: core served {served} plans for {submissions} logical submissions"
    );
    assert_eq!(
        summary.text("affinity"),
        os::cpu_list(&own_cpus),
        "{prefix}: helper affinity differs from core"
    );
    assert_eq!(
        child_cpus, own_cpus,
        "{prefix}: helper affinity differs from core"
    );

    let per = |value: u64, count: u64| value as f64 / count as f64;
    let loops = summary.number("loop_round_trips");
    let timing = Percentiles {
        p50_ns: summary.float("p50_ns"),
        p99_ns: summary.float("p99_ns"),
        mean_ns: summary.float("mean_ns"),
    };
    stats::print_percentiles(&prefix, &timing);
    if settings.criterion.is_some() {
        stats::print_criterion(&prefix, &stats::criterion_median(&settings.output, &prefix));
    }
    let core_wakeups = per(core.context_switches, served);
    let plugin_wakeups = per(summary.number("loop_switches"), loops);
    let core_allocations = per(core.allocations, served);
    let plugin_allocations = per(summary.number("loop_allocations"), loops);
    let plugin_copies = per(summary.number("loop_copies"), loops);
    let plugin_clocks = per(summary.number("loop_clock_reads"), loops);
    metric(
        &prefix,
        "core_cpu_ns",
        format_args!("{:.1}", per(core.cpu_ns, served)),
    );
    metric(
        &prefix,
        "plugin_cpu_ns",
        format_args!("{:.1}", per(summary.number("loop_cpu_ns"), loops)),
    );
    if cfg!(windows) {
        metric(
            &prefix,
            "core_cpu_cycles",
            format_args!("{:.0}", per(core.cpu_cycles, served)),
        );
        metric(
            &prefix,
            "plugin_cpu_cycles",
            format_args!("{:.0}", per(summary.number("loop_cpu_cycles"), loops)),
        );
    }
    metric(&prefix, "round_trips", served / submissions);
    metric(&prefix, "reads", plan.reads());
    metric(&prefix, "bytes", plan.bytes());
    metric(
        &prefix,
        "wire_frames",
        format_args!("{:.2}", per(core_transport.frames, served)),
    );
    metric(&prefix, "control_frames", per(private_frames, served));
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
    metric(
        &prefix,
        "copies",
        format_args!("{:.2}", per(core_transport.copies, served) + plugin_copies),
    );
    metric(
        &prefix,
        "clock_reads",
        format_args!(
            "{:.2}",
            per(core_transport.clock_reads, served) + plugin_clocks
        ),
    );
    metric(&prefix, "result_sha256", first_sha);
    metric(&prefix, "result_mismatches", summary.number("mismatches"));
    metric(&prefix, "submissions", submissions);
    // Busy CPUs of the run's CPUs not explained by core and helper.
    let others = host_busy
        .saturating_sub(core.cpu_ns)
        .saturating_sub(summary.number("session_cpu_ns"));
    metric(
        &prefix,
        "load_other_busy_cpus",
        format_args!("{:.3}", others as f64 / wall_ns as f64),
    );
    metric(&prefix, "affinity_plugin", summary.text("affinity"));
    if let Some(fds) = summary.optional("inherited_fds") {
        // Exactly stdio plus the endpoint's descriptors, nothing else.
        let mut expected: Vec<u64> = endpoint
            .split(',')
            .map(|fd| fd.parse().expect("descriptor number"))
            .chain([0, 1, 2])
            .collect();
        expected.sort_unstable();
        let expected: Vec<String> = expected.iter().map(u64::to_string).collect();
        assert_eq!(
            fds,
            expected.join(","),
            "{prefix}: helper inherited descriptors"
        );
        metric(&prefix, "inherited_fds", fds);
    }
    timing.p50_ns
}

/// The key=value summary a helper leaves for core.
struct Summary {
    entries: Vec<(String, String)>,
}

impl Summary {
    fn read(path: &Path) -> Self {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read helper summary {}: {error}", path.display()));
        Self {
            entries: text
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        }
    }

    fn optional(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn text(&self, key: &str) -> &str {
        self.optional(key)
            .unwrap_or_else(|| panic!("helper summary lacks {key}"))
    }

    fn number(&self, key: &str) -> u64 {
        self.text(key).parse().expect("integer summary value")
    }

    fn float(&self, key: &str) -> f64 {
        self.text(key).parse().expect("numeric summary value")
    }
}

/// The plugin side of one session.
struct Client {
    io: Box<dyn FrameIo>,
    plan: Vec<u8>,
    correlation: u64,
    expected: Option<Vec<u8>>,
    mismatches: u64,
    submissions: u64,
    /// Totals over every measured loop (timed samples and Criterion
    /// batches), excluding warm-up and Criterion's analysis.
    measured: Usage,
    measured_transport: TransportCounters,
    measured_round_trips: u64,
}

impl Client {
    fn round_trip(&mut self) {
        self.correlation += 1;
        self.submissions += 1;
        self.io.send(
            &FrameHeader {
                opcode: SUBMIT,
                flags: VOSU_READ_PLAN_SYNC,
                correlation: self.correlation,
            },
            &self.plan,
        );
        let header = self.io.recv();
        assert_eq!(
            (header.opcode, header.correlation),
            (RESULT, self.correlation),
            "result frame answers this submission"
        );
    }

    fn check(&mut self) {
        let payload = self.io.payload();
        match &self.expected {
            Some(expected) if expected.as_slice() != payload => self.mismatches += 1,
            Some(_) => {}
            None => self.expected = Some(payload.to_vec()),
        }
    }

    /// The timed-sample window: each round trip timed alone; every
    /// response compared (outside its sample's timing).
    fn measure_samples(&mut self, count: usize) -> Vec<u128> {
        let mut samples = Vec::with_capacity(count);
        let usage = Usage::begin();
        let transport = self.io.counters();
        for _ in 0..count {
            let start = Instant::now();
            self.round_trip();
            samples.push(start.elapsed().as_nanos());
            self.check();
        }
        self.finish_window(usage, transport, count as u64);
        samples
    }

    /// One Criterion batch: `iterations` back-to-back round trips timed
    /// together; the last response is compared after the clock stops.
    fn measure_batch(&mut self, iterations: u64) -> Duration {
        let usage = Usage::begin();
        let transport = self.io.counters();
        let started = Instant::now();
        for _ in 0..iterations {
            self.round_trip();
        }
        let elapsed = started.elapsed();
        self.check();
        self.finish_window(usage, transport, iterations);
        elapsed
    }

    fn finish_window(&mut self, usage: Usage, transport: TransportCounters, round_trips: u64) {
        self.measured_transport
            .add(self.io.counters().since(transport));
        self.measured.add(Usage::end().since(usage));
        self.measured_round_trips += round_trips;
    }
}

/// Helper-process arguments after `--p1-child <variant>`.
struct ChildArgs {
    candidate: Candidate,
    endpoint: String,
    ready_only: bool,
    shape: String,
    warmup: usize,
    samples: usize,
    criterion_ms: u64,
    output: PathBuf,
}

fn parse_child(args: &[String]) -> ChildArgs {
    let mut parsed = ChildArgs {
        candidate: Candidate::Ring,
        endpoint: String::new(),
        ready_only: false,
        shape: String::new(),
        warmup: 0,
        samples: 0,
        criterion_ms: 0,
        output: PathBuf::new(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().expect("option value").clone();
        match arg.as_str() {
            "--p1-child" => parsed.candidate = Candidate::from_name(&value()),
            "--endpoint" => parsed.endpoint = value(),
            "--ready-only" => parsed.ready_only = true,
            "--shape" => parsed.shape = value(),
            "--warmup" => parsed.warmup = value().parse().expect("warmup count"),
            "--samples" => parsed.samples = value().parse().expect("sample count"),
            "--criterion-ms" => parsed.criterion_ms = value().parse().expect("milliseconds"),
            "--out" => parsed.output = PathBuf::from(value()),
            other => panic!("unknown helper argument {other:?}"),
        }
    }
    parsed
}

/// The open descriptors this process started with (Linux), for the exact
/// inheritance allowlist check.
#[cfg(target_os = "linux")]
fn open_descriptors() -> Option<String> {
    let listed: Vec<i32> = std::fs::read_dir("/proc/self/fd")
        .expect("list /proc/self/fd")
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    // The directory's own descriptor is closed again by now.
    let mut open: Vec<i32> = listed
        .into_iter()
        // SAFETY: F_GETFD only queries the descriptor table.
        .filter(|&fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1)
        .collect();
    open.sort_unstable();
    Some(
        open.iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(","),
    )
}

#[cfg(windows)]
fn open_descriptors() -> Option<String> {
    None
}

/// The helper's whole life; called from the runner's `main` for
/// `--p1-child`.
pub fn child_main(args: &[String]) {
    let inherited = open_descriptors();
    let args = parse_child(args);
    let mut io = args.candidate.attach_child(&args.endpoint);
    let session = Usage::begin();
    io.send(
        &FrameHeader {
            opcode: control::READY,
            flags: 0,
            correlation: 0,
        },
        &[],
    );
    if args.ready_only {
        return;
    }
    let plan = shapes()
        .into_iter()
        .find(|(name, _)| *name == args.shape)
        .map(|(_, plan)| plan)
        .expect("known shape");
    let mut client = Client {
        io,
        plan: encode_plan(&plan),
        correlation: 0,
        expected: None,
        mismatches: 0,
        submissions: 0,
        measured: Usage::default(),
        measured_transport: TransportCounters::default(),
        measured_round_trips: 0,
    };
    for _ in 0..args.warmup {
        client.round_trip();
        client.check();
    }
    let timing = percentiles(client.measure_samples(args.samples));
    if args.criterion_ms > 0 {
        let id = format!("{}_{}", args.shape, args.candidate.name());
        let mut criterion =
            stats::criterion(&args.output, Duration::from_millis(args.criterion_ms));
        criterion.bench_function(&id, |bencher| {
            bencher.iter_custom(|iterations| client.measure_batch(iterations));
        });
    }
    let last = client.io.payload().to_vec();
    client.io.send(
        &FrameHeader {
            opcode: control::DESTROYED,
            flags: 0,
            correlation: 0,
        },
        &[],
    );
    let session = Usage::end().since(session);
    let first = client.expected.as_deref().expect("at least one response");

    let mut summary = format!(
        "p50_ns={}\np99_ns={}\nmean_ns={}\n",
        timing.p50_ns, timing.p99_ns, timing.mean_ns
    );
    let mut line = |key: &str, value: &dyn std::fmt::Display| {
        summary.push_str(&format!("{key}={value}\n"));
    };
    line("submissions", &client.submissions);
    line("loop_round_trips", &client.measured_round_trips);
    line("loop_cpu_ns", &client.measured.cpu_ns);
    line("loop_cpu_cycles", &client.measured.cpu_cycles);
    line("loop_switches", &client.measured.context_switches);
    line("loop_allocations", &client.measured.allocations);
    line("loop_copies", &client.measured_transport.copies);
    line("loop_clock_reads", &client.measured_transport.clock_reads);
    line("session_cpu_ns", &session.cpu_ns);
    line("first_sha256", &sha256_hex(first));
    line("last_sha256", &sha256_hex(&last));
    line("mismatches", &client.mismatches);
    line("affinity", &os::cpu_list(&os::own_affinity()));
    if let Some(fds) = &inherited {
        line("inherited_fds", fds);
    }
    let path = args.output.join(format!(
        "summary-{}_{}.txt",
        args.shape,
        args.candidate.name()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| file.write_all(summary.as_bytes()))
        .unwrap_or_else(|error| panic!("write helper summary {}: {error}", path.display()));
}
