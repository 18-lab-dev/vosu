#!/usr/bin/env python3
"""Verification lever for `crates/vosu-sys/src/events/bpf/`: builds the real
shipped `process_events.bpf.o` on the host (via `cargo build -p vosu-sys
--features linux-process-events`, reading the object's path straight out of
that build's own `OUT_DIR`), copies it into a unique guest directory in the
`vosu-gaming` Linux VM, loads every program with `bpftool -d prog loadall`,
and checks the result against the object's expected five programs and each
program's processed-instruction count, verifier states, and verification
time against a budget. `--compile-in-guest` instead recompiles the source
in the guest with host-equivalent clang flags, for the rare case where the
host and guest toolchains need to be compared directly.

This never starts the VM (unlike `scripts/vm-test.py`, which can). Run it
whenever `crates/vosu-sys/src/events/bpf/` changes, per AGENTS.md.

    python scripts/ebpf-verify.py [--budget N] [--bench N] [--idle-ms MS]
                                   [--iterations N] [--churn-rate-hz HZ]
                                   [--churn-duration-ms MS]
                                   [--compile-in-guest]
                                   [--host HOST] [--port PORT]
                                   [--ssh-user USER]

Exits nonzero if the guest `bpftool` run itself fails (including a sudo
failure or a missing `bpftool`), if any of the object's five expected
programs is missing or an unexpected one appears, if any program fails to
load, or if any program exceeds the budget (default 100,000 processed
instructions -- a 10x margin under the kernel's real 1,000,000-instruction
verifier limit). With `--bench`, a bench-staging or bench-run failure also
fails the run.
"""

from __future__ import annotations

import argparse
import json
import re
import secrets
import shlex
import statistics
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BPF_SOURCE = ROOT / "crates/vosu-sys/src/events/bpf/process_events.bpf.c"
DEFAULT_BUDGET = 100_000
DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 2222
DEFAULT_SSH_USER = "vosu"
KNOWN_HOSTS = "/home/user/.ssh/known_hosts.vosu-gaming"
SSH_TIMEOUT_SECONDS = 120
HOST_BUILD_TIMEOUT_SECONDS = 600

# The programs `process_events.bpf.c` defines; a guest `bpftool -d prog
# loadall` run that is missing one (or reports one not in this set) fails
# regardless of budget or load status, since that means the run never
# actually verified what it claims to.
EXPECTED_PROGRAMS = frozenset(
    {
        "process_exec",
        "process_fork",
        "process_exit",
        "process_exit_group_dead",
        "task_rename",
    }
)


# --------------------------------------------------------------------------
# Pure helpers: clang/bpftool command construction and output parsing.
# Covered by scripts/test_ebpf_verify.py without touching the VM.
# --------------------------------------------------------------------------


def clang_compile_args(src, obj, header_dir, arch="x86_64"):
    """The clang invocation `libbpf_cargo::SkeletonBuilder::build` runs
    (`-g -O2 -target bpf -D__TARGET_ARCH_<arch> -c <src> -o <obj>`, a CO-RE
    header include, and `-fno-stack-protector`; see
    libbpf-cargo's build.rs `compile_single`/`with_compiler_args`). Used
    only by `--compile-in-guest`; the default path loads the host's own
    `SkeletonBuilder` output directly instead of approximating it."""
    core_arch = {
        "x86_64": "x86",
        "aarch64": "arm64",
        "powerpc64": "powerpc",
        "s390x": "s390",
        "riscv64": "riscv",
        "loongarch64": "loongarch",
        "sparc64": "sparc",
        "mips64": "mips",
    }.get(arch, arch)
    return [
        "clang",
        "-I",
        str(header_dir),
        "-fno-stack-protector",
        f"-D__TARGET_ARCH_{core_arch}",
        "-g",
        "-O2",
        "-target",
        "bpf",
        "-c",
        str(src),
        "-o",
        str(obj),
    ]


def remote_work_dir(token: str) -> str:
    return f"/home/vosu/ebpf-verify-{token}"


def remote_pin_dir(work_dir: str) -> str:
    """Derived from `work_dir`'s own unique token (see `remote_work_dir`)
    instead of taking a separate label, so the pin directory can never
    drift out of step with the work directory it belongs to."""
    token = Path(work_dir).name.removeprefix("ebpf-verify-")
    return f"/sys/fs/bpf/ebpf-verify-{token}"


