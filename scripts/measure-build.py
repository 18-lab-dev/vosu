#!/usr/bin/env python3
"""Measure the M0 build-time goal
(docs/v2/implementation-plan.md section 8 "Build-time goal").

For each named probe file: append a comment line, time `cargo check --locked`,
`cargo build --locked`, and `cargo test --locked -p <crate> --lib --no-run`,
revert the file, repeat for `--runs` (default 5), and report the median and
min-max range per command. Builds are pinned off the cores the
vosu-windows-pro VM uses (`taskset --cpu-list`), so a concurrent native
Windows run does not skew timing; `--cpu-list ""` disables pinning.

Probes (section 8 "Measurement"): the ADR's own `src/api/fixed.rs` (moves to
`vosu-adapter-support` after M4a), the largest osu! crate's reader
(`vosu-osu-reader`), the state reducer (`vosu-osu-state`), the core router
(`src/server/routes.rs`, stays in the root package until M1-M7 per the
section 3 mapping), and the dashboard renderer (`src/runtime/render.rs`,
stays in the root package until M7). `vosu-plugin-api` does not exist before
M1, so that probe is skipped.
"""

import argparse
import json
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parent.parent

# (probe name, file relative to ROOT, owning crate package name)
PROBES = [
    ("api-fixed", "src/api/fixed.rs", "vosu"),
    ("osu-reader-lazer", "crates/vosu-osu-reader/src/memory/lazer/reader.rs", "vosu-osu-reader"),
    ("osu-state-reducer", "crates/vosu-osu-state/src/bundle/reducer.rs", "vosu-osu-state"),
    ("server-routes", "src/server/routes.rs", "vosu"),
    ("dashboard-render", "src/runtime/render.rs", "vosu"),
]

# vosu-windows-pro's QEMU worker threads are confined to host CPUs 8-15,24-31
# (the non-V-cache CCD); measure on the other CCD so a concurrent native run
# does not steal cycles from the timed process.
DEFAULT_CPU_LIST = "0-7,16-23"
DEFAULT_RUNS = 5
MARKER = "// measure-build.py probe touch\n"


def taskset_prefix(cpu_list):
    return ["taskset", "--cpu-list", cpu_list] if cpu_list else []


def timed(cmd, cpu_list, env):
    full_cmd = taskset_prefix(cpu_list) + cmd
    start = time.perf_counter()
    result = subprocess.run(full_cmd, cwd=ROOT, env=env, capture_output=True, text=True)
    elapsed = time.perf_counter() - start
    if result.returncode != 0:
        raise RuntimeError(
            f"{' '.join(full_cmd)} failed ({result.returncode}):\n{result.stdout}\n{result.stderr}"
        )
    return elapsed


def median_range(samples):
    return statistics.median(samples), min(samples), max(samples)


def measure_probe(name, relative_path, crate, cpu_list, runs, env):
    path = ROOT / relative_path
    original = path.read_text(encoding="utf-8")
    test_cmd = ["cargo", "test", "--locked", "-p", crate, "--lib", "--no-run"]
    samples = {"cargo check --locked": [], "cargo build --locked": [], " ".join(test_cmd): []}
    try:
        for _ in range(runs):
            path.write_text(original + MARKER, encoding="utf-8")
            samples["cargo check --locked"].append(timed(["cargo", "check", "--locked"], cpu_list, env))
            samples["cargo build --locked"].append(timed(["cargo", "build", "--locked"], cpu_list, env))
            samples[" ".join(test_cmd)].append(timed(test_cmd, cpu_list, env))
            path.write_text(original, encoding="utf-8")
    finally:
        path.write_text(original, encoding="utf-8")
    return {
        "probe": name,
        "path": relative_path,
        "crate": crate,
        "commands": {command: median_range(values) for command, values in samples.items()},
    }


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--cpu-list",
        default=DEFAULT_CPU_LIST,
        help="taskset --cpu-list value; pass '' to disable pinning",
    )
    parser.add_argument("--runs", type=int, default=DEFAULT_RUNS)
    parser.add_argument(
        "--probe",
        action="append",
        dest="probes",
        help="only measure this probe name (repeatable); default: all",
    )
    parser.add_argument("--json", action="store_true", help="print machine-readable JSON")
    args = parser.parse_args()

    cpu_list = args.cpu_list
    if cpu_list and shutil.which("taskset") is None:
        print("taskset not found on PATH; running unpinned", file=sys.stderr)
        cpu_list = ""

    probes = PROBES
    if args.probes:
        wanted = set(args.probes)
        probes = [probe for probe in PROBES if probe[0] in wanted]
        missing = wanted - {probe[0] for probe in probes}
        if missing:
            print(f"Unknown probe name(s): {', '.join(sorted(missing))}", file=sys.stderr)
            return 1

    available_probes = []
    for probe in probes:
        name, relative_path, _crate = probe
        if (ROOT / relative_path).exists():
            available_probes.append(probe)
        else:
            print(f"Skipping {name}: {relative_path} does not exist in this tree", file=sys.stderr)

    # One untimed warm build so every probe starts from a warm target/.
    print("Warming target/ with a full workspace build...", file=sys.stderr)
    subprocess.run(
        ["cargo", "build", "--locked", "--workspace", "--all-features"],
        cwd=ROOT,
        check=True,
        capture_output=True,
    )

    results = []
    for name, relative_path, crate in available_probes:
        print(f"Measuring {name} ({relative_path}, {args.runs} runs)...", file=sys.stderr)
        results.append(measure_probe(name, relative_path, crate, cpu_list, args.runs, None))

    if args.json:
        print(json.dumps(results, indent=2))
        return 0

    for result in results:
        print(f"\n{result['probe']} ({result['path']}, crate {result['crate']}):")
        for command, (median, low, high) in result["commands"].items():
            print(f"  {command}: median {median:.3f}s, range {low:.3f}-{high:.3f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
