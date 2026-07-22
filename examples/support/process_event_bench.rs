
use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{Value, json};
use tokio::time::{Instant as TokioInstant, sleep, sleep_until, timeout_at};
use vosu::instance::{InstanceRuntime, NativeProcessProvider, Pid, ProcessEventMode};
use vosu::memory::lazer_loader::{FetchError, FetchFuture, LazerOffsetLoader, OffsetFetcher};

const EVENT_TIMEOUT: Duration = Duration::from_secs(2);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(7);
const POLL_TICK: Duration = Duration::from_millis(100);
const DEFAULT_CHURN_RATE_HZ: u64 = 50;
const DEFAULT_CHURN_DURATION: Duration = Duration::from_secs(1);
const MAX_DURATION: Duration = Duration::from_secs(3_600);
const MAX_ITERATIONS: usize = 1_000_000;
const MAX_CHURN_RATE_HZ: u64 = 1_000;

type Runtime = InstanceRuntime<NativeProcessProvider, DisabledFetcher>;

#[derive(Clone, Copy)]
enum Mode {
    Poll,
    CnProc,
    Ebpf,
}

impl Mode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "poll" => Ok(Self::Poll),
            "cn-proc" => Ok(Self::CnProc),
            "ebpf" => Ok(Self::Ebpf),
            _ => Err(format!(
                "invalid mode {value:?}; expected poll, cn-proc, or ebpf"
            )),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Poll => "poll",
            Self::CnProc => "cn-proc",
            Self::Ebpf => "ebpf",
        }
    }

    const fn event_mode(self) -> Option<ProcessEventMode> {
        match self {
            Self::Poll => None,
            Self::CnProc => Some(ProcessEventMode::CnProc),
            Self::Ebpf => Some(ProcessEventMode::Ebpf),
        }
    }
}

struct Args {
    mode: Mode,
    idle: Duration,
    iterations: usize,
    churn_rate_hz: u64,
    churn_duration: Duration,
}

#[derive(Clone, Copy)]
struct ResourceUsage {
    user_cpu: Duration,
    system_cpu: Duration,
    voluntary_context_switches: u64,
    involuntary_context_switches: u64,
}

struct PhaseMetrics {
    elapsed: Duration,
    usage: ResourceUsage,
    scans: u64,
    targeted_inspections: u64,
    events: u64,
    event_wakes: u64,
    reconciliation_requests: u64,
}

struct LifecycleMetrics {
    add_latencies: Vec<Duration>,
    add_timeouts: usize,
    rename_latencies: Vec<Duration>,
    rename_timeouts: usize,
    remove_latencies: Vec<Duration>,
    remove_timeouts: usize,
}

struct Results {
    initial_scan_remove_latency: Duration,
    idle: PhaseMetrics,
    lifecycle: PhaseMetrics,
    churn_spawned: usize,
    lifecycle_metrics: LifecycleMetrics,
    total_events: u64,
    total_scans: u64,
    total_targeted_inspections: u64,
    total_event_wakes: u64,
    total_reconciliation_requests: u64,
}

#[derive(Clone, Copy)]
struct DisabledFetcher;

impl OffsetFetcher for DisabledFetcher {
    fn fetch<'a>(
        &'a self,
        _url: &'a str,
        _user_agent: &'a str,
        _timeout: Duration,
    ) -> FetchFuture<'a> {
        Box::pin(async {
            Err(FetchError {
                message: "network disabled by process-event benchmark".to_owned(),
            })
        })
    }
}

struct ExecutableAlias {
    directory: PathBuf,
    path: PathBuf,
}

impl ExecutableAlias {
    fn create() -> io::Result<Self> {
        let executable = env::current_exe()?;
        for suffix in 0..100 {
            let directory = env::temp_dir().join(format!(
                "vosu-process-event-bench-{}-{suffix}",
                std::process::id()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    let path = directory.join("osu!.exe");
                    // A regular copy intentionally drops file capabilities from the privileged
                    // benchmark loader so target helpers remain inspectable as ordinary processes.
                    if let Err(error) = fs::copy(&executable, &path) {
                        let _ = fs::remove_dir(&directory);
                        return Err(error);
                    }
                    return Ok(Self { directory, path });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a unique executable alias directory",
        ))
    }
}

impl Drop for ExecutableAlias {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.directory);
    }
}

struct GatedChild {
    child: Option<Child>,
    pid: Pid,
}