def build_loadall_command(obj_path: str, pin_dir: str) -> str:
    """The remote command line: load every program, filter the (otherwise
    multi-hundred-MB, one line per explored verifier state) `-d` log down to
    the per-program summary lines this script parses, then remove the pins
    it created. `sudo` is required for `bpftool prog loadall`.

    Uses bash's `PIPESTATUS[0]` (not the pipeline's own `$?`) to capture
    `bpftool`'s exit status specifically: a `sudo` failure, a missing
    `bpftool`, or a crash mid-verification can leave `grep` seeing no
    matching line (so `grep`'s own status, and `$?` without `pipefail`,
    is unrelated to whether `bpftool` actually succeeded) or seeing a
    partial match despite `bpftool` still failing afterward -- either way
    the exit status this script checks must be `bpftool`'s own."""
    grep_pattern = (
        r"libbpf: prog '[^']+':( -- BEGIN PROG LOAD LOG --| pinned at| BPF program load failed)"
        r"|^processed [0-9]+ insns \(limit|^verification time [0-9]+ usec"
        r"|Processed [0-9]+ insn|too large|BPF program is too large"
    )
    script = (
        f"sudo rm -rf {pin_dir}; "
        f"sudo bpftool -d prog loadall {obj_path} {pin_dir} 2>&1 | grep -E {shlex.quote(grep_pattern)}; "
        f"status=${{PIPESTATUS[0]}}; sudo rm -rf {pin_dir}; exit $status"
    )
    return f"bash -c {shlex.quote(script)}"


def bench_command(
    binary: str, mode: str, idle_ms: int, iterations: int, churn_rate_hz: int, churn_duration_ms: int
) -> str:
    """Positional argv `process_event_bench` takes: `<mode> <idle-ms>
    <iterations> [churn-rate-hz] [churn-duration-ms]`."""
    return f"{binary} {mode} {idle_ms} {iterations} {churn_rate_hz} {churn_duration_ms}"


@dataclass
class ProgramVerification:
    name: str
    loaded: bool
    processed: int | None = None
    limit: int | None = None
    total_states: int | None = None
    peak_states: int | None = None
    verification_time_us: int | None = None
    error: str | None = None


_BEGIN_RE = re.compile(r"^libbpf: prog '([^']+)': -- BEGIN PROG LOAD LOG --$")
_FAILED_RE = re.compile(r"^libbpf: prog '([^']+)': BPF program load failed(?::\s*(.*))?$")
_PINNED_RE = re.compile(r"^libbpf: prog '([^']+)': pinned at")
_SUMMARY_RE = re.compile(
    r"^processed (\d+) insns \(limit (\d+)\) max_states_per_insn \d+ "
    r"total_states (\d+) peak_states (\d+)"
)
_TIME_RE = re.compile(r"^verification time (\d+) usec$")
_TOO_LARGE_RE = re.compile(r"(Processed \d+ insn.*|.*too large.*)", re.IGNORECASE)


def parse_loadall_output(text: str) -> list[ProgramVerification]:
    """Parses the grep-filtered output `build_loadall_command` produces into
    one `ProgramVerification` per program bpftool attempted to load, in the
    order they appeared -- whether that attempt is first announced by a
    `-- BEGIN PROG LOAD LOG --` line (the success path, and libbpf's older
    failure order) or by a `BPF program load failed` line (libbpf 1.7's
    order on failure: the warning prints before the verbose log). Either
    way the two lines for the same program name collapse into a single
    row rather than two. A program bpftool never attempted at all (e.g.
    the object failed to parse, or loading stopped after an earlier
    program's hard failure) is not represented -- callers that need "did
    every program in the object get attempted" should compare the result
    names against their own expected program set (see
    `EXPECTED_PROGRAMS`/`check_verification`)."""
    programs: list[ProgramVerification] = []
    current: ProgramVerification | None = None
    pending_error_lines: list[str] = []

    def flush():
        nonlocal current, pending_error_lines
        if current is not None:
            if pending_error_lines and current.error is None and not current.loaded:
                current.error = " / ".join(pending_error_lines)
            programs.append(current)
        current = None
        pending_error_lines = []

    for line in text.splitlines():
        line = line.rstrip()
        begin = _BEGIN_RE.match(line)
        if begin:
            name = begin.group(1)
            if current is not None and current.name == name:
                # This program's load-failed warning already opened this
                # row (libbpf 1.7's failure order); keep it instead of
                # starting a duplicate for the same program.
                continue
            flush()
            current = ProgramVerification(name=name, loaded=True)
            continue
        failed = _FAILED_RE.match(line)
        if failed:
            name = failed.group(1)
            if current is None or current.name != name:
                flush()
                current = ProgramVerification(name=name, loaded=False)
            else:
                current.loaded = False
            if failed.group(2):
                pending_error_lines.append(failed.group(2))
            continue
        pinned = _PINNED_RE.match(line)
        if pinned and current is not None and current.name == pinned.group(1):
            flush()
            continue
        if current is None:
            continue
        summary = _SUMMARY_RE.match(line)
        if summary:
            current.processed = int(summary.group(1))
            current.limit = int(summary.group(2))
            current.total_states = int(summary.group(3))
            current.peak_states = int(summary.group(4))
            continue
        time_match = _TIME_RE.match(line)
        if time_match:
            current.verification_time_us = int(time_match.group(1))
            continue
        if _TOO_LARGE_RE.match(line):
            pending_error_lines.append(line)
    flush()
    return programs


