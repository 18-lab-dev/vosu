#!/usr/bin/env python3
"""Hunt native-Windows flakes: build a filtered test binary once in the
Windows VM, then rerun it N times in a row with full parallelism, recording
every failing test name and iteration.

Usage:
    python scripts/vm-stress.py [--package P] [--lib] [--test T]
                                 [--iterations N] [--jobs N]
                                 [--build-timeout SECS] [--iter-timeout SECS]
                                 -- <test filter>

This is scripts/vm-test.py's sibling for reliability work, not a substitute
for it: vm-test.py answers "does this pass on Windows right now?"; this
answers "does this still pass after N reruns?", the shape a flake report
needs. It stages exactly HEAD plus the agent's uncommitted working tree into
a unique directory under V:\\bt (same approach as vm-test.py), builds once
with `cargo test --no-run`, then reruns `cargo test` with the same args plus
the given filter N times -- nothing changes between reruns, so Cargo's own
fingerprint check is a fast no-op and each iteration goes straight to
executing the already-built binary with its default (full) parallelism, the
same as a single `cargo test`/`cargo nextest run` invocation would use.

Never starts the VM; refuses to run if it is not already running.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import secrets
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_ITERATIONS = 30
DEFAULT_JOBS = 8
DEFAULT_BUILD_TIMEOUT_SECONDS = 30 * 60
DEFAULT_ITER_TIMEOUT_SECONDS = 3 * 60
CLIENT_TIMEOUT_GRACE_SECONDS = 180
POLL_INTERVAL_SECONDS = 3


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# Reuse vm-test.py's pure helpers (cargo arg building, git bundling/patching,
# the `windows-vm` wrappers) instead of duplicating them.
vm_test = _load("vm_test", "vm-test.py")


def build_cargo_args_build(package, test, lib):
    """`cargo test ... --no-run`: compiles the filtered test binary without
    running it."""
    return vm_test.build_cargo_test_args(package, test, lib, []) + ["--no-run"]


def build_cargo_args_iter(package, test, lib, test_filter, extra):
    """`cargo test ... -- <filter> [extra]`: reruns the already-built binary."""
    return vm_test.build_cargo_test_args(package, test, lib, [test_filter, *extra])


def build_invocation_config(
    remote_dir, commit, cargo_args_build, cargo_args_iter, has_patch, iterations, jobs,
    build_timeout_seconds, iter_timeout_seconds,
):
    return {
        "WorkRoot": remote_dir,
        "BundlePath": f"{remote_dir}\\vosu.bundle",
        "PatchPath": f"{remote_dir}\\uncommitted.patch" if has_patch else "",
        "ExpectedCommit": commit,
        "CargoArgsBuild": cargo_args_build,
        "CargoArgsIter": cargo_args_iter,
        "Iterations": iterations,
        "Jobs": jobs,
        "BuildTimeoutSeconds": build_timeout_seconds,
        "IterTimeoutSeconds": iter_timeout_seconds,
    }


def remote_work_root(short_sha, token):
    return f"V:\\bt\\vms-{short_sha}-{token}"


def task_name(short_sha, token):
    return f"vmstress-{short_sha}-{token}"


def build_run_command(remote_dir):
    return (
        f"powershell.exe -NoProfile -ExecutionPolicy Bypass "
        f"-File {remote_dir}\\vm-stress.ps1 -ConfigPath {remote_dir}\\invocation.json"
    )


def build_progress_script(remote_dir):
    """PowerShell script text reporting progress.json (if present) and
    whether done.json exists yet, as one compact JSON line."""
    progress_path = f"{remote_dir}\\progress.json"
    done_path = f"{remote_dir}\\done.json"
    return f"""$ErrorActionPreference = 'Stop'