impl GatedChild {
    fn child_mut(&mut self) -> io::Result<&mut Child> {
        self.child
            .as_mut()
            .ok_or_else(|| io::Error::other("helper process was already reaped"))
    }
}

impl Drop for GatedChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn_blocking(move || {
                let _ = child.kill();
                let _ = child.wait();
            });
        } else {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    match env::args().nth(1).as_deref() {
        Some("--target-helper") => {
            if let Err(error) = run_initial_target_helper() {
                eprintln!("process_event_bench target helper: {error}");
                std::process::exit(3);
            }
            return;
        }
        Some("--rename-helper") => {
            if let Err(error) = run_rename_helper() {
                eprintln!("process_event_bench rename helper: {error}");
                std::process::exit(3);
            }
            return;
        }
        _ => {}
    }
    if let Err(error) = run().await {
        eprintln!("process_event_bench: {error}");
        eprintln!(
            "usage: cargo run --release --features linux-process-events \
             --example process_event_bench -- <poll|cn-proc|ebpf> \
             <idle-ms> <iterations> [churn-rate-hz] [churn-duration-ms]"
        );
        std::process::exit(2);
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    let alias = ExecutableAlias::create()?;
    let mut initial_scan_helper = spawn_exec_helper(&alias.path)?;
    trigger_helper(&mut initial_scan_helper)?;
    if !wait_for_helper_exec(initial_scan_helper.pid, EVENT_TIMEOUT).await? {
        return Err("timed out preparing the initial-scan target helper".into());
    }

    let provider = NativeProcessProvider::open_process_events(args.mode.event_mode())?;
    let loader = LazerOffsetLoader::new(
        DisabledFetcher,
        PathBuf::from("/dev/null/vosu-process-event-bench"),
        Bytes::new(),
    )
    .with_endpoints(Vec::<String>::new());
    let runtime = InstanceRuntime::start(provider, loader);

    wait_for_scan(&runtime, 0).await?;
    if !wait_for_pid(&runtime, initial_scan_helper.pid, true, EVENT_TIMEOUT).await? {
        return Err("initial scan did not publish the exact preflight target PID".into());
    }
    rename_initial_helper(&mut initial_scan_helper)?;
    if !wait_for_helper_comm(initial_scan_helper.pid, "bench-away", EVENT_TIMEOUT).await? {
        return Err(
            "timed out confirming initial-scan target rename away from candidate comm".into(),
        );
    }
    let initial_scan_remove_started = Instant::now();
    stop_child(&mut initial_scan_helper).await?;
    if !wait_for_pid(&runtime, initial_scan_helper.pid, false, EVENT_TIMEOUT).await? {
        return Err("timed out promptly removing the initial-scan target PID".into());
    }
    let initial_scan_remove_latency = initial_scan_remove_started.elapsed();
    require_backend_healthy(&runtime, &args, "initial-scan preflight")?;

    warm_runtime(&runtime, &alias.path).await?;
    require_backend_healthy(&runtime, &args, "warm-up")?;
    let initial_events = runtime.process_event_count();
    let initial_scans = runtime.discovery_scan_count();
    let initial_targeted_inspections = runtime.targeted_inspection_count();
    let initial_event_wakes = runtime.event_wake_count();
    let initial_reconciliation_requests = runtime.reconciliation_request_count();

    let idle = measure_idle(&runtime, args.idle).await?;
    require_backend_healthy(&runtime, &args, "idle phase")?;
    let (lifecycle_metrics, lifecycle, churn_spawned) =
        measure_lifecycle_under_churn(&runtime, &args, &alias.path).await?;
    require_backend_healthy(&runtime, &args, "lifecycle phase")?;

    let results = Results {
        initial_scan_remove_latency,
        idle,
        lifecycle,
        churn_spawned,
        lifecycle_metrics,
        total_events: runtime.process_event_count().saturating_sub(initial_events),
        total_scans: runtime.discovery_scan_count().saturating_sub(initial_scans),
        total_targeted_inspections: runtime
            .targeted_inspection_count()
            .saturating_sub(initial_targeted_inspections),
        total_event_wakes: runtime
            .event_wake_count()
            .saturating_sub(initial_event_wakes),
        total_reconciliation_requests: runtime
            .reconciliation_request_count()
            .saturating_sub(initial_reconciliation_requests),
    };
    runtime.shutdown().await;

    let output = output_json(&args, &results);
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, &output)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn parse_args() -> Result<Args, String> {
    let mut args = env::args();
    let _program = args.next();
    let mode = Mode::parse(
        &args
            .next()
            .ok_or_else(|| "missing benchmark mode".to_owned())?,
    )?;
    let idle_ms = parse_positive(args.next(), "idle-ms")?;
    let iterations_u64 = parse_positive(args.next(), "iterations")?;
    let churn_rate_hz = parse_optional(args.next(), "churn-rate-hz", DEFAULT_CHURN_RATE_HZ)?;
    let churn_duration_ms = parse_optional(
        args.next(),
        "churn-duration-ms",
        duration_ms(DEFAULT_CHURN_DURATION),
    )?;
    if args.next().is_some() {
        return Err("unexpected extra argument".to_owned());
    }

    let idle = Duration::from_millis(idle_ms);
    let churn_duration = Duration::from_millis(churn_duration_ms);
    if idle > MAX_DURATION || churn_duration > MAX_DURATION {
        return Err(format!(
            "idle-ms and churn-duration-ms must not exceed {}",
            MAX_DURATION.as_millis()
        ));
    }
    if churn_rate_hz > MAX_CHURN_RATE_HZ {
        return Err(format!("churn-rate-hz must not exceed {MAX_CHURN_RATE_HZ}"));
    }
    let iterations = usize::try_from(iterations_u64)
        .map_err(|_| "iterations is too large for this platform".to_owned())?;
    if iterations > MAX_ITERATIONS {
        return Err(format!("iterations must not exceed {MAX_ITERATIONS}"));
    }

    Ok(Args {
        mode,
        idle,
        iterations,
        churn_rate_hz,
        churn_duration,
    })
}

fn parse_positive(value: Option<String>, name: &str) -> Result<u64, String> {
    let value = value.ok_or_else(|| format!("missing {name}"))?;
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("invalid {name} {value:?}; expected a positive integer"))?;
    if parsed == 0 {
        return Err(format!("{name} must be greater than zero"));
    }
    Ok(parsed)
}

