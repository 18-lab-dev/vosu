#[cfg(not(target_os = "linux"))]
compile_error!("process_event_lazer_bench is only supported on Linux");

use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;
use tokio::time::{sleep, timeout_at};
use vosu::instance::{InstanceRuntime, NativeProcessProvider, Pid, ProcessEventMode};
use vosu::memory::lazer_loader::{FetchError, FetchFuture, LazerOffsetLoader, OffsetFetcher};

const EVENT_TIMEOUT: Duration = Duration::from_secs(15);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(7);
const DEFAULT_DWELL: Duration = Duration::from_secs(10);

type Runtime = InstanceRuntime<NativeProcessProvider, DisabledFetcher>;

#[derive(Clone, Copy)]
struct ResourceUsage {
    user_cpu: Duration,
    system_cpu: Duration,
    voluntary_context_switches: u64,
    involuntary_context_switches: u64,
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
                message: "network disabled by live lazer benchmark".to_owned(),
            })
        })
    }
}

struct Client {
    child: Option<Child>,
    process_group: Pid,
}

impl Client {
    fn spawn(executable: &PathBuf) -> io::Result<Self> {
        let child = Command::new(executable)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self {
            process_group: child.id(),
            child: Some(child),
        })
    }

    fn child_mut(&mut self) -> io::Result<&mut Child> {
        self.child
            .as_mut()
            .ok_or_else(|| io::Error::other("lazer launcher was already reaped"))
    }

    fn terminate_group(&self) -> io::Result<()> {
        let process_group = i32::try_from(self.process_group)
            .map_err(|_| io::Error::other("lazer process group exceeded pid_t"))?;
        // SAFETY: a negative PID addresses the child-owned process group. The
        // benchmark created that group and never stores zero as its identifier.
        if unsafe { libc::kill(-process_group, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    async fn terminate_and_reap(&mut self) -> io::Result<()> {
        self.terminate_group()?;
        let Some(child) = self.child.take() else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || {
            let mut child = child;
            child.wait().map(|_| ())
        })
        .await
        .map_err(|error| io::Error::other(format!("lazer launcher reaper failed: {error}")))?
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.terminate_group();
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.wait();
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("process_event_lazer_bench: {error}");
        eprintln!(
            "usage: process_event_lazer_bench <poll|cn-proc|ebpf> <lazer-executable> [dwell-ms]"
        );
        std::process::exit(2);
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let (mode, executable, dwell) = parse_args()?;
    let provider = NativeProcessProvider::open_process_events(match mode.as_str() {
        "poll" => None,
        "cn-proc" => Some(ProcessEventMode::CnProc),
        "ebpf" => Some(ProcessEventMode::Ebpf),
        _ => unreachable!("validated benchmark mode"),
    })?;
    let loader = LazerOffsetLoader::new(
        DisabledFetcher,
        PathBuf::from("/dev/null/vosu-process-event-lazer-bench"),
        Bytes::new(),
    )
    .with_endpoints(Vec::<String>::new());
    let runtime = InstanceRuntime::start(provider, loader);
    wait_for_scan(&runtime).await?;

    let baseline = runtime
        .subscribe_current()
        .borrow()
        .instances
        .iter()
        .map(|instance| instance.pid)
        .collect::<HashSet<_>>();
    let events_before = runtime.process_event_count();
    let scans_before = runtime.discovery_scan_count();
    let inspections_before = runtime.targeted_inspection_count();
    let event_wakes_before = runtime.event_wake_count();
    let reconciliation_requests_before = runtime.reconciliation_request_count();
    let usage_before = get_resource_usage()?;
    let add_started = Instant::now();
    let mut client = Client::spawn(&executable)?;
    let launcher_pid = client.process_group;

    let Some(target_pid) = wait_for_new_pid(&runtime, &baseline, EVENT_TIMEOUT).await? else {
        let status = client.child_mut()?.try_wait()?;
        return Err(format!(
            "no new lazer target was published within {} ms; launcher PID {launcher_pid}, status: {status:?}",
            EVENT_TIMEOUT.as_millis()
        )
        .into());
    };
    let add_latency = add_started.elapsed();
    let (target_process_group, target_start_time) = proc_identity(target_pid)?;
    if target_process_group != launcher_pid {
        return Err(format!(
            "published lazer PID {target_pid} belonged to process group {target_process_group}, \
             not controlled launcher group {launcher_pid}"
        )
        .into());
    }
    if !matches!(mode.as_str(), "poll") && !runtime.process_events_healthy() {
        return Err("eBPF backend became unhealthy after lazer discovery".into());
    }

    sleep(dwell).await;
    let launcher_status = client
        .child_mut()?
        .try_wait()?
        .map(|status| status.to_string());
    if let Some(status) = launcher_status.as_deref() {
        return Err(format!(
            "lazer launcher exited before the {dwell:?} dwell completed with {status}"
        )
        .into());
    }

    let remove_started = Instant::now();
    client.terminate_and_reap().await?;
    if !wait_for_pid(&runtime, target_pid, false, CLEANUP_TIMEOUT).await? {
        return Err(
            format!("exact lazer PID {target_pid} remained published after termination").into(),
        );
    }
    let remove_latency = remove_started.elapsed();
    let usage = subtract_resource_usage(get_resource_usage()?, usage_before);

    let output = json!({
        "mode": mode,
        "client_executable": executable,
        "launcher_pid": launcher_pid,
        "launcher_status_before_termination": launcher_status,
        "target_pid": target_pid,
        "target_process_group": target_process_group,
        "target_start_time_ticks": target_start_time,
        "exact_pid_attribution": true,
        "dwell_ms": dwell.as_millis(),
        "add_latency_us": add_latency.as_micros(),
        "remove_latency_us": remove_latency.as_micros(),
        "user_cpu_us": usage.user_cpu.as_micros(),
        "system_cpu_us": usage.system_cpu.as_micros(),
        "voluntary_context_switches": usage.voluntary_context_switches,
        "involuntary_context_switches": usage.involuntary_context_switches,
        "event_count": runtime.process_event_count().saturating_sub(events_before),
        "event_wake_count": runtime.event_wake_count().saturating_sub(event_wakes_before),
        "reconciliation_request_count": runtime
            .reconciliation_request_count()
            .saturating_sub(reconciliation_requests_before),
        "full_scan_count": runtime.discovery_scan_count().saturating_sub(scans_before),
        "targeted_inspection_count": runtime
            .targeted_inspection_count()
            .saturating_sub(inspections_before),
        "backend_healthy": runtime.process_events_healthy(),
        "caveat": "the AppImage launcher may fork; add ends when one new lazer target PID is published, attribution records its Linux start time and requires membership in the benchmark-owned process group; remove starts immediately before group termination, includes kill plus launcher reap, and ends when that exact target PID disappears; CPU time and context switches cover the complete live launch, dwell, and removal interval; full-scan count is all completed reconciliation/discovery passes; event-wake count is Tokio runtime process-event notifier branches taken, not OS or kernel wakeups; reconciliation-request count is newly latched backend or queue requests caused by source loss, parse/truncation, or bounded queue overflow, and excludes periodic five-second scans"
    });
    runtime.shutdown().await;

    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, &output)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn parse_args() -> Result<(String, PathBuf, Duration), String> {
    let mut args = env::args();
    let _program = args.next();
    let mode = args
        .next()
        .ok_or_else(|| "missing benchmark mode".to_owned())?;
    if !matches!(mode.as_str(), "poll" | "cn-proc" | "ebpf") {
        return Err(format!(
            "invalid mode {mode:?}; expected poll, cn-proc, or ebpf"
        ));
    }
    let executable = PathBuf::from(
        args.next()
            .ok_or_else(|| "missing lazer executable".to_owned())?,
    );
    if !executable.is_file() {
        return Err(format!(
            "lazer executable {} is not a file",
            executable.display()
        ));
    }
    let dwell = args.next().map_or(Ok(DEFAULT_DWELL), |value| {
        let millis = value
            .parse::<u64>()
            .map_err(|_| format!("invalid dwell-ms {value:?}; expected a positive integer"))?;
        if millis == 0 {
            return Err("dwell-ms must be greater than zero".to_owned());
        }
        Ok(Duration::from_millis(millis))
    })?;
    if args.next().is_some() {
        return Err("unexpected extra argument".to_owned());
    }
    Ok((mode, executable, dwell))
}

fn proc_identity(pid: Pid) -> Result<(Pid, u64), Box<dyn Error>> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let close = stat
        .rfind(')')
        .ok_or_else(|| io::Error::other("malformed proc stat command name"))?;
    let fields = stat
        .get(close + 2..)
        .ok_or_else(|| io::Error::other("malformed proc stat fields"))?
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    let process_group = fields
        .get(2)
        .ok_or_else(|| io::Error::other("proc stat missing process group"))?
        .parse::<Pid>()?;
    let start_time = fields
        .get(19)
        .ok_or_else(|| io::Error::other("proc stat missing start time"))?
        .parse::<u64>()?;
    Ok((process_group, start_time))
}

async fn wait_for_new_pid(
    runtime: &Runtime,
    baseline: &HashSet<Pid>,
    timeout: Duration,
) -> Result<Option<Pid>, Box<dyn Error>> {
    let mut snapshots = runtime.subscribe_current();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let new_pids = snapshots
            .borrow()
            .instances
            .iter()
            .map(|instance| instance.pid)
            .filter(|pid| !baseline.contains(pid))
            .collect::<Vec<_>>();
        match new_pids.as_slice() {
            [] => {}
            [pid] => return Ok(Some(*pid)),
            _ => {
                return Err(format!(
                    "lazer launch published multiple new targets; exact attribution is ambiguous: {new_pids:?}"
                )
                .into());
            }
        }
        match timeout_at(deadline, snapshots.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err("lifecycle snapshot publisher stopped".into()),
            Err(_) => return Ok(None),
        }
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
        match timeout_at(deadline, snapshots.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err("lifecycle snapshot publisher stopped".into()),
            Err(_) => return Ok(false),
        }
    }
}

async fn wait_for_scan(runtime: &Runtime) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + EVENT_TIMEOUT;
    while runtime.discovery_scan_count() == 0 {
        if Instant::now() >= deadline {
            return Err("timed out waiting for the initial discovery scan".into());
        }
        sleep(Duration::from_micros(100)).await;
    }
    Ok(())
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
