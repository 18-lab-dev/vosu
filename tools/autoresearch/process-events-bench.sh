#!/usr/bin/env bash
set -Eeuo pipefail

# rsync below publishes the working tree relative to the repository root.
cd "$(dirname -- "${BASH_SOURCE[0]}")/../.."

readonly REMOTE_HOST='vosu@127.0.0.1'
readonly REMOTE_PORT='2222'
readonly REMOTE_DIR='/home/vosu/vosu-process-events'
readonly REMOTE_BIN="${REMOTE_DIR}/target/release/examples/process_event_bench"
readonly IDLE_MS='1000'
readonly ITERATIONS='16'
readonly CHURN_RATE_HZ='50'
readonly CHURN_DURATION_MS='10000'
readonly SAMPLES='3'
readonly -a SSH=(ssh -p "$REMOTE_PORT" -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new)

fail() {
    printf 'autoresearch: %s\n' "$*" >&2
    exit 1
}

command -v rsync >/dev/null 2>&1 || fail 'rsync is required on the local host'
command -v python3 >/dev/null 2>&1 || fail 'python3 is required for local JSON validation'

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/vosu-autoresearch.XXXXXX") || fail 'could not create temporary result directory'
cleanup() {
    rm -rf -- "$work_dir"
}
trap cleanup EXIT
trap 'fail "failed at line ${LINENO}: ${BASH_COMMAND}"' ERR

"${SSH[@]}" "$REMOTE_HOST" \
    "test -e '$REMOTE_DIR/.git' && command -v rsync >/dev/null && command -v cargo >/dev/null && command -v setcap >/dev/null && command -v getcap >/dev/null" \
    || fail "remote checkout or required tools are unavailable in $REMOTE_DIR"

printf 'autoresearch: syncing worktree to benchmark VM\n' >&2
rsync --archive --delete --quiet \
    --exclude='/.git' \
    --exclude='/target/' \
    --exclude='/.autoresearch/' \
    --exclude='/.autoresearch-*/' \
    --exclude='/autoresearch-results/' \
    --exclude='/autoresearch-*.json' \
    --exclude='/autoresearch-*.log' \
    -e "ssh -p $REMOTE_PORT -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new" \
    ./ "${REMOTE_HOST}:${REMOTE_DIR}/" \
    || fail 'rsync to benchmark VM failed'

printf 'autoresearch: building incremental release benchmark\n' >&2
"${SSH[@]}" "$REMOTE_HOST" bash -s -- "$REMOTE_DIR" <<'REMOTE_BUILD'
set -Eeuo pipefail
repo=$1
cd "$repo"
cargo build --quiet --release --locked --features linux-process-events --example process_event_bench
bin="$repo/target/release/examples/process_event_bench"
test -x "$bin" || { printf 'autoresearch remote: benchmark binary was not produced: %s\n' "$bin" >&2; exit 1; }
sudo setcap -r "$bin" 2>/dev/null || true
sudo setcap 'cap_bpf,cap_perfmon,cap_sys_resource+ep' "$bin"
observed=$(getcap "$bin")
expected="$bin cap_sys_resource,cap_perfmon,cap_bpf=ep"
if [[ "$observed" != "$expected" ]]; then
    printf 'autoresearch remote: unexpected file capabilities\n  expected: %s\n  observed: %s\n' "$expected" "$observed" >&2
    exit 1
fi
REMOTE_BUILD

for mode in cn-proc ebpf; do
    for ((sample = 1; sample <= SAMPLES; sample++)); do
        result="$work_dir/${mode}.${sample}.json"
        printf 'autoresearch: running %s sample %d/%d\n' "$mode" "$sample" "$SAMPLES" >&2
        if ! "${SSH[@]}" "$REMOTE_HOST" timeout --foreground 120s \
            "$REMOTE_BIN" "$mode" "$IDLE_MS" "$ITERATIONS" "$CHURN_RATE_HZ" "$CHURN_DURATION_MS" \
            >"$result"; then
            fail "$mode sample $sample failed on benchmark VM"
        fi
        [[ -s "$result" ]] || fail "$mode sample $sample produced no JSON"
    done
done

python3 - "$work_dir" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
modes = ("cn-proc", "ebpf")
records = {}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def integer(value, field, label):
    require(isinstance(value, int) and not isinstance(value, bool),
            f"{label}: {field} must be an integer, got {value!r}")
    return value