fn parse_optional(value: Option<String>, name: &str, default: u64) -> Result<u64, String> {
    value.map_or(Ok(default), |value| parse_positive(Some(value), name))
}

async fn warm_runtime(runtime: &Runtime, alias: &Path) -> Result<(), Box<dyn Error>> {
    let mut helper = spawn_exec_helper(alias)?;
    trigger_helper(&mut helper)?;
    if !wait_for_pid(runtime, helper.pid, true, EVENT_TIMEOUT).await? {
        return Err("timed out warming the runtime with the exact target PID".into());
    }
    stop_child(&mut helper).await?;
    if !wait_for_pid(runtime, helper.pid, false, CLEANUP_TIMEOUT).await? {
        return Err("timed out removing the warm-up target PID".into());
    }
    Ok(())
}

async fn wait_for_helper_exec(pid: Pid, timeout: Duration) -> io::Result<bool> {
    wait_for_helper_comm(pid, "osu!.exe", timeout).await
}

async fn wait_for_helper_comm(pid: Pid, expected: &str, timeout: Duration) -> io::Result<bool> {
    let comm = PathBuf::from(format!("/proc/{pid}/comm"));
    let deadline = TokioInstant::now() + timeout;
    loop {
        match fs::read_to_string(&comm) {
            Ok(name) if name.trim_end() == expected => return Ok(true),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if TokioInstant::now() >= deadline {
            return Ok(false);
        }
        sleep(Duration::from_millis(1)).await;
    }
}

async fn measure_idle(runtime: &Runtime, duration: Duration) -> io::Result<PhaseMetrics> {
    let started = TokioInstant::now();
    let deadline = started + duration;
    let scans_before = runtime.discovery_scan_count();
    let inspections_before = runtime.targeted_inspection_count();
    let events_before = runtime.process_event_count();
    let event_wakes_before = runtime.event_wake_count();
    let reconciliation_requests_before = runtime.reconciliation_request_count();
    let usage_before = get_resource_usage()?;
    sleep_until(deadline).await;
    let usage_after = get_resource_usage()?;
    Ok(PhaseMetrics {
        elapsed: started.elapsed(),
        usage: subtract_resource_usage(usage_after, usage_before),
        scans: runtime.discovery_scan_count().saturating_sub(scans_before),
        targeted_inspections: runtime
            .targeted_inspection_count()
            .saturating_sub(inspections_before),
        events: runtime.process_event_count().saturating_sub(events_before),
        event_wakes: runtime
            .event_wake_count()
            .saturating_sub(event_wakes_before),
        reconciliation_requests: runtime
            .reconciliation_request_count()
            .saturating_sub(reconciliation_requests_before),
    })
}

async fn measure_lifecycle_under_churn(
    runtime: &Runtime,
    args: &Args,
    alias: &Path,
) -> Result<(LifecycleMetrics, PhaseMetrics, usize), Box<dyn Error>> {
    let scans_before = runtime.discovery_scan_count();
    let inspections_before = runtime.targeted_inspection_count();
    let events_before = runtime.process_event_count();
    let event_wakes_before = runtime.event_wake_count();
    let reconciliation_requests_before = runtime.reconciliation_request_count();
    let usage_before = get_resource_usage()?;
    let phase_started = TokioInstant::now();
    let deadline = phase_started + args.churn_duration;
    let churn_task = tokio::spawn(run_unrelated_churn(
        args.churn_rate_hz,
        phase_started,
        deadline,
    ));

    let lifecycle_result = timeout_at(
        deadline,
        measure_lifecycles(runtime, args.iterations, alias),
    )
    .await;
    let churn_spawned = churn_task.await??;
    let lifecycle_metrics = match lifecycle_result {
        Ok(result) => result?,
        Err(_) => {
            return Err(format!(
                "lifecycle workload did not complete within the fixed {} ms churn phase \
                 (spawned {churn_spawned} churn processes); increase churn-duration-ms",
                args.churn_duration.as_millis()
            )
            .into());
        }
    };
    let usage_after = get_resource_usage()?;
    let metrics = PhaseMetrics {
        elapsed: phase_started.elapsed(),
        usage: subtract_resource_usage(usage_after, usage_before),
        scans: runtime.discovery_scan_count().saturating_sub(scans_before),
        targeted_inspections: runtime
            .targeted_inspection_count()
            .saturating_sub(inspections_before),
        events: runtime.process_event_count().saturating_sub(events_before),
        event_wakes: runtime
            .event_wake_count()
            .saturating_sub(event_wakes_before),
        reconciliation_requests: runtime
            .reconciliation_request_count()
            .saturating_sub(reconciliation_requests_before),
    };
    Ok((lifecycle_metrics, metrics, churn_spawned))
}

async fn measure_lifecycles(
    runtime: &Runtime,
    iterations: usize,
    alias: &Path,
) -> Result<LifecycleMetrics, Box<dyn Error>> {
    let (add_latencies, add_timeouts, remove_latencies, remove_timeouts) =
        measure_exec_lifecycle(runtime, iterations, alias).await?;
    let (rename_latencies, rename_timeouts) = measure_rename_lifecycle(runtime, iterations).await?;
    Ok(LifecycleMetrics {
        add_latencies,
        add_timeouts,
        rename_latencies,
        rename_timeouts,
        remove_latencies,
        remove_timeouts,
    })
}

async fn measure_exec_lifecycle(
    runtime: &Runtime,
    iterations: usize,
    alias: &Path,
) -> Result<(Vec<Duration>, usize, Vec<Duration>, usize), Box<dyn Error>> {
    let mut add_latencies = Vec::with_capacity(iterations);
    let mut remove_latencies = Vec::with_capacity(iterations);
    let mut add_timeouts = 0;
    let mut remove_timeouts = 0;
    let phase_origin = TokioInstant::now();
    for iteration in 0..iterations {
        let mut helper = spawn_exec_helper(alias)?;
        if wait_for_pid(runtime, helper.pid, true, Duration::ZERO).await? {
            return Err(format!(
                "generic exec helper PID {} was already targeted",
                helper.pid
            )
            .into());
        }
        sleep_until_iteration_phase(phase_origin, iteration, iterations).await;

        let add_started = Instant::now();
        trigger_helper(&mut helper)?;
        if wait_for_pid(runtime, helper.pid, true, EVENT_TIMEOUT).await? {
            add_latencies.push(add_started.elapsed());
        } else {
            add_timeouts += 1;
            stop_child(&mut helper).await?;
            require_pid_absent(runtime, helper.pid).await?;
            continue;
        }

        let remove_started = Instant::now();
        stop_child(&mut helper).await?;
        if wait_for_pid(runtime, helper.pid, false, EVENT_TIMEOUT).await? {
            remove_latencies.push(remove_started.elapsed());
        } else {
            remove_timeouts += 1;
        }
        require_pid_absent(runtime, helper.pid).await?;
    }
    Ok((
        add_latencies,
        add_timeouts,
        remove_latencies,
        remove_timeouts,
    ))
}

async fn measure_rename_lifecycle(
    runtime: &Runtime,
    iterations: usize,
) -> Result<(Vec<Duration>, usize), Box<dyn Error>> {
    let mut latencies = Vec::with_capacity(iterations);
    let mut timeouts = 0;
    let phase_origin = TokioInstant::now();
    for iteration in 0..iterations {
        let mut helper = spawn_rename_helper()?;
        if wait_for_pid(runtime, helper.pid, true, Duration::ZERO).await? {
            return Err(format!(
                "generic rename helper PID {} was already targeted",
                helper.pid
            )
            .into());
        }
        sleep_until_iteration_phase(phase_origin, iteration, iterations).await;

        let started = Instant::now();
        trigger_helper(&mut helper)?;
        if wait_for_pid(runtime, helper.pid, true, EVENT_TIMEOUT).await? {
            latencies.push(started.elapsed());
        } else {
            timeouts += 1;
        }
        stop_child(&mut helper).await?;
        require_pid_absent(runtime, helper.pid).await?;
    }
    Ok((latencies, timeouts))
}

async fn run_unrelated_churn(
    rate_hz: u64,
    phase_origin: TokioInstant,
    deadline: TokioInstant,
) -> io::Result<usize> {
    let interval = Duration::from_nanos(1_000_000_000 / rate_hz);
    let mut next_slot = 0_u128;
    let mut spawned = 0;
    loop {
        let now = TokioInstant::now();
        if now >= deadline {
            break;
        }

        let current_slot =
            now.saturating_duration_since(phase_origin).as_nanos() / interval.as_nanos();
        next_slot = next_slot.max(current_slot);
        let spawn_deadline = phase_origin + duration_from_nanos(interval.as_nanos() * next_slot);
        if spawn_deadline >= deadline {
            break;
        }
        sleep_until(spawn_deadline).await;
        if TokioInstant::now() >= deadline {
            break;
        }

        let child = Command::new("/bin/true").spawn()?;
        spawned += 1;
        next_slot += 1;
        let reaper = tokio::task::spawn_blocking(move || {
            let mut child = child;
            child.wait()
        });
        match timeout_at(deadline, reaper).await {
            Ok(result) => {
                result
                    .map_err(|error| io::Error::other(format!("churn reaper failed: {error}")))??;
            }
            Err(_) => break,
        }
    }
    sleep_until(deadline).await;
    Ok(spawned)
}

fn spawn_exec_helper(alias: &Path) -> io::Result<GatedChild> {
    let child = Command::new("/bin/sh")
        .args([
            "-c",
            "IFS= read -r _; exec \"$1\" --target-helper",
            "vosu-process-event-gate",
        ])
        .arg(alias)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(GatedChild {
        pid: child.id(),
        child: Some(child),
    })
}

fn spawn_rename_helper() -> io::Result<GatedChild> {
    let child = Command::new(env::current_exe()?)
        .arg("--rename-helper")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(GatedChild {
        pid: child.id(),
        child: Some(child),
    })
}

fn run_initial_target_helper() -> io::Result<()> {
    let mut gate = [0_u8; 1];
    io::stdin().read_exact(&mut gate)?;
    let name = b"bench-away\0";
    // SAFETY: PR_SET_NAME reads at most 16 bytes from this valid NUL-terminated
    // static buffer and does not retain its pointer.
    if unsafe { libc::prctl(libc::PR_SET_NAME, name.as_ptr() as libc::c_ulong, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    std::thread::sleep(Duration::from_secs(10));
    Ok(())
}

fn run_rename_helper() -> io::Result<()> {
    let mut gate = [0_u8; 1];
    io::stdin().read_exact(&mut gate)?;
    let name = b"osu!.exe\0";
    // SAFETY: PR_SET_NAME reads at most 16 bytes from this valid NUL-terminated
    // static buffer and does not retain its pointer.
    if unsafe { libc::prctl(libc::PR_SET_NAME, name.as_ptr() as libc::c_ulong, 0, 0, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    std::thread::sleep(Duration::from_secs(10));
    Ok(())
}

fn trigger_helper(helper: &mut GatedChild) -> io::Result<()> {
    helper
        .child_mut()?
        .stdin
        .as_mut()
        .ok_or_else(|| io::Error::other("helper stdin was unavailable"))?
        .write_all(b"go\n")
}

fn rename_initial_helper(helper: &mut GatedChild) -> io::Result<()> {
    helper
        .child_mut()?
        .stdin
        .as_mut()
        .ok_or_else(|| io::Error::other("helper stdin was unavailable"))?
        .write_all(b"rename\n")
}

async fn stop_child(helper: &mut GatedChild) -> io::Result<()> {
    let Some(child) = helper.child.take() else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || {
        let mut child = child;
        let _ = child.kill();
        child.wait().map(|_| ())
    })
    .await
    .map_err(|error| io::Error::other(format!("helper reaper failed: {error}")))?
}

async fn require_pid_absent(runtime: &Runtime, pid: Pid) -> Result<(), Box<dyn Error>> {
    if wait_for_pid(runtime, pid, false, CLEANUP_TIMEOUT).await? {
        Ok(())
    } else {
        Err(format!("target PID {pid} remained published after cleanup").into())
    }
}

async fn wait_for_pid(
    runtime: &Runtime,
    pid: Pid,
    expected_present: bool,
    timeout: Duration,
) -> Result<bool, Box<dyn Error>> {
    let mut snapshots = runtime.subscribe_current();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let present = snapshots
            .borrow()
            .instances
            .iter()
            .any(|instance| instance.pid == pid);
        if present == expected_present {
            return Ok(true);
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(false);
        }
        match tokio::time::timeout_at(deadline, snapshots.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err("lifecycle snapshot publisher stopped".into()),
            Err(_) => return Ok(false),
        }
    }
}

fn require_backend_healthy(
    runtime: &Runtime,
    args: &Args,
    phase: &str,
) -> Result<(), Box<dyn Error>> {
    if matches!(args.mode, Mode::Poll) || runtime.process_events_healthy() {
        Ok(())
    } else {
        Err(format!(
            "requested {} process-event backend became unhealthy after {phase}; \
             refusing to report polling fallback measurements",
            args.mode.name()
        )
        .into())
    }
}

async fn wait_for_scan(runtime: &Runtime, previous: u64) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    while runtime.discovery_scan_count() <= previous {
        if Instant::now() >= deadline {
            return Err("timed out waiting for the initial discovery scan".into());
        }
        sleep(Duration::from_micros(100)).await;
    }
    Ok(())
}

fn iteration_phase(iteration: usize, iterations: usize) -> Duration {
    duration_from_nanos(POLL_TICK.as_nanos() * iteration as u128 / iterations as u128)
}

async fn sleep_until_iteration_phase(
    phase_origin: TokioInstant,
    iteration: usize,
    iterations: usize,
) {
    let now = TokioInstant::now();
    let period_nanos = POLL_TICK.as_nanos();
    let elapsed_periods = now.saturating_duration_since(phase_origin).as_nanos() / period_nanos;
    let mut deadline = phase_origin
        + duration_from_nanos(
            elapsed_periods * period_nanos + iteration_phase(iteration, iterations).as_nanos(),
        );
    if deadline <= now {
        deadline += POLL_TICK;
    }
    sleep_until(deadline).await;
}

fn get_resource_usage() -> io::Result<ResourceUsage> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the valid, uniquely borrowed output on success.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call initialized the complete rusage value.
    let usage = unsafe { usage.assume_init() };
    Ok(ResourceUsage {
        user_cpu: timeval_duration(usage.ru_utime)?,
        system_cpu: timeval_duration(usage.ru_stime)?,
        voluntary_context_switches: context_switch_count(usage.ru_nvcsw)?,
        involuntary_context_switches: context_switch_count(usage.ru_nivcsw)?,
    })
}

fn timeval_duration(value: libc::timeval) -> io::Result<Duration> {
    let seconds = u64::try_from(value.tv_sec)
        .map_err(|_| io::Error::other("getrusage returned negative time"))?;
    let micros = u32::try_from(value.tv_usec)
        .map_err(|_| io::Error::other("getrusage returned invalid microseconds"))?;
    if micros >= 1_000_000 {
        return Err(io::Error::other("getrusage microseconds out of range"));
    }
    Ok(Duration::new(seconds, micros * 1_000))
}

fn context_switch_count(value: libc::c_long) -> io::Result<u64> {
    u64::try_from(value).map_err(|_| io::Error::other("getrusage returned a negative count"))
}

fn subtract_resource_usage(after: ResourceUsage, before: ResourceUsage) -> ResourceUsage {
    ResourceUsage {
        user_cpu: after.user_cpu.saturating_sub(before.user_cpu),
        system_cpu: after.system_cpu.saturating_sub(before.system_cpu),
        voluntary_context_switches: after
            .voluntary_context_switches
            .saturating_sub(before.voluntary_context_switches),
        involuntary_context_switches: after
            .involuntary_context_switches
            .saturating_sub(before.involuntary_context_switches),
    }
}

fn latency_json(latencies: &[Duration]) -> Value {
    let mut sorted = latencies.to_vec();
    sorted.sort_unstable();
    sorted.first().map_or_else(
        || json!({"min_us": null, "median_us": null, "p95_us": null, "max_us": null}),
        |_| {
            json!({
                "min_us": duration_us(sorted[0]),
                "median_us": duration_us(median(&sorted)),
                "p95_us": duration_us(percentile_95(&sorted)),
                "max_us": duration_us(sorted[sorted.len() - 1])
            })
        },
    )
}

fn output_json(args: &Args, results: &Results) -> Value {
    let lifecycle = &results.lifecycle_metrics;
    let add_latency = latency_json(&lifecycle.add_latencies);
    let rename_latency = latency_json(&lifecycle.rename_latencies);
    let remove_latency = latency_json(&lifecycle.remove_latencies);
    let timeout_count =
        lifecycle.add_timeouts + lifecycle.rename_timeouts + lifecycle.remove_timeouts;
    let mode_caveat = match args.mode {
        Mode::Poll => {
            "poll measurements use the real discovery runtime at its normal 100 ms idle cadence; CPU includes actual /proc discovery scans"
        }
        Mode::CnProc => "CN_PROC is lossy and limited to the initial user and PID namespaces",
        Mode::Ebpf => {
            "eBPF is lossy, requires the initial PID namespace, and requires CAP_BPF plus CAP_PERFMON or a broader compatibility capability"
        }
    };

    let actual_churn_rate = if results.lifecycle.elapsed.is_zero() {
        0.0
    } else {
        results.churn_spawned as f64 / results.lifecycle.elapsed.as_secs_f64()
    };
    let churn = json!({
        "requested_duration_ms": duration_ms(args.churn_duration),
        "actual_duration_us": duration_us(results.lifecycle.elapsed),
        "target_rate_hz": args.churn_rate_hz,
        "actual_rate_hz": actual_churn_rate,
        "spawned_processes": results.churn_spawned,
        "event_count": results.lifecycle.events,
        "event_wake_count": results.lifecycle.event_wakes,
        "reconciliation_request_count": results.lifecycle.reconciliation_requests,
        "full_scan_count": results.lifecycle.scans,
        "discovery_scan_count": results.lifecycle.scans,
        "targeted_inspection_count": results.lifecycle.targeted_inspections,
        "user_cpu_us": duration_us(results.lifecycle.usage.user_cpu),
        "system_cpu_us": duration_us(results.lifecycle.usage.system_cpu),
        "voluntary_context_switches": results.lifecycle.usage.voluntary_context_switches,
        "involuntary_context_switches": results.lifecycle.usage.involuntary_context_switches
    });

    json!({
        "mode": args.mode.name(),
        "idle_duration_ms": duration_ms(args.idle),
        "iterations": args.iterations,
        "event_timeout_ms": duration_ms(EVENT_TIMEOUT),
        "cleanup_timeout_ms": duration_ms(CLEANUP_TIMEOUT),
        "poll_tick_ms": duration_ms(POLL_TICK),
        "event_count": results.total_events,
        "event_wake_count": results.total_event_wakes,
        "reconciliation_request_count": results.total_reconciliation_requests,
        "full_scan_count": results.total_scans,
        "discovery_scan_count": results.total_scans,
        "targeted_inspection_count": results.total_targeted_inspections,
        "initial_scan_preflight": {
            "remove_latency_us": duration_us(results.initial_scan_remove_latency),
            "excluded_from_metric_baselines": true
        },
        "timeout_count": timeout_count,
        "miss_count": timeout_count,
        "latency_sample_count": lifecycle.add_latencies.len(),
        "latency": add_latency,
        "add_timeout_count": lifecycle.add_timeouts,
        "add_miss_count": lifecycle.add_timeouts,
        "add_latency_sample_count": lifecycle.add_latencies.len(),
        "add_latency": add_latency,
        "comm_rename_timeout_count": lifecycle.rename_timeouts,
        "comm_rename_miss_count": lifecycle.rename_timeouts,
        "comm_rename_latency_sample_count": lifecycle.rename_latencies.len(),
        "comm_rename_latency": rename_latency,
        "remove_timeout_count": lifecycle.remove_timeouts,
        "remove_miss_count": lifecycle.remove_timeouts,
        "remove_latency_sample_count": lifecycle.remove_latencies.len(),
        "remove_latency": remove_latency,
        "latency_phase_schedule": {
            "period_ms": duration_ms(POLL_TICK),
            "start_inclusive_ms": 0,
            "end_exclusive_ms": duration_ms(POLL_TICK),
            "strategy": "desired iteration phase on the next future absolute period grid"
        },
        "user_cpu_us": duration_us(results.idle.usage.user_cpu),
        "system_cpu_us": duration_us(results.idle.usage.system_cpu),
        "voluntary_context_switches": results.idle.usage.voluntary_context_switches,
        "involuntary_context_switches": results.idle.usage.involuntary_context_switches,
        "idle": phase_json(&results.idle),
        "lifecycle": phase_json(&results.lifecycle),
        "churn": churn,
        "caveats": [
            mode_caveat,
            "initial-scan preflight starts an unprivileged copied target before InstanceRuntime, requires its exact PID from the initial full scan, renames its leader comm to noncandidate bench-away and confirms /proc observed the rename, then times termination/reap through prompt lifecycle disappearance; it is excluded from idle and lifecycle metric baselines",
            "add latency starts immediately before releasing a pre-spawned generic shell to exec the temporary osu!.exe alias and ends only when the lifecycle snapshot contains that preserved PID",
            "comm-rename latency starts immediately before releasing a persistent generic benchmark helper to PR_SET_NAME itself osu!.exe and ends only when the lifecycle snapshot contains that PID",
            "remove latency starts immediately before terminating an established target, includes termination and reap, and ends only when that exact PID disappears from the lifecycle snapshot",
            "unrelated /bin/true churn uses absolute spawn-slot deadlines for the fixed requested duration; missed slots are skipped, and churn cannot satisfy exact-PID lifecycle waits",
            "full-scan count is all completed reconciliation/discovery passes; targeted-inspection count is per-TGID delta inspection work; event-wake count is Tokio runtime process-event notifier branches taken, not OS or kernel wakeups; reconciliation-request count is newly latched backend or queue requests caused by source loss, parse/truncation, or bounded queue overflow, and excludes periodic five-second scans",
            "self CPU includes the benchmark, Tokio runtime, event backend, and discovery runtime"
        ]
    })
}

fn phase_json(metrics: &PhaseMetrics) -> Value {
    json!({
        "duration_us": duration_us(metrics.elapsed),
        "event_count": metrics.events,
        "event_wake_count": metrics.event_wakes,
        "reconciliation_request_count": metrics.reconciliation_requests,
        "full_scan_count": metrics.scans,
        "discovery_scan_count": metrics.scans,
        "targeted_inspection_count": metrics.targeted_inspections,
        "user_cpu_us": duration_us(metrics.usage.user_cpu),
        "system_cpu_us": duration_us(metrics.usage.system_cpu),
        "voluntary_context_switches": metrics.usage.voluntary_context_switches,
        "involuntary_context_switches": metrics.usage.involuntary_context_switches
    })
}

fn median(sorted: &[Duration]) -> Duration {
    let midpoint = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        duration_from_nanos((sorted[midpoint - 1].as_nanos() + sorted[midpoint].as_nanos()) / 2)
    } else {
        sorted[midpoint]
    }
}

fn percentile_95(sorted: &[Duration]) -> Duration {
    sorted[(sorted.len() * 95).div_ceil(100).saturating_sub(1)]
}

fn duration_from_nanos(nanos: u128) -> Duration {
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