@dataclass
class BudgetResult:
    ok: bool
    violations: list[str] = field(default_factory=list)


def evaluate_budget(programs: list[ProgramVerification], budget: int) -> BudgetResult:
    violations = []
    for program in programs:
        if not program.loaded:
            violations.append(f"{program.name}: failed to load ({program.error or 'unknown error'})")
        elif program.processed is None:
            violations.append(f"{program.name}: no processed-instruction count parsed")
        elif program.processed > budget:
            violations.append(f"{program.name}: processed {program.processed} > budget {budget}")
    return BudgetResult(ok=not violations, violations=violations)


@dataclass
class VerificationResult:
    ok: bool
    violations: list[str] = field(default_factory=list)


def check_verification(
    rc: int,
    err: str,
    programs: list[ProgramVerification],
    budget: int,
    expected_programs: frozenset[str] = EXPECTED_PROGRAMS,
) -> VerificationResult:
    """Combines the guest command's own exit status, the parsed per-program
    table, and the expected program set into one pass/fail verdict:

    - A nonzero `bpftool` exit status (see `build_loadall_command`) fails
      outright. This is what catches a `sudo` failure, a missing
      `bpftool`, or any crash that leaves no per-program summary line for
      `parse_loadall_output` to see.
    - Any mismatch between the programs the parsed output actually names
      and `expected_programs` fails too, even when `rc == 0`: an empty
      parse (e.g. a map-create failure whose error text matches none of
      `build_loadall_command`'s grep patterns) ends up here as "every
      expected program missing".
    - Each present program still has to load within `budget`
      (`evaluate_budget`).
    """
    violations: list[str] = []
    if rc != 0:
        detail = err.strip()
        violations.append(f"bpftool loadall exited {rc}" + (f": {detail}" if detail else ""))
    names = {program.name for program in programs}
    missing = sorted(expected_programs - names)
    if missing:
        violations.append(f"missing expected program(s): {', '.join(missing)}")
    unexpected = sorted(names - expected_programs)
    if unexpected:
        violations.append(f"unexpected program(s) in object: {', '.join(unexpected)}")
    violations.extend(evaluate_budget(programs, budget).violations)
    return VerificationResult(ok=not violations, violations=violations)


def format_table(programs: list[ProgramVerification], budget: int) -> str:
    headers = ["program", "loaded", "processed", "limit", "total_states", "peak_states", "verify_us"]
    rows = [headers]
    for program in programs:
        rows.append(
            [
                program.name,
                "yes" if program.loaded else f"NO ({program.error or 'unknown'})",
                str(program.processed) if program.processed is not None else "-",
                str(program.limit) if program.limit is not None else "-",
                str(program.total_states) if program.total_states is not None else "-",
                str(program.peak_states) if program.peak_states is not None else "-",
                str(program.verification_time_us) if program.verification_time_us is not None else "-",
            ]
        )
    widths = [max(len(row[i]) for row in rows) for i in range(len(headers))]
    lines = []
    for index, row in enumerate(rows):
        lines.append(" | ".join(cell.ljust(widths[i]) for i, cell in enumerate(row)))
        if index == 0:
            lines.append("-+-".join("-" * w for w in widths))
    lines.append(f"\nbudget: {budget} processed instructions")
    return "\n".join(lines)