try:
    for mode in modes:
        samples = []
        for sample_number in range(1, 4):
            path = root / f"{mode}.{sample_number}.json"
            label = f"{mode} sample {sample_number}"
            try:
                with path.open("r", encoding="utf-8") as stream:
                    data = json.load(stream)
                    require(stream.read().strip() == "", f"{label}: trailing data after JSON record")
            except (OSError, json.JSONDecodeError) as error:
                raise ValueError(f"{label}: invalid JSON: {error}") from error

            require(isinstance(data, dict), f"{label}: JSON root must be an object")
            require(data.get("mode") == mode,
                    f"{label}: requested mode {mode!r}, reported {data.get('mode')!r}")
            require(integer(data.get("idle_duration_ms"), "idle_duration_ms", label) == 1000,
                    f"{label}: idle_duration_ms is not 1000")
            require(integer(data.get("iterations"), "iterations", label) == 16,
                    f"{label}: iterations is not 16")
            require(integer(data.get("timeout_count"), "timeout_count", label) == 0,
                    f"{label}: lifecycle timeout_count is nonzero")
            require(integer(data.get("reconciliation_request_count"),
                            "reconciliation_request_count", label) == 0,
                    f"{label}: reconciliation requests are nonzero")

            lifecycle = data.get("lifecycle")
            churn = data.get("churn")
            require(isinstance(lifecycle, dict), f"{label}: lifecycle object is missing")
            require(isinstance(churn, dict), f"{label}: churn object is missing")
            require(integer(lifecycle.get("reconciliation_request_count"),
                            "lifecycle.reconciliation_request_count", label) == 0,
                    f"{label}: lifecycle reconciliation requests are nonzero")
            require(integer(churn.get("requested_duration_ms"),
                            "churn.requested_duration_ms", label) == 10000,
                    f"{label}: churn duration is not 10000 ms")
            require(integer(churn.get("target_rate_hz"), "churn.target_rate_hz", label) == 50,
                    f"{label}: churn rate is not 50 Hz")
            require(integer(churn.get("spawned_processes"),
                            "churn.spawned_processes", label) == 500,
                    f"{label}: churn did not spawn exactly 500 processes")

            for field in ("add_latency_sample_count", "comm_rename_latency_sample_count",
                          "remove_latency_sample_count"):
                require(integer(data.get(field), field, label) == 16,
                        f"{label}: {field} is not 16")
            for field in ("add_timeout_count", "comm_rename_timeout_count", "remove_timeout_count"):
                require(integer(data.get(field), field, label) == 0,
                        f"{label}: {field} is nonzero")

            for field in ("user_cpu_us", "system_cpu_us", "child_user_cpu_us",
                          "child_system_cpu_us", "total_workload_cpu_us", "event_count",
                          "event_wake_count", "targeted_inspection_count",
                          "voluntary_context_switches", "involuntary_context_switches",
                          "child_voluntary_context_switches",
                          "child_involuntary_context_switches",
                          "total_workload_voluntary_context_switches",
                          "total_workload_involuntary_context_switches"):
                integer(lifecycle.get(field), f"lifecycle.{field}", label)
            self_cpu = lifecycle["user_cpu_us"] + lifecycle["system_cpu_us"]
            child_cpu = lifecycle["child_user_cpu_us"] + lifecycle["child_system_cpu_us"]
            require(child_cpu > 0,
                    f"{label}: reaped churn/helpers contributed no child CPU")
            require(lifecycle["total_workload_cpu_us"] == self_cpu + child_cpu,
                    f"{label}: total_workload_cpu_us does not equal self plus child CPU")
            require(
                lifecycle["total_workload_voluntary_context_switches"]
                == lifecycle["voluntary_context_switches"]
                + lifecycle["child_voluntary_context_switches"],
                f"{label}: total voluntary context switches do not equal self plus child")
            require(
                lifecycle["total_workload_involuntary_context_switches"]
                == lifecycle["involuntary_context_switches"]
                + lifecycle["child_involuntary_context_switches"],
                f"{label}: total involuntary context switches do not equal self plus child")
            for field in ("add_latency", "comm_rename_latency", "remove_latency"):
                latency = data.get(field)
                require(isinstance(latency, dict), f"{label}: {field} object is missing")
                integer(latency.get("median_us"), f"{field}.median_us", label)

            samples.append(data)
        records[mode] = samples
except (ValueError, KeyError) as error:
    print(f"autoresearch: validation failed: {error}", file=sys.stderr)
    raise SystemExit(1)


def median(values):
    ordered = sorted(values)
    return ordered[len(ordered) // 2]


def lifecycle_values(samples, field):
    return [sample["lifecycle"][field] for sample in samples]


metrics = {}
for mode, samples in records.items():
    prefix = mode.replace("-", "_")
    self_cpu_totals = [
        sample["lifecycle"]["user_cpu_us"] + sample["lifecycle"]["system_cpu_us"]
        for sample in samples
    ]
    child_cpu_totals = [
        sample["lifecycle"]["child_user_cpu_us"] + sample["lifecycle"]["child_system_cpu_us"]
        for sample in samples
    ]
    metrics[f"{prefix}_lifecycle_self_cpu_us"] = median(self_cpu_totals)
    metrics[f"{prefix}_lifecycle_child_cpu_us"] = median(child_cpu_totals)
    metrics[f"{prefix}_lifecycle_total_workload_cpu_us"] = median(
        lifecycle_values(samples, "total_workload_cpu_us"))
    metrics[f"{prefix}_lifecycle_user_cpu_us"] = median(lifecycle_values(samples, "user_cpu_us"))
    metrics[f"{prefix}_lifecycle_system_cpu_us"] = median(lifecycle_values(samples, "system_cpu_us"))
    metrics[f"{prefix}_lifecycle_child_user_cpu_us"] = median(
        lifecycle_values(samples, "child_user_cpu_us"))
    metrics[f"{prefix}_lifecycle_child_system_cpu_us"] = median(
        lifecycle_values(samples, "child_system_cpu_us"))
    for field in ("event_count", "event_wake_count", "targeted_inspection_count",
                  "voluntary_context_switches", "involuntary_context_switches",
                  "child_voluntary_context_switches",
                  "child_involuntary_context_switches",
                  "total_workload_voluntary_context_switches",
                  "total_workload_involuntary_context_switches"):
        metrics[f"{prefix}_lifecycle_{field}"] = median(lifecycle_values(samples, field))
    for output_name, field in (("add_latency_us", "add_latency"),
                               ("rename_latency_us", "comm_rename_latency"),
                               ("remove_latency_us", "remove_latency")):
        metrics[f"{prefix}_{output_name}"] = median(
            [sample[field]["median_us"] for sample in samples])

combined = (metrics["cn_proc_lifecycle_total_workload_cpu_us"]
            + metrics["ebpf_lifecycle_total_workload_cpu_us"])
print(f"METRIC combined_lifecycle_cpu_us={combined}")
for name, value in metrics.items():
    print(f"METRIC {name}={value}")
PY
