#!/usr/bin/env python3
"""Run filtered tests natively in the Windows VM before handing off a change.

Usage:
    python scripts/vm-test.py [--package P] [--test T] [--lib] [--timeout SECS]
                               [--jobs N] [--env NAME=VALUE ...] [-- <test filter/args>]

This is a lightweight lever, not the full prepush gate: it stages exactly
HEAD plus the agent's uncommitted working tree into a unique directory under
V:\\bt, runs one filtered `cargo test` natively through windows-ci-env.ps1 /
vm-test.ps1 (the same environment setup scripts/native-ci.ps1 uses), and
tails the result back. Use it to catch Windows-only behavior (conditional
compilation, path/line-ending handling, native process/memory code) before
the much slower full `scripts/prepush.py` gate, which still must pass before
landing.

Concurrency: each invocation gets a unique staging directory and
CARGO_TARGET_DIR, and defaults CARGO_BUILD_JOBS to 8 of the VM's 16 vCPUs
(see windows-ci-env.ps1's Set-VosuCargoWorkEnv), so two of these, or one of
these alongside a prepush.py native phase, can run at the same time without
a manual pause: Cargo's package-cache locking makes a shared, read-mostly
CARGO_HOME safe to use concurrently, and the capped job count leaves CPU
headroom instead of each invocation claiming every core.

Never starts the VM; refuses to run if it is not already running.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import secrets
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_TIMEOUT_SECONDS = 20 * 60
DEFAULT_JOBS = 8
# Grace added on top of -TimeoutSeconds before the Linux side gives up
# waiting on the remote task: staging (clone/checkout/patch/toolchain
# import) happens before vm-test.ps1's own cargo-step timeout starts, so a
# tight equality would false-positive on normal staging latency.
CLIENT_TIMEOUT_GRACE_SECONDS = 120
POLL_INTERVAL_SECONDS = 2


# --------------------------------------------------------------------------
# Pure helpers: argument building, command/script construction, cleanup
# planning. Covered by scripts/test_vm_test.py without touching the VM.
# --------------------------------------------------------------------------


def build_cargo_test_args(package, test, lib, extra):
    """Build the `cargo test ...` argv run inside the VM.

    Mirrors AGENTS.md's native command shape: `--workspace` unless a
    `--package` narrows it, `--locked --all-features` always, and anything
    after `--` passed straight through as the test binary's own filter/args.
    """
    args = ["test", "--locked", "--all-features"]
    if package:
        args += ["--package", package]
    else:
        args.append("--workspace")
    if lib:
        args.append("--lib")
    if test:
        args += ["--test", test]
    if extra:
        args += ["--", *extra]
    return args


def needs_plugin_host_fixtures(package, test, lib):
    """Whether the selected cargo test can include the root plugin_host_native
    target, which reads fixture libraries prepared in the guest beforehand
    (scripts/plugin-host-fixtures.py) and never builds them itself."""
    return not lib and package in (None, "vosu") and test in (None, "plugin_host_native")


def build_invocation_config(remote_dir, commit, cargo_args, has_patch, jobs, timeout_seconds,
                            env=None, plugin_host_fixtures=False, release_daemon=False):
    """The JSON config vm-test.ps1 reads via -ConfigPath.

    schtasks' own /TR value is capped at 261 characters, far too short for
    a command line carrying a full cargo argv (a test filter can be long,
    or contain spaces/quotes); writing it to a file next to the bundle
    keeps the scheduled command itself short and argument-content-free.

    `env` holds extra process environment for the cargo step, e.g. the D15
    fixture path scripts/test-catalog-forge.py --windows-vm stages; the
    scheduled task does not inherit the SSH session's environment.
    `PluginHostFixtures` makes vm-test.ps1 build and stage the plugin-host
    fixture libraries in the guest before the test run; `ReleaseDaemon`
    also builds and stages the release daemon the `approval` module runs
    (plugin-host-fixtures.py --release-daemon).
    """
    return {
        "WorkRoot": remote_dir,
        "BundlePath": f"{remote_dir}\\vosu.bundle",
        "PatchPath": f"{remote_dir}\\uncommitted.patch" if has_patch else "",
        "ExpectedCommit": commit,
        "CargoArgs": cargo_args,
        "Jobs": jobs,
        "TimeoutSeconds": timeout_seconds,
        "Env": dict(env or {}),
        "PluginHostFixtures": plugin_host_fixtures or release_daemon,
        "ReleaseDaemon": release_daemon,
    }


def parse_env(pairs):
    """`NAME=VALUE` strings to a dict; a malformed or duplicate name is an error."""
    env = {}
    for pair in pairs:
        name, separator, value = pair.partition("=")
        if not separator or not name or name in env or not name.replace("_", "").isalnum():
            raise VmTestError(f"--env expects a unique NAME=VALUE, got {pair!r}")
        env[name] = value
    return env


def remote_work_root(short_sha, token):
    return f"V:\\bt\\vmt-{short_sha}-{token}"


def task_name(short_sha, token):
    return f"vmtest-{short_sha}-{token}"


def build_run_command(remote_dir):
    """The powershell.exe command line scheduled through schtasks /TR.

    Deliberately short and quote-free (just two space-free paths under
    $remote_dir, which never contains spaces): the run's actual parameters
    live in invocation.json (see build_invocation_config), not here.
    """
    return (
        f"powershell.exe -NoProfile -ExecutionPolicy Bypass "
        f"-File {remote_dir}\\vm-test.ps1 -ConfigPath {remote_dir}\\invocation.json"
    )


def build_create_and_run_script(task, run_command):
    """PowerShell script text (sent via `windows-vm exec`) that schedules the
    detached task and starts it immediately (/SC ONCE with a placeholder
    /ST; the real trigger is the explicit /Run, so the task starts now and
    survives the SSH session ending)."""
    assert '"' not in run_command, "run_command must stay quote-free; see build_run_command"
    return (
        f'schtasks /Create /TN "{task}" /SC ONCE /ST 23:59 /RL LIMITED /F /TR "{run_command}"\n'
        'if ($LASTEXITCODE -ne 0) { throw "schtasks /Create failed" }\n'
        f'schtasks /Run /TN "{task}"\n'
        'if ($LASTEXITCODE -ne 0) { throw "schtasks /Run failed" }\n'
    )


def build_tail_script(remote_dir, stdout_offset, stderr_offset):
    """PowerShell script text that reads new bytes from the cargo-test log
    files since the given byte offsets and reports whether done.json
    exists yet, as one compact JSON line."""
    stdout_path = f"{remote_dir}\\logs\\cargo-test.stdout.log"
    stderr_path = f"{remote_dir}\\logs\\cargo-test.stderr.log"
    done_path = f"{remote_dir}\\done.json"
    return f"""$ErrorActionPreference = 'Stop'
