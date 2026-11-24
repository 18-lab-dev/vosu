#!/usr/bin/env python3
"""Compare already-built Criterion binaries on an exclusively reserved Linux core."""

import argparse
from contextlib import contextmanager
import json
import math
import os
from pathlib import Path
import random
import statistics
import subprocess
import sys
import uuid


CGROUP = Path("/sys/fs/cgroup")
TOPOLOGY = Path("/sys/devices/system/cpu")


def cpu_set(text):
    result = set()
    for item in text.strip().split(","):
        if not item:
            continue
        ends = item.split("-")
        start, end = int(ends[0]), int(ends[-1])
        if len(ends) > 2 or start < 0 or end < start:
            raise ValueError(f"invalid CPU list: {text}")
        result.update(range(start, end + 1))
    return result


def physical_core(cpu, topology=TOPOLOGY):
    siblings = cpu_set((topology / f"cpu{cpu}/topology/thread_siblings_list").read_text())
    if cpu not in siblings:
        raise ValueError(f"CPU {cpu} absent from its sibling list")
    return siblings


@contextmanager
def reserve_core(cpu, root=CGROUP, topology=TOPOLOGY):
    """An isolated cpuset partition removes this core from other cgroups."""
    siblings = physical_core(cpu, topology)
    if "cpuset" not in (root / "cgroup.subtree_control").read_text().split():
        raise ValueError("root cpuset controller must already be enabled by the administrator")
    if not siblings <= cpu_set((root / "cpuset.cpus.effective").read_text()):
        raise ValueError("the full physical core is not available in the root cpuset")
    group = root / f"vosu-bench-{uuid.uuid4().hex}"
    group.mkdir()
    try:
        (group / "cpuset.mems").write_text((root / "cpuset.mems.effective").read_text())
        cpus = ",".join(map(str, sorted(siblings)))
        (group / "cpuset.cpus").write_text(cpus)
        (group / "cpuset.cpus.exclusive").write_text(cpus)
        (group / "cpuset.cpus.partition").write_text("isolated")
        state = (group / "cpuset.cpus.partition").read_text().strip()
        effective = cpu_set((group / "cpuset.cpus.effective").read_text())
        if state != "isolated" or effective != siblings:
            raise ValueError(f"exclusive core reservation failed: {state}, CPUs {effective}")
        yield group, siblings
    finally:
        # subprocess.run reaps the benchmark before this context exits, including
        # on timeout. Removing the empty partition returns its CPUs to the host.
        group.rmdir()


def child_setup(group, cpu, identity):
    def setup():
        (group / "cgroup.procs").write_text(str(os.getpid()))
        os.sched_setaffinity(0, {cpu})
        if identity is not None:
            uid, gid = identity
            os.setgroups([])
            os.setgid(gid)
            os.setuid(uid)
    return setup


def load_medians(home):
    values = {}
    for path in sorted(home.glob("**/new/estimates.json")):
        value = json.loads(path.read_text())["median"]["point_estimate"]
        if not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0:
            raise ValueError(f"invalid median in {path}")
        values[path.parent.parent.relative_to(home).as_posix()] = value
    if not values:
        raise ValueError(f"no Criterion estimates under {home}; filter matched no benchmarks?")
    return values


def summarize(values):
    rng = random.Random(0)
    samples = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(10000))
    return {
        "median_ns": statistics.median(values),
        "bootstrap_95_ci_ns": [samples[249], samples[9749]],
        "round_medians_ns": values,
        "spread_ns": max(values) - min(values),
    }


def report(rounds):
    labels = list(rounds)
    expected = set(rounds[labels[0]][0])
    for label in labels:
        for values in rounds[label]:
            if set(values) != expected:
                raise ValueError(f"benchmark IDs differ for {label}; compare identical fixtures/filters")
    result = {}
    for benchmark in sorted(expected):
        revisions = {
            label: summarize([values[benchmark] for values in rounds[label]])
            for label in labels
        }
        baseline = revisions[labels[0]]
        comparisons = {}
        for label in labels[1:]:
            candidate = revisions[label]
            delta = candidate["median_ns"] - baseline["median_ns"]
            noise = max(candidate["spread_ns"], baseline["spread_ns"])
            a, b = baseline["bootstrap_95_ci_ns"], candidate["bootstrap_95_ci_ns"]
            overlap = max(a[0], b[0]) <= min(a[1], b[1])
            comparisons[label] = {
                "delta_percent": 100 * delta / baseline["median_ns"],
                "inconclusive": noise >= abs(delta) or overlap,
                "spread_exceeds_delta": noise > abs(delta),
            }
        result[benchmark] = {"revisions": revisions, "versus_first": comparisons}
    return result