$result = [ordered]@{{}}
if (Test-Path '{progress_path}') {{
    $result.progress = Get-Content '{progress_path}' -Raw | ConvertFrom-Json
}} else {{
    $result.progress = $null
}}
$result.done = Test-Path '{done_path}'
if ($result.done) {{
    $result.doneData = Get-Content '{done_path}' -Raw | ConvertFrom-Json
}}
$result | ConvertTo-Json -Compress -Depth 6
"""


def main():
    args = parse_args(sys.argv[1:])
    try:
        return run(args)
    except vm_test.VmTestError as error:
        print(f"vm-stress.py: {error}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("vm-stress.py: interrupted; cleaning up", file=sys.stderr)
        return 130


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--package", help="cargo test --package <P> instead of --workspace")
    parser.add_argument("--test", help="cargo test --test <T>, selecting one integration test binary")
    parser.add_argument("--lib", action="store_true", help="cargo test --lib, selecting only the library target")
    parser.add_argument(
        "--iterations", type=int, default=DEFAULT_ITERATIONS,
        help=f"number of reruns after the single build (default {DEFAULT_ITERATIONS})",
    )
    parser.add_argument(
        "--jobs", type=int, default=DEFAULT_JOBS,
        help=f"CARGO_BUILD_JOBS cap in the VM for the build step (default {DEFAULT_JOBS} of 16 vCPUs)",
    )
    parser.add_argument(
        "--build-timeout", type=int, default=DEFAULT_BUILD_TIMEOUT_SECONDS,
        help=f"wall-clock timeout for the one-time build in seconds (default {DEFAULT_BUILD_TIMEOUT_SECONDS})",
    )
    parser.add_argument(
        "--iter-timeout", type=int, default=DEFAULT_ITER_TIMEOUT_SECONDS,
        help=f"wall-clock timeout per rerun in seconds (default {DEFAULT_ITER_TIMEOUT_SECONDS})",
    )
    parser.add_argument(
        "--keep-remote", action="store_true",
        help="never delete the staging directory/logs in V:\\bt, e.g. to inspect a flake's full output afterward",
    )
    parser.add_argument(
        "extra", nargs="+",
        help="the test filter (required, first positional after `--`), plus any extra test-binary args",
    )
    return parser.parse_args(argv)


def run(args):
    vm_test.require_vm_running()

    test_filter, extra = args.extra[0], args.extra[1:]

    sha = vm_test.head_sha()
    short_sha = sha[:12]
    token = secrets.token_hex(4)
    remote_dir = remote_work_root(short_sha, token)
    task = task_name(short_sha, token)

    cargo_args_build = build_cargo_args_build(args.package, args.test, args.lib)
    cargo_args_iter = build_cargo_args_iter(args.package, args.test, args.lib, test_filter, extra)

    total_timeout = args.build_timeout + args.iterations * args.iter_timeout

    print(f"Staging {sha} (+ uncommitted changes) into {remote_dir} as task {task}")
    print(f"build:  cargo {' '.join(cargo_args_build)}")
    print(f"rerun:  cargo {' '.join(cargo_args_iter)}  x{args.iterations}")

    cleaned_up = False

    def cleanup():
        nonlocal cleaned_up
        if cleaned_up:
            return
        cleaned_up = True
        if args.keep_remote:
            print(f"vm-stress.py: --keep-remote: leaving {remote_dir} and task {task} in place")
            return
        try:
            vm_test.vm_exec_script(vm_test.build_cleanup_script(task, remote_dir), timeout=60)
        except Exception as error:  # best-effort: never mask the real result
            print(f"warning: cleanup of {remote_dir} / task {task} failed: {error}", file=sys.stderr)

    try:
        with tempfile.TemporaryDirectory() as local_tmp:
            local_tmp = Path(local_tmp)
            bundle_path = local_tmp / "vosu.bundle"
            patch_path = local_tmp / "uncommitted.patch"

            vm_test.create_bundle(bundle_path, sha)
            has_patch = vm_test.create_uncommitted_patch(patch_path)

            config = build_invocation_config(
                remote_dir, sha, cargo_args_build, cargo_args_iter, has_patch,
                args.iterations, args.jobs, args.build_timeout, args.iter_timeout,
            )
            config_path = local_tmp / "invocation.json"
            config_path.write_text(json.dumps(config), encoding="utf-8")

            rc, out, err = vm_test.vm_exec_script(
                f'New-Item -ItemType Directory -Force -Path "{remote_dir}" | Out-Null\n'
            )
            if rc != 0:
                raise vm_test.VmTestError(f"could not create {remote_dir}: {err or out}")

            vm_test.vm_put(bundle_path, f"{remote_dir}/vosu.bundle")
            if has_patch:
                vm_test.vm_put(patch_path, f"{remote_dir}/uncommitted.patch")
            vm_test.vm_put(config_path, f"{remote_dir}/invocation.json")
            vm_test.vm_put(ROOT / "scripts" / "vm-stress.ps1", f"{remote_dir}/vm-stress.ps1")
            vm_test.vm_put(ROOT / "scripts" / "windows-ci-env.ps1", f"{remote_dir}/windows-ci-env.ps1")

            run_command = build_run_command(remote_dir)
            rc, out, err = vm_test.vm_exec_script(vm_test.build_create_and_run_script(task, run_command))
            if rc != 0:
                raise vm_test.VmTestError(f"could not schedule {task}: {err or out}")

            started = time.monotonic()
            deadline = started + total_timeout + CLIENT_TIMEOUT_GRACE_SECONDS
            last_reported = None
            done = None
            while True:
                rc, out, err = vm_test.vm_exec_script(build_progress_script(remote_dir), timeout=60)
                if rc == 0 and out.strip():
                    try:
                        status = json.loads(vm_test.strip_ps_noise(out))
                    except json.JSONDecodeError:
                        status = None
                    if status is not None:
                        progress = status.get("progress")
                        if progress and progress != last_reported:
                            print(
                                f"  {progress['completed']}/{progress['total']} iterations done, "
                                f"{progress['failingSoFar']} failing so far"
                            )
                            last_reported = progress
                        if status.get("done"):
                            done = status.get("doneData")
                            break
                if time.monotonic() >= deadline:
                    print(
                        f"vm-stress.py: no result from {task} after {total_timeout + CLIENT_TIMEOUT_GRACE_SECONDS}s; "
                        "giving up and cleaning up",
                        file=sys.stderr,
                    )
                    return 124
                time.sleep(POLL_INTERVAL_SECONDS)

            wall = time.monotonic() - started
            exit_code = 1
            if done is not None:
                exit_code = done.get("exitCode", 1)
                error_message = done.get("error")
                if error_message:
                    print(f"vm-stress.py: remote error: {error_message}", file=sys.stderr)

            try:
                results = fetch_results(remote_dir)
            except Exception as error:
                print(f"vm-stress.py: could not parse results.json: {error}", file=sys.stderr)
                results = None
            if results is not None:
                print_results(results)

            print(f"\nvm-stress.py: exit {exit_code}, wall {wall:.1f}s, commit {sha}")
            return exit_code
    finally:
        cleanup()


def fetch_results(remote_dir):
    with tempfile.TemporaryDirectory() as local_tmp:
        local_path = Path(local_tmp) / "results.json"
        completed = subprocess.run(
            ["windows-vm", "get", f"{remote_dir}\\results.json", str(local_path)],
            capture_output=True, text=True, timeout=60,
        )
        if completed.returncode != 0:
            print(f"vm-stress.py: could not fetch results.json: {completed.stderr or completed.stdout}", file=sys.stderr)
            return None
        return json.loads(local_path.read_text(encoding="utf-8-sig"))


def print_results(results):
    print(f"\n{results['failingIterations']}/{results['iterations']} iterations had a failure:")
    for name, info in results.get("perTest", {}).items():
        iterations = ", ".join(str(i) for i in info["iterations"])
        print(f"  FLAKY {name}: failed {info['failureCount']}x (iterations {iterations})")
    for crash in results.get("crashedIterations", []):
        print(
            f"  CRASH iteration {crash['iteration']}: exit {crash['exitCode']} "
            f"timedOut={crash['timedOut']} -- {crash['stdoutLog']}"
        )


if __name__ == "__main__":
    sys.exit(main())
