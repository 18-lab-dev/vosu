use super::*;

pub(super) fn get_resource_usage(who: libc::c_int) -> io::Result<ResourceUsage> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the valid, uniquely borrowed output on success.
    if unsafe { libc::getrusage(who, usage.as_mut_ptr()) } != 0 {
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

pub(super) fn subtract_resource_usage(
    after: ResourceUsage,
    before: ResourceUsage,
) -> ResourceUsage {
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

pub(super) fn output_json(args: &Args, results: &Results) -> Value {
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
            "unprefixed CPU and context-switch fields are RUSAGE_SELF for the benchmark, Tokio runtime, event backend, and discovery runtime; child fields are RUSAGE_CHILDREN for reaped churn and lifecycle helpers; total-workload fields saturating-sum self and child usage"
        ]
    })
}

fn phase_json(metrics: &PhaseMetrics) -> Value {
    let child_user_cpu_us = duration_us(metrics.child_usage.user_cpu);
    let child_system_cpu_us = duration_us(metrics.child_usage.system_cpu);
    let total_workload_cpu = metrics
        .usage
        .user_cpu
        .saturating_add(metrics.usage.system_cpu)
        .saturating_add(metrics.child_usage.user_cpu)
        .saturating_add(metrics.child_usage.system_cpu);
    let total_workload_voluntary_context_switches = metrics
        .usage
        .voluntary_context_switches
        .saturating_add(metrics.child_usage.voluntary_context_switches);
    let total_workload_involuntary_context_switches = metrics
        .usage
        .involuntary_context_switches
        .saturating_add(metrics.child_usage.involuntary_context_switches);
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
        "child_user_cpu_us": child_user_cpu_us,
        "child_system_cpu_us": child_system_cpu_us,
        "total_workload_cpu_us": duration_us(total_workload_cpu),
        "voluntary_context_switches": metrics.usage.voluntary_context_switches,
        "involuntary_context_switches": metrics.usage.involuntary_context_switches,
        "child_voluntary_context_switches": metrics.child_usage.voluntary_context_switches,
        "child_involuntary_context_switches": metrics.child_usage.involuntary_context_switches,
        "total_workload_voluntary_context_switches": total_workload_voluntary_context_switches,
        "total_workload_involuntary_context_switches": total_workload_involuntary_context_switches
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

pub(super) fn duration_from_nanos(nanos: u128) -> Duration {
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

pub(super) fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