def run_rounds(binaries, args, group, identity):
    rounds = {label: [] for label, _ in binaries}
    for index in range(args.rounds):
        # A,B,...,A,B,...: never run every baseline round before candidates.
        for label, binary in binaries:
            home = args.output / f"round-{index + 1}" / label
            home.parent.mkdir(exist_ok=True)
            if identity is not None:
                os.chown(home.parent, *identity)
            home.mkdir()
            if identity is not None:
                os.chown(home, *identity)
            env = dict(os.environ, CRITERION_HOME=str(home))
            command = [str(binary), "--bench", args.filter,
                       "--warm-up-time", str(args.warmup),
                       "--measurement-time", str(args.measurement),
                       "--sample-size", str(args.samples), "--noplot"]
            print(f"Round {index + 1}/{args.rounds}: {label}", flush=True)
            with (home / "run.log").open("w") as log:
                if identity is not None:
                    os.chown(home / "run.log", *identity)
                subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT,
                               check=True, timeout=args.timeout,
                               preexec_fn=child_setup(group, args.cpu, identity))
            rounds[label].append(load_medians(home))
    return rounds


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="REVISION=PATH")
    parser.add_argument("--filter", required=True, help="Criterion benchmark regex")
    parser.add_argument("--cpu", type=int, required=True, help="logical CPU; all its siblings are reserved")
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--warmup", type=float, default=3)
    parser.add_argument("--measurement", type=float, default=10)
    parser.add_argument("--samples", type=int, default=100)
    parser.add_argument("--timeout", type=float, default=600, help="seconds per binary invocation")
    parser.add_argument("--output", type=Path, required=True, help="new evidence directory")
    args = parser.parse_args()
    try:
        if args.rounds < 2 or args.samples < 10 or min(args.warmup, args.measurement, args.timeout) <= 0:
            raise ValueError("need >=2 rounds, >=10 samples, and positive timing limits")
        if not all(math.isfinite(x) for x in (args.warmup, args.measurement, args.timeout)):
            raise ValueError("timing limits must be finite")
        binaries = []
        for item in args.binary:
            label, separator, path = item.partition("=")
            if not separator or not label or label in (".", "..") or "/" in label:
                raise ValueError("--binary requires a safe REVISION=PATH label")
            binary = Path(path).resolve(strict=True)
            if not binary.is_file() or not os.access(binary, os.X_OK):
                raise ValueError(f"not an executable bench binary: {binary}")
            binaries.append((label, binary))
        if len(binaries) < 2 or len({label for label, _ in binaries}) != len(binaries):
            raise ValueError("supply at least two uniquely labelled revisions")
        if os.geteuid() != 0 or "SUDO_UID" not in os.environ or "SUDO_GID" not in os.environ:
            raise ValueError("run with sudo: exclusive cpuset reservation needs root; benches run as the invoking user")
        identity = (int(os.environ["SUDO_UID"]), int(os.environ["SUDO_GID"]))
        if identity[0] == 0:
            raise ValueError("invoke sudo from an unprivileged account; benchmarks must not run as root")
        args.output = args.output.resolve()
        args.output.mkdir(parents=True, exist_ok=False)
        os.chown(args.output, *identity)
        with reserve_core(args.cpu) as (group, siblings):
            rounds = run_rounds(binaries, args, group, identity)
        evidence = {
            "cpu": args.cpu, "reserved_core": sorted(siblings),
            "binaries": {label: str(binary) for label, binary in binaries},
            "filter": args.filter, "rounds": args.rounds,
            "results": report(rounds),
        }
        text = json.dumps(evidence, indent=2) + "\n"
        (args.output / "summary.json").write_text(text)
        os.chown(args.output / "summary.json", *identity)
        print(text, end="")
        return 0
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f"bench-compare: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())