#!/usr/bin/env python3
"""Verification lever for `crates/vosu-sys/src/events/bpf/`: stages HEAD plus
any uncommitted diff into a unique guest directory in the `vosu-gaming` Linux
VM, compiles `process_events.bpf.o` there with the same clang flags and CO-RE
headers `libbpf_cargo::SkeletonBuilder` uses on the host, loads every program
with `bpftool -d prog loadall`, and reports each program's processed
instruction count, verifier states, and verification time against a budget.

This never starts the VM (unlike `scripts/vm-test.py`, which can). Run it
whenever `crates/vosu-sys/src/events/bpf/` changes, per AGENTS.md.

    python scripts/ebpf-verify.py [--budget N] [--bench N] [--host HOST]
                                   [--port PORT] [--ssh-user USER]

Exits nonzero if any program fails to load or exceeds the budget (default
100,000 processed instructions -- a 10x margin under the kernel's real
1,000,000-instruction verifier limit).
"""

from __future__ import annotations

import argparse
import re
import secrets
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


# --------------------------------------------------------------------------
# Pure helpers: clang/bpftool command construction and output parsing.
# Covered by scripts/test_ebpf_verify.py without touching the VM.
# --------------------------------------------------------------------------


def clang_compile_args(src, obj, header_dir, arch="x86_64"):
    """The clang invocation `libbpf_cargo::SkeletonBuilder::build` runs
    (`-g -O2 -target bpf -D__TARGET_ARCH_<arch> -c <src> -o <obj>`, a CO-RE
    header include, and `-fno-stack-protector`; see
    libbpf-cargo's build.rs `compile_single`/`with_compiler_args`), so a
    guest-built object is equivalent to the host's."""
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


def remote_pin_dir(work_dir: str, program_label: str) -> str:
    return f"/sys/fs/bpf/ebpf-verify-{program_label}"


def build_loadall_command(obj_path: str, pin_dir: str) -> str:
    """The remote command line: load every program, filter the (otherwise
    multi-hundred-MB, one line per explored verifier state) `-d` log down to
    the per-program summary lines this script parses, then remove the pins
    it created. `sudo` is required for `bpftool prog loadall`."""
    grep_pattern = (
        r"libbpf: prog '[^']+':( -- BEGIN PROG LOAD LOG --| pinned at| BPF program load failed)"
        r"|^processed [0-9]+ insns \(limit|^verification time [0-9]+ usec"
        r"|Processed [0-9]+ insn|too large|BPF program is too large"
    )
    return (
        f"sudo rm -rf {pin_dir}; "
        f"sudo bpftool -d prog loadall {obj_path} {pin_dir} 2>&1 "
        f"| grep -E \"{grep_pattern}\"; "
        f"status=$?; sudo rm -rf {pin_dir}; exit $status"
    )


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
    order they appeared. A program that never reaches `BEGIN PROG LOAD LOG`
    (e.g. the object failed to parse, or loading stopped after an earlier
    program's hard failure) is not represented -- callers that need "did
    every program in the object get attempted" should compare the result
    count against their own expected program list."""
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
            flush()
            current = ProgramVerification(name=begin.group(1), loaded=True)
            continue
        failed = _FAILED_RE.match(line)
        if failed:
            if current is None or current.name != failed.group(1):
                flush()
                current = ProgramVerification(name=failed.group(1), loaded=False)
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


def bench_command(binary: str, mode: str, iterations: int, churn_rate_hz: int, churn_duration_ms: int) -> str:
    return f"{binary} {mode} {iterations} {churn_rate_hz} {churn_duration_ms}"


# --------------------------------------------------------------------------
# Errors.
# --------------------------------------------------------------------------


class EbpfVerifyError(RuntimeError):
    pass


# --------------------------------------------------------------------------
# VM I/O: thin wrappers around `ssh`/`scp`.
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


# --------------------------------------------------------------------------
# Orchestration.
# --------------------------------------------------------------------------


def stage_and_build(vm: Vm, work_dir: str) -> str:
    """Stages the current BPF source (HEAD plus any uncommitted diff -- for
    this one self-contained source file that is exactly the live working
    tree) into `work_dir` and compiles it there with the host-equivalent
    clang flags. Returns the remote object path."""
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


def run_bench(vm: Vm, work_dir: str, iterations: int, churn_rate_hz: int, churn_duration_ms: int, samples: int):
    """Best-effort: builds the `process_event_bench` example in `work_dir`
    (needs the full workspace plus the prepared offset-generator asset) and
    runs `samples` interleaved `ebpf`/`cn-proc` samples. Returns raw JSON
    text per (mode, sample); a build failure raises so the caller can decide
    whether to treat it as fatal."""
    results: dict[str, list[str]] = {"ebpf": [], "cn-proc": []}
    binary = f"{work_dir}/target/release/examples/process_event_bench"
    rc, out, err = vm.run(
        f"cd {work_dir} && cargo build --release -p vosu --example process_event_bench "
        f"--features linux-process-events 2>&1",
        timeout=20 * 60,
    )
    if rc != 0:
        raise EbpfVerifyError(f"guest release build of process_event_bench failed: {out}")
    rc, _, _ = vm.run(
        f"sudo setcap 'cap_bpf,cap_perfmon,cap_sys_resource+ep' {binary}"
    )
    if rc != 0:
        raise EbpfVerifyError(f"setcap on {binary} failed")
    for sample in range(samples):
        for mode in ("ebpf", "cn-proc"):
            cmd = bench_command(binary, mode, iterations, churn_rate_hz, churn_duration_ms)
            rc, out, err = vm.run(cmd, timeout=120)
            if rc != 0:
                raise EbpfVerifyError(f"{mode} sample {sample} failed: {out}\n{err}")
            results[mode].append(out)
    return results


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--budget", type=int, default=DEFAULT_BUDGET)
    parser.add_argument("--bench", type=int, default=0, help="run N interleaved ebpf/cn-proc samples")
    parser.add_argument(
        "--iterations", type=int, default=16, help="process_event_bench iteration count for --bench"
    )
    parser.add_argument("--churn-rate-hz", type=int, default=50)
    parser.add_argument("--churn-duration-ms", type=int, default=10_000)
    parser.add_argument("--host", default=DEFAULT_HOST)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--ssh-user", default=DEFAULT_SSH_USER)
    args = parser.parse_args(argv)

    vm = Vm(args.host, args.port, args.ssh_user)
    token = secrets.token_hex(4)
    work_dir = remote_work_dir(token)
    pin_dir = remote_pin_dir(work_dir, token)

    try:
        obj_path = stage_and_build(vm, work_dir)
        rc, programs, err = run_verification(vm, obj_path, pin_dir)
        print(format_table(programs, args.budget))
        budget_result = evaluate_budget(programs, args.budget)
        if not budget_result.ok:
            print("\nFAILED:")
            for violation in budget_result.violations:
                print(f"  - {violation}")

        bench_ok = True
        if args.bench:
            try:
                bench_results = run_bench(
                    vm,
                    work_dir,
                    args.iterations,
                    args.churn_rate_hz,
                    args.churn_duration_ms,
                    args.bench,
                )
                print("\nbench samples:")
                for mode, samples in bench_results.items():
                    print(f"  {mode}: {len(samples)} samples collected")
            except EbpfVerifyError as error:
                bench_ok = False
                print(f"\nbench FAILED: {error}")

        return 0 if (budget_result.ok and bench_ok) else 1
    finally:
        vm.run(f"sudo rm -rf {pin_dir}; rm -rf {work_dir}")


if __name__ == "__main__":
    sys.exit(main())