def summarize_bench_results(results: dict[str, list[str]]) -> dict[str, dict[str, float]]:
    """Reduces each mode's raw `process_event_bench` JSON samples (see
    `examples/support/process_event_metrics.rs::output_json`) to the two
    figures FilterFix3's manual table reported: lifecycle-phase CPU, self
    only (`lifecycle.user_cpu_us + lifecycle.system_cpu_us`, excluding
    reaped-child time, i.e. the backend's own cost) and the median
    add-latency (new-process detection latency)."""
    summary: dict[str, dict[str, float]] = {}
    for mode, samples in results.items():
        if not samples:
            continue
        cpu_values = []
        latency_values = []
        for sample_text in samples:
            data = json.loads(sample_text)
            lifecycle = data["lifecycle"]
            cpu_values.append(lifecycle["user_cpu_us"] + lifecycle["system_cpu_us"])
            latency_values.append(data["add_latency"]["median_us"])
        summary[mode] = {
            "lifecycle_cpu_us_mean": statistics.fmean(cpu_values),
            "lifecycle_cpu_us_min": min(cpu_values),
            "lifecycle_cpu_us_max": max(cpu_values),
            "detect_latency_us_mean": statistics.fmean(latency_values),
            "detect_latency_us_min": min(latency_values),
            "detect_latency_us_max": max(latency_values),
        }
    return summary


def format_bench_table(summary: dict[str, dict[str, float]]) -> str:
    lines = ["bench results:", f"{'mode':<10} | {'lifecycle CPU (own, us)':>28} | {'median detect latency (us)':>30}"]
    for mode, metrics in summary.items():
        cpu = f"{metrics['lifecycle_cpu_us_mean']:.0f} / {metrics['lifecycle_cpu_us_min']:.0f}-{metrics['lifecycle_cpu_us_max']:.0f}"
        latency = (
            f"{metrics['detect_latency_us_mean']:.1f} / "
            f"{metrics['detect_latency_us_min']:.0f}-{metrics['detect_latency_us_max']:.0f}"
        )
        lines.append(f"{mode:<10} | {cpu:>28} | {latency:>30}")
    return "\n".join(lines)


# --------------------------------------------------------------------------
# Errors.
# --------------------------------------------------------------------------


class EbpfVerifyError(RuntimeError):
    pass


# --------------------------------------------------------------------------
# Host build discovery: locates the exact artifacts the host's own build
# just produced, so the VM verifies and benchmarks what actually ships.
# --------------------------------------------------------------------------


def _cargo_metadata(cwd: Path = ROOT) -> dict:
    completed = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=60,
    )
    if completed.returncode != 0:
        raise EbpfVerifyError(f"cargo metadata failed:\n{completed.stderr}")
    return json.loads(completed.stdout)


def locate_host_bpf_object(cwd: Path = ROOT) -> Path:
    """Builds `vosu-sys` with `linux-process-events` on the host and returns
    the `process_events.bpf.o` its build script just wrote into that
    build's own `OUT_DIR`, read from the `build-script-executed` message in
    `--message-format=json-render-diagnostics` output -- the same object
    `include_bytes!` embeds into the shipped binary."""
    metadata = _cargo_metadata(cwd)
    package_id = next((p["id"] for p in metadata["packages"] if p["name"] == "vosu-sys"), None)
    if package_id is None:
        raise EbpfVerifyError("workspace metadata has no vosu-sys package")

    completed = subprocess.run(
        [
            "cargo",
            "build",
            "-p",
            "vosu-sys",
            "--features",
            "linux-process-events",
            "--message-format=json-render-diagnostics",
        ],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=HOST_BUILD_TIMEOUT_SECONDS,
    )
    if completed.returncode != 0:
        raise EbpfVerifyError(f"host cargo build -p vosu-sys failed:\n{completed.stderr}")

    out_dir = None
    for line in completed.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if message.get("reason") == "build-script-executed" and message.get("package_id") == package_id:
            out_dir = message.get("out_dir")
    if out_dir is None:
        raise EbpfVerifyError(
            "cargo build -p vosu-sys produced no build-script-executed message for vosu-sys "
            "with an out_dir; is the linux-process-events build script running?"
        )
    object_path = Path(out_dir) / "process_events.bpf.o"
    if not object_path.is_file():
        raise EbpfVerifyError(f"expected {object_path} after the host build, but it is missing")
    return object_path