function Read-Tail($path, $offset) {{
    if (-not (Test-Path $path)) {{ return '' }}
    $fs = [System.IO.File]::Open($path, 'Open', 'Read', 'ReadWrite')
    try {{
        $len = $fs.Length
        if ($len -le $offset) {{ return '' }}
        $fs.Seek($offset, 'Begin') | Out-Null
        $buf = New-Object byte[] ($len - $offset)
        $null = $fs.Read($buf, 0, $buf.Length)
        [Convert]::ToBase64String($buf)
    }} finally {{ $fs.Close() }}
}}
$result = [ordered]@{{
    stdoutB64 = (Read-Tail '{stdout_path}' {stdout_offset})
    stdoutLen = if (Test-Path '{stdout_path}') {{ (Get-Item '{stdout_path}').Length }} else {{ 0 }}
    stderrB64 = (Read-Tail '{stderr_path}' {stderr_offset})
    stderrLen = if (Test-Path '{stderr_path}') {{ (Get-Item '{stderr_path}').Length }} else {{ 0 }}
    done      = (Test-Path '{done_path}')
}}
if ($result.done) {{
    $doneData = Get-Content '{done_path}' -Raw | ConvertFrom-Json
    $result.exitCode = $doneData.exitCode
    $result.timedOut = $doneData.timedOut
    $result.error = $doneData.error
}}
$result | ConvertTo-Json -Compress -Depth 4
"""


def build_cleanup_script(task, remote_dir):
    """PowerShell script text that removes the scheduled task and staging
    directory. Every statement is best-effort (errors swallowed) so this is
    safe to run even when an earlier step never got that far, on Ctrl-C, or
    on a client-side timeout."""
    return (
        f'schtasks /End /TN "{task}" 2>$null | Out-Null\n'
        f'schtasks /Delete /TN "{task}" /F 2>$null | Out-Null\n'
        f'Remove-Item -Recurse -Force "{remote_dir}" -ErrorAction SilentlyContinue\n'
    )


def plan_cleanup(task, remote_dir):
    """Ordered, inspectable description of what cleanup does, for tests that
    should not have to parse the PowerShell text."""
    return [
        ("end_task", task),
        ("delete_task", task),
        ("remove_dir", remote_dir),
    ]


# --------------------------------------------------------------------------
# VM I/O. Thin wrappers around the canonical `windows-vm` CLI.
# --------------------------------------------------------------------------


class VmTestError(RuntimeError):
    pass


def vm_status_json():
    completed = subprocess.run(
        ["windows-vm", "status", "--json"], capture_output=True, text=True, timeout=30
    )
    if completed.returncode != 0:
        raise VmTestError(f"windows-vm status failed: {completed.stderr or completed.stdout}")
    return json.loads(completed.stdout)


def require_vm_running():
    info = vm_status_json()
    if info.get("state") != "running":
        raise VmTestError(
            f"Windows VM is {info.get('state')}; vm-test.py never starts it. "
            "Start it yourself (e.g. `windows-vm start`) and retry."
        )


def vm_exec_script(script_text, *, timeout=120):
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".ps1", delete=False, encoding="utf-8"
    ) as handle:
        handle.write(script_text)
        script_path = handle.name
    try:
        completed = subprocess.run(
            ["windows-vm", "exec", "--file", script_path],
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    finally:
        os.unlink(script_path)
    return completed.returncode, completed.stdout, completed.stderr


def vm_put(local_path, remote_path):
    completed = subprocess.run(
        ["windows-vm", "put", str(local_path), remote_path],
        capture_output=True,
        text=True,
        timeout=120,
    )
    if completed.returncode != 0:
        raise VmTestError(f"could not upload {local_path} -> {remote_path}: {completed.stderr or completed.stdout}")


def strip_ps_noise(text):
    # windows-vm already strips CLIXML progress noise; this only guards
    # against the output not being pure JSON when something above throws.
    lines = [line for line in text.splitlines() if line.strip()]
    return lines[-1] if lines else ""


# --------------------------------------------------------------------------
# Git staging: HEAD bundle + an uncommitted-changes patch, without mutating
# the agent's real index.
# --------------------------------------------------------------------------


def git(*args, cwd=ROOT, check=True):
    completed = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)
    if check and completed.returncode != 0:
        raise VmTestError(f"git {' '.join(args)} failed: {completed.stderr.strip()}")
    return completed.stdout


def head_sha():
    return git("rev-parse", "HEAD").strip()


def create_bundle(dest: Path, sha: str):
    # `git bundle create` refuses a bare commit id ("Refusing to create
    # empty bundle"); it needs a named ref. `sha` is passed only to assert
    # the bundle really contains the commit we think it does.
    git("bundle", "create", str(dest), "HEAD")
    verify = subprocess.run(
        ["git", "bundle", "list-heads", str(dest)], capture_output=True, text=True
    )
    if sha not in verify.stdout:
        raise VmTestError(f"bundle {dest} does not contain expected HEAD {sha}: {verify.stdout}")


def create_uncommitted_patch(dest: Path) -> bool:
    """Write a patch of every uncommitted change (staged, unstaged, new,
    deleted files) relative to HEAD to `dest`, using a throwaway copy of the
    index so the agent's real staged/unstaged state is never touched.
    Returns True if the patch is non-empty."""
    git_dir = Path(git("rev-parse", "--git-dir").strip())
    if not git_dir.is_absolute():
        git_dir = (ROOT / git_dir).resolve()
    real_index = git_dir / "index"
    with tempfile.TemporaryDirectory() as tmp:
        shadow_index = Path(tmp) / "index"
        if real_index.exists():
            shadow_index.write_bytes(real_index.read_bytes())
        env = {**os.environ, "GIT_INDEX_FILE": str(shadow_index)}
        add = subprocess.run(["git", "add", "-A"], cwd=ROOT, env=env, capture_output=True, text=True)
        if add.returncode != 0:
            raise VmTestError(f"staging uncommitted changes into the shadow index failed: {add.stderr}")
        diff = subprocess.run(
            ["git", "diff", "--binary", "--no-color", "HEAD"],
            cwd=ROOT,
            env=env,
            capture_output=True,
            text=True,
        )
        if diff.returncode != 0:
            raise VmTestError(f"diffing the shadow index against HEAD failed: {diff.stderr}")
    dest.write_text(diff.stdout, encoding="utf-8")
    return len(diff.stdout) > 0


# --------------------------------------------------------------------------
# Orchestration.
# --------------------------------------------------------------------------


def stream_decode(b64_text):
    if not b64_text:
        return ""
    return base64.b64decode(b64_text).decode("utf-8", errors="replace")


def run(args):
    require_vm_running()

    sha = head_sha()
    short_sha = sha[:12]
    token = secrets.token_hex(4)
    remote_dir = remote_work_root(short_sha, token)
    task = task_name(short_sha, token)

    cargo_args = build_cargo_test_args(args.package, args.test, args.lib, args.extra)

    print(f"Staging {sha} (+ uncommitted changes) into {remote_dir} as task {task}")
    print(f"cargo {' '.join(cargo_args)}")

    exit_code = 1
    timed_out = False
    cleaned_up = False

    def cleanup():
        nonlocal cleaned_up
        if cleaned_up:
            return
        cleaned_up = True
        try:
            vm_exec_script(build_cleanup_script(task, remote_dir), timeout=60)
        except Exception as error:  # best-effort: never mask the real result
            print(f"warning: cleanup of {remote_dir} / task {task} failed: {error}", file=sys.stderr)

    try:
        with tempfile.TemporaryDirectory() as local_tmp:
            local_tmp = Path(local_tmp)
            bundle_path = local_tmp / "vosu.bundle"
            patch_path = local_tmp / "uncommitted.patch"

            create_bundle(bundle_path, sha)
            has_patch = create_uncommitted_patch(patch_path)

            config = build_invocation_config(
                remote_dir, sha, cargo_args, has_patch, args.jobs, args.timeout, parse_env(args.env),
                plugin_host_fixtures=needs_plugin_host_fixtures(args.package, args.test, args.lib),
                release_daemon=args.release_daemon,
            )
            config_path = local_tmp / "invocation.json"
            config_path.write_text(json.dumps(config), encoding="utf-8")

            rc, out, err = vm_exec_script(
                f'New-Item -ItemType Directory -Force -Path "{remote_dir}" | Out-Null\n'
            )
            if rc != 0:
                raise VmTestError(f"could not create {remote_dir}: {err or out}")

            vm_put(bundle_path, f"{remote_dir}/vosu.bundle")
            if has_patch:
                vm_put(patch_path, f"{remote_dir}/uncommitted.patch")
            vm_put(config_path, f"{remote_dir}/invocation.json")
            vm_put(ROOT / "scripts" / "vm-test.ps1", f"{remote_dir}/vm-test.ps1")
            vm_put(ROOT / "scripts" / "windows-ci-env.ps1", f"{remote_dir}/windows-ci-env.ps1")

            run_command = build_run_command(remote_dir)
            rc, out, err = vm_exec_script(build_create_and_run_script(task, run_command))
            if rc != 0:
                raise VmTestError(f"could not schedule {task}: {err or out}")

            started = time.monotonic()
            deadline = started + args.timeout + CLIENT_TIMEOUT_GRACE_SECONDS
            stdout_offset = 0
            stderr_offset = 0
            done = None
            while True:
                rc, out, err = vm_exec_script(
                    build_tail_script(remote_dir, stdout_offset, stderr_offset), timeout=60
                )
                if rc == 0 and out.strip():
                    try:
                        status = json.loads(strip_ps_noise(out))
                    except json.JSONDecodeError:
                        status = None
                    if status is not None:
                        new_stdout = stream_decode(status.get("stdoutB64", ""))
                        new_stderr = stream_decode(status.get("stderrB64", ""))
                        if new_stdout:
                            sys.stdout.write(new_stdout)
                            sys.stdout.flush()
                        if new_stderr:
                            sys.stderr.write(new_stderr)
                            sys.stderr.flush()
                        stdout_offset = status.get("stdoutLen", stdout_offset)
                        stderr_offset = status.get("stderrLen", stderr_offset)
                        if status.get("done"):
                            done = status
                            break
                if time.monotonic() >= deadline:
                    print(
                        f"vm-test.py: no result from {task} after "
                        f"{args.timeout + CLIENT_TIMEOUT_GRACE_SECONDS}s; giving up and cleaning up",
                        file=sys.stderr,
                    )
                    timed_out = True
                    exit_code = 124
                    break
                time.sleep(POLL_INTERVAL_SECONDS)

            wall = time.monotonic() - started
            if done is not None:
                exit_code = done.get("exitCode", 1)
                timed_out = bool(done.get("timedOut"))
                error_message = done.get("error")
                if error_message:
                    print(f"vm-test.py: remote error: {error_message}", file=sys.stderr)
            print(f"\nvm-test.py: exit {exit_code}, timed_out={timed_out}, wall {wall:.1f}s, commit {sha}")
            return exit_code
    finally:
        cleanup()


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--package", help="cargo test --package <P> instead of --workspace")
    parser.add_argument("--test", help="cargo test --test <T>, selecting one integration test binary")
    parser.add_argument("--lib", action="store_true", help="cargo test --lib, selecting only the library target")
    parser.add_argument(
        "--timeout",
        type=int,
        default=DEFAULT_TIMEOUT_SECONDS,
        help=f"hard wall-clock timeout in seconds for the remote test run (default {DEFAULT_TIMEOUT_SECONDS})",
    )
    parser.add_argument(
        "--jobs",
        type=int,
        default=DEFAULT_JOBS,
        help=f"CARGO_BUILD_JOBS cap in the VM (default {DEFAULT_JOBS} of 16 vCPUs)",
    )
    parser.add_argument(
        "--release-daemon",
        action="store_true",
        help="also build the release daemon (no test-harness) for the plugin_host_native approval module",
    )
    parser.add_argument(
        "--env",
        action="append",
        default=[],
        metavar="NAME=VALUE",
        help="extra environment for the remote cargo step (repeatable)",
    )
    parser.add_argument(
        "extra",
        nargs="*",
        help="passed to the test binary after `--`, e.g. a test name filter or --nocapture",
    )
    return parser.parse_args(argv)


def main():
    args = parse_args(sys.argv[1:])
    try:
        return run(args)
    except VmTestError as error:
        print(f"vm-test.py: {error}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("vm-test.py: interrupted; cleaning up", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())