def locate_host_bench_binary(cwd: Path = ROOT) -> Path:
    """Builds `process_event_bench` in release mode on the host and returns
    its path, for staging into the guest alongside the shipped BPF object."""
    metadata = _cargo_metadata(cwd)
    completed = subprocess.run(
        [
            "cargo",
            "build",
            "--release",
            "-p",
            "vosu",
            "--example",
            "process_event_bench",
            "--features",
            "linux-process-events",
        ],
        cwd=cwd,
        capture_output=True,
        text=True,
        timeout=HOST_BUILD_TIMEOUT_SECONDS,
    )
    if completed.returncode != 0:
        raise EbpfVerifyError(f"host release build of process_event_bench failed:\n{completed.stderr}")
    target_dir = Path(metadata["target_directory"])
    binary = target_dir / "release" / "examples" / "process_event_bench"
    if not binary.is_file():
        raise EbpfVerifyError(f"expected {binary} after the host release build, but it is missing")
    return binary


# --------------------------------------------------------------------------
# VM I/O: thin wrappers around `ssh`.
# --------------------------------------------------------------------------


class Vm:
    def __init__(self, host, port, user):
        self.host = host
        self.port = port
        self.user = user

    def _ssh_base(self):
        return [
            "ssh",
            "-p",
            str(self.port),
            "-o",
            "BatchMode=yes",
            "-o",
            f"UserKnownHostsFile={KNOWN_HOSTS}",
            "-o",
            "StrictHostKeyChecking=accept-new",
            f"{self.user}@{self.host}",
        ]

    def run(self, command: str, timeout=SSH_TIMEOUT_SECONDS):
        completed = subprocess.run(
            [*self._ssh_base(), command],
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return completed.returncode, completed.stdout, completed.stderr

    def put_text(self, remote_path: str, content: str):
        completed = subprocess.run(
            [*self._ssh_base(), f"cat > {remote_path}"],
            input=content,
            capture_output=True,
            text=True,
            timeout=SSH_TIMEOUT_SECONDS,
        )
        if completed.returncode != 0:
            raise EbpfVerifyError(f"writing {remote_path} failed: {completed.stderr}")

    def put_bytes(self, remote_path: str, content: bytes):
        completed = subprocess.run(
            [*self._ssh_base(), f"cat > {remote_path}"],
            input=content,
            capture_output=True,
            timeout=SSH_TIMEOUT_SECONDS,
        )
        if completed.returncode != 0:
            raise EbpfVerifyError(f"writing {remote_path} failed: {completed.stderr.decode(errors='replace')}")


# --------------------------------------------------------------------------
# Orchestration.
# --------------------------------------------------------------------------


def stage_host_object(vm: Vm, work_dir: str) -> str:
    """Copies the host-built BPF object (`locate_host_bpf_object`) into a
    unique guest directory and returns its remote path."""
    object_path = locate_host_bpf_object()
    rc, _, err = vm.run(f"mkdir -p {work_dir}")
    if rc != 0:
        raise EbpfVerifyError(f"mkdir {work_dir} failed: {err}")
    remote_obj = f"{work_dir}/process_events.bpf.o"
    vm.put_bytes(remote_obj, object_path.read_bytes())
    return remote_obj


def stage_and_build_in_guest(vm: Vm, work_dir: str) -> str:
    """`--compile-in-guest` fallback: stages the current BPF source (HEAD
    plus any uncommitted diff -- for this one self-contained source file
    that is exactly the live working tree) into `work_dir` and compiles it
    there with the host-equivalent clang flags. Returns the remote object
    path."""
    rc, _, err = vm.run(f"mkdir -p {work_dir}")
    if rc != 0:
        raise EbpfVerifyError(f"mkdir {work_dir} failed: {err}")
    source_text = BPF_SOURCE.read_text(encoding="utf-8")
    vm.put_text(f"{work_dir}/process_events.bpf.c", source_text)
    obj_path = f"{work_dir}/process_events.bpf.o"
    args = clang_compile_args(
        f"{work_dir}/process_events.bpf.c",
        obj_path,
        "/usr/include",
    )
    rc, out, err = vm.run(" ".join(args))
    if rc != 0:
        raise EbpfVerifyError(f"guest clang build failed: {out}\n{err}")
    return obj_path


def run_verification(vm: Vm, obj_path: str, pin_dir: str):
    rc, out, err = vm.run(build_loadall_command(obj_path, pin_dir))
    programs = parse_loadall_output(out)
    return rc, programs, err


def stage_bench_binary(vm: Vm, work_dir: str) -> str:
    """Copies the host-built release `process_event_bench` into the guest
    work directory and grants it the capabilities the eBPF backend needs,
    without which it can load events but not programs."""
    binary_path = locate_host_bench_binary()
    remote_binary = f"{work_dir}/process_event_bench"
    vm.put_bytes(remote_binary, binary_path.read_bytes())
    rc, out, err = vm.run(
        f"chmod +x {remote_binary} && "
        f"sudo setcap 'cap_bpf,cap_perfmon,cap_sys_resource+ep' {remote_binary}"
    )
    if rc != 0:
        raise EbpfVerifyError(f"staging process_event_bench in the guest failed: {out}\n{err}")
    return remote_binary


def run_bench(
    vm: Vm,
    binary: str,
    idle_ms: int,
    iterations: int,
    churn_rate_hz: int,
    churn_duration_ms: int,
    samples: int,
) -> dict[str, list[str]]:
    """Runs `samples` interleaved `ebpf`/`cn-proc` samples of the staged
    `process_event_bench` binary. Returns raw JSON text per (mode, sample);
    a run failure raises so the caller can decide whether to treat it as
    fatal."""
    results: dict[str, list[str]] = {"ebpf": [], "cn-proc": []}
    for sample in range(samples):
        for mode in ("ebpf", "cn-proc"):
            cmd = bench_command(binary, mode, idle_ms, iterations, churn_rate_hz, churn_duration_ms)
            rc, out, err = vm.run(cmd, timeout=120)
            if rc != 0:
                raise EbpfVerifyError(f"{mode} sample {sample} failed: {out}\n{err}")
            results[mode].append(out)
    return results


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--budget", type=int, default=DEFAULT_BUDGET)
    parser.add_argument("--bench", type=int, default=0, help="run N interleaved ebpf/cn-proc samples")
    parser.add_argument("--idle-ms", type=int, default=1_000, help="process_event_bench idle-phase duration for --bench")
    parser.add_argument(
        "--iterations", type=int, default=16, help="process_event_bench iteration count for --bench"
    )
    parser.add_argument("--churn-rate-hz", type=int, default=50)
    parser.add_argument("--churn-duration-ms", type=int, default=10_000)
    parser.add_argument(
        "--compile-in-guest",
        action="store_true",
        help="recompile the BPF source in the guest instead of loading the host's OUT_DIR object",
    )
    parser.add_argument("--host", default=DEFAULT_HOST)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--ssh-user", default=DEFAULT_SSH_USER)
    args = parser.parse_args(argv)

    vm = Vm(args.host, args.port, args.ssh_user)
    token = secrets.token_hex(4)
    work_dir = remote_work_dir(token)
    pin_dir = remote_pin_dir(work_dir)

    try:
        if args.compile_in_guest:
            obj_path = stage_and_build_in_guest(vm, work_dir)
        else:
            obj_path = stage_host_object(vm, work_dir)
        rc, programs, err = run_verification(vm, obj_path, pin_dir)
        print(format_table(programs, args.budget))
        result = check_verification(rc, err, programs, args.budget)
        if not result.ok:
            print("\nFAILED:")
            for violation in result.violations:
                print(f"  - {violation}")

        bench_ok = True
        if args.bench:
            try:
                binary = stage_bench_binary(vm, work_dir)
                bench_results = run_bench(
                    vm,
                    binary,
                    args.idle_ms,
                    args.iterations,
                    args.churn_rate_hz,
                    args.churn_duration_ms,
                    args.bench,
                )
                bench_summary = summarize_bench_results(bench_results)
                print("\n" + format_bench_table(bench_summary))
            except EbpfVerifyError as error:
                bench_ok = False
                print(f"\nbench FAILED: {error}")

        return 0 if (result.ok and bench_ok) else 1
    except EbpfVerifyError as error:
        print(f"FAILED: {error}", file=sys.stderr)
        return 1
    finally:
        try:
            vm.run(f"sudo rm -rf {pin_dir}; rm -rf {work_dir}")
        except Exception as cleanup_error:  # noqa: BLE001 - must never mask the real failure above
            print(f"cleanup warning: failed to remove {work_dir} / {pin_dir}: {cleanup_error}", file=sys.stderr)


if __name__ == "__main__":
    sys.exit(main())
