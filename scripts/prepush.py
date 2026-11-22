#!/usr/bin/env python3
"""Run every applicable CI check locally against a clean HEAD and record evidence."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
EVIDENCE_DIR = ROOT / "target" / "prepush"
# Own, non-incremental target dir outside the checkout filesystem: a shared
# target dir has produced stale test binaries for concurrent agents before,
# and this repo's own filesystem can fail cargo's archive mmap for a target
# dir placed on it (AGENTS.md / CONTRIBUTING.md).
CARGO_ENV = {
    "CARGO_INCREMENTAL": "0",
    "CARGO_TARGET_DIR": os.environ.get("CARGO_TARGET_DIR", "/tmp/vosu-target-prepush"),
}
LINUX_ENV = {**CARGO_ENV, "C_INCLUDE_PATH": "/usr/include/x86_64-linux-gnu"}

# Native commands this file does not run directly; scripts/native-ci.ps1 runs
# them inside the VM. Keep this in sync with that script's own comment header
# and with ci.yml's windows-native job; scripts/test_gate_tooling.py checks it.
NATIVE_COMMANDS = {
    "native-clippy": "cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings",
    "native-test-workspace": "cargo test --workspace --lib --bins --tests --examples --all-features --locked --target x86_64-pc-windows-msvc",
    "native-test-doc": "cargo test --workspace --doc --all-features --locked --target x86_64-pc-windows-msvc",
}


def run(cmd, *, cwd=ROOT, env_overrides=None, extra_env_unset=()):
    import os

    env = dict(os.environ)
    for key in extra_env_unset:
        env.pop(key, None)
    if env_overrides:
        env.update(env_overrides)
    started = time.monotonic()
    completed = subprocess.run(
        cmd, cwd=cwd, env=env, capture_output=True, text=True, check=False
    )
    wall = time.monotonic() - started
    return completed.returncode, completed.stdout, completed.stderr, wall


def git(*args, cwd=ROOT, check=True):
    completed = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    if check and completed.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {completed.stderr.strip()}")
    return completed.stdout.strip()


def clean_ps_output(text):
    """Strip Windows PowerShell CLIXML progress-stream noise from stdout/stderr."""
    lines = []
    for line in text.splitlines():
        if line.startswith("**") or "CLIXML" in line or line.startswith("<Objs") or line.startswith("</Objs>"):
            continue
        lines.append(line)
    return "\n".join(lines)


def vm(*args):
    completed = subprocess.run(
        ["windows-vm", *args], capture_output=True, text=True, check=False
    )
    out = clean_ps_output(completed.stdout)
    err = clean_ps_output(completed.stderr)
    return completed.returncode, out, err


class StepResult:
    def __init__(self, name, required, status, exit_code=None, wall_seconds=None, command="", log_tail=""):
        self.name = name
        self.required = required
        self.status = status  # passed | failed | skipped | not_applicable
        self.exit_code = exit_code
        self.wall_seconds = wall_seconds
        self.command = command
        self.log_tail = log_tail[-4000:] if log_tail else ""

    def to_json(self):
        return {
            "name": self.name,
            "required": self.required,
            "status": self.status,
            "exit_code": self.exit_code,
            "wall_seconds": self.wall_seconds,
            "command": self.command,
            "log_tail": self.log_tail,
        }


def run_linux_step(name, cmd, log_dir, env_overrides=None, extra_env_unset=()):
    exit_code, stdout, stderr, wall = run(cmd, env_overrides=env_overrides, extra_env_unset=extra_env_unset)
    log_path = log_dir / f"{name}.log"
    log_path.write_text(f"$ {' '.join(cmd)}\n\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}\n", encoding="utf-8")
    status = "passed" if exit_code == 0 else "failed"
    return StepResult(name, True, status, exit_code, round(wall, 3), " ".join(cmd), stdout + stderr)


def changed_paths(base="main"):
    """Paths changed on HEAD relative to origin/<base>, or None when that cannot be proven."""
    try:
        git("fetch", "origin", base, check=True)
    except RuntimeError as error:
        print(f"warning: could not fetch origin/{base} to classify changes: {error}", file=sys.stderr)
        return None
    diff = git("diff", "--no-renames", "--name-only", f"origin/{base}...HEAD", check=False)
    return [line for line in diff.splitlines() if line]


def workflows_changed(paths):
    # Unknown changes fail safe: run actionlint rather than skip it.
    return paths is None or any(path.startswith(".github/workflows/") for path in paths)


def ui_changed(paths):
    if paths is None:
        return True
    spec = importlib.util.spec_from_file_location("ci_changes", ROOT / "scripts" / "ci-changes.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.classify(paths)[2]


def run_actionlint(log_dir, applicable):
    name = "actionlint"
    if not applicable:
        return StepResult(name, False, "not_applicable", command="go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 .github/workflows/*.yml")
    workflow_files = sorted(str(p) for p in (ROOT / ".github" / "workflows").glob("*.yml"))
    cmd = ["go", "run", "github.com/rhysd/actionlint/cmd/actionlint@v1.7.12", *workflow_files]
    exit_code, stdout, stderr, wall = run(cmd)
    log_path = log_dir / f"{name}.log"
    log_path.write_text(f"$ {' '.join(cmd)}\n\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}\n", encoding="utf-8")
    status = "passed" if exit_code == 0 else "failed"
    return StepResult(name, True, status, exit_code, round(wall, 3), " ".join(cmd), stdout + stderr)


LINUX_STEPS = [
    ("fmt", ["cargo", "fmt", "--all", "--", "--check"], CARGO_ENV, ()),
    ("clippy-linux", ["cargo", "clippy", "--workspace", "--all-targets", "--all-features", "--locked", "--", "-D", "warnings"], LINUX_ENV, ()),
    ("test-linux", ["cargo", "test", "--workspace", "--all-features", "--locked"], LINUX_ENV, ()),
    ("doc-links", [sys.executable, "scripts/check-doc-links.py"], None, ()),
    ("file-length", [sys.executable, "scripts/check-file-length.py"], None, ()),
    ("brand", [sys.executable, "scripts/check-brand.py"], None, ()),
    ("module-boundaries", [sys.executable, "scripts/check-module-boundaries.py"], None, ()),
    ("license-notices", [sys.executable, "scripts/assemble-release-license.py"], None, ()),
    ("asset-hashes", [sys.executable, "scripts/check-asset-hashes.py"], None, ()),
    ("script-tests", [sys.executable, "-m", "unittest", "discover", "-s", "scripts", "-p", "test_*.py"], None, ()),
]
XWIN_CLIPPY_CMD = ["cargo", "xwin", "clippy", "--workspace", "--all-targets", "--all-features", "--locked", "--target", "x86_64-pc-windows-msvc", "--", "-D", "warnings"]
ACTIONLINT_CMD_TEXT = "go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 .github/workflows/*.yml"
DASHBOARD_BUILD_CMD = ["cargo", "build", "--locked", "--features", "test-harness"]

# Literal commands this file runs, for scripts/test_gate_tooling.py's ci.yml
# drift check. The dictionary values use real argv tokens, not shell-globbed
# text, except actionlint's workflow glob which ci.yml also globs. Binary and
# Playwright paths differ per machine, so the drift test compares them as
# `<binary>` and `<playwright>` placeholders.
COMMAND_MANIFEST = {name: " ".join("python" if token == sys.executable else token for token in cmd) for name, cmd, _, _ in LINUX_STEPS}
COMMAND_MANIFEST["xwin-clippy"] = " ".join(XWIN_CLIPPY_CMD)
COMMAND_MANIFEST["actionlint"] = ACTIONLINT_CMD_TEXT
COMMAND_MANIFEST["dashboard-build"] = " ".join(DASHBOARD_BUILD_CMD)
COMMAND_MANIFEST["dashboard-tool-host"] = "python tests/tool_host_smoke.py --binary <binary>"
COMMAND_MANIFEST["dashboard-browser"] = "python tests/dashboard_smoke.py --binary <binary> --playwright-module <playwright>"
COMMAND_MANIFEST.update(NATIVE_COMMANDS)


def run_dashboard(log_dir, applicable, playwright_module):
    """CI's dashboard job: build a test-harness daemon, then the tool host and browser smokes."""
    name = "dashboard"
    command = "; ".join(COMMAND_MANIFEST[key] for key in ("dashboard-build", "dashboard-tool-host", "dashboard-browser"))
    if not applicable:
        return StepResult(name, False, "not_applicable", command=command)
    if not playwright_module or not Path(playwright_module).is_file():
        return StepResult(
            name, True, "failed", command=command,
            log_tail="UI paths changed but no Playwright module was found; pass --playwright-module "
            "<path to playwright/index.mjs> or set VOSU_PLAYWRIGHT_MODULE",
        )
    binary = Path(LINUX_ENV["CARGO_TARGET_DIR"]) / "debug" / "vosu"
    commands = [
        (DASHBOARD_BUILD_CMD, LINUX_ENV),
        ([sys.executable, "tests/tool_host_smoke.py", "--binary", str(binary)], None),
        ([sys.executable, "tests/dashboard_smoke.py", "--binary", str(binary), "--playwright-module", playwright_module], None),
    ]
    log = []
    total = 0.0
    for cmd, env_overrides in commands:
        exit_code, stdout, stderr, wall = run(cmd, env_overrides=env_overrides)
        total += wall
        log.append(f"$ {' '.join(cmd)}\n\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}\n")
        if exit_code != 0:
            break
    (log_dir / f"{name}.log").write_text("\n".join(log), encoding="utf-8")
    status = "passed" if exit_code == 0 else "failed"
    return StepResult(name, True, status, exit_code, round(total, 3), command, log[-1])


def run_native(sha, log_dir, evidence_dir, start_vm):
    names = ["native-clippy", "native-test-workspace", "native-test-doc"]
    sha12 = sha[:12]
    remote_stage = f"V:\\omp-prepush\\{sha12}"
    remote_work = f"V:\\bt\\vosu-{sha12}"

    def failed_all(message):
        print(f"native CI phase failed: {message}", file=sys.stderr)
        return [StepResult(n, True, "failed", log_tail=message) for n in names]

    code, out, err = vm("status")
    if code != 0:
        return failed_all(f"windows-vm status failed: {err or out}")
    running = "Windows VM: running" in out
    if not running:
        if not start_vm:
            return failed_all("Windows VM is stopped; rerun with --start-vm to start it, or run --skip native.")
        print("Starting Windows VM (was stopped) for the native CI phase.")
        code, out, err = vm("start")
        if code != 0:
            return failed_all(f"windows-vm start failed: {err or out}")

    # A freshly started guest boots for a while before SSH accepts logins, so
    # poll doctor until it authenticates rather than failing on the first try.
    deadline = time.monotonic() + (300 if not running else 0)
    while True:
        code, out, err = vm("doctor")
        if code == 0 and '"computer"' in out:
            break
        if time.monotonic() >= deadline:
            return failed_all(f"windows-vm doctor did not report an authenticated session: {out}\n{err}")
        time.sleep(10)

    bundle_path = evidence_dir / f"vosu-{sha12}.bundle"
    # `git bundle` requires a named ref, not a bare commit id; prepush.py
    # already verified the tree is clean and HEAD == sha above.
    git("bundle", "create", str(bundle_path), "HEAD", check=True)

    code, out, err = vm("exec", f"New-Item -ItemType Directory -Force -Path '{remote_stage}' | Out-Null")
    if code != 0:
        return failed_all(f"could not create remote staging dir: {err or out}")

    code, out, err = vm("put", str(bundle_path), f"{remote_stage}/vosu.bundle")
    if code != 0:
        return failed_all(f"could not upload bundle: {err or out}")
    code, out, err = vm("put", str(ROOT / "scripts" / "native-ci.ps1"), f"{remote_stage}/native-ci.ps1")
    if code != 0:
        return failed_all(f"could not upload native-ci.ps1: {err or out}")

    started = time.monotonic()
    run_cmd = (
        f"powershell.exe -NoProfile -ExecutionPolicy Bypass -File {remote_stage}\\native-ci.ps1 "
        f"-BundlePath {remote_stage}\\vosu.bundle -ExpectedCommit {sha}"
    )
    code, out, err = vm("exec", run_cmd)
    wall = time.monotonic() - started
    (log_dir / "native-run.log").write_text(f"$ {run_cmd}\n\n--- stdout ---\n{out}\n--- stderr ---\n{err}\n", encoding="utf-8")

    local_summary = evidence_dir / "native-summary.json"
    get_code, get_out, get_err = vm("get", f"{remote_work}\\summary.json", str(local_summary))
    local_logs = evidence_dir / "native-logs"
    local_logs.mkdir(exist_ok=True)
    vm("get", "-r", f"{remote_work}\\logs", str(local_logs))

    # Best-effort cleanup of the VM work directories this run created.
    vm("exec", f"Remove-Item -Recurse -Force '{remote_stage}' -ErrorAction SilentlyContinue")
    vm("exec", f"Remove-Item -Recurse -Force '{remote_work}' -ErrorAction SilentlyContinue")

    if get_code != 0 or not local_summary.exists():
        return failed_all(f"native-ci.ps1 did not produce a summary.json (exec exit {code}, {wall:.1f}s): {out}\n{err}\n{get_err}")

    summary = json.loads(local_summary.read_text(encoding="utf-8-sig"))
    by_step = {s["step"]: s for s in summary.get("steps", [])}
    mapping = {
        "native-clippy": "cargo-clippy",
        "native-test-workspace": "cargo-test-workspace",
        "native-test-doc": "cargo-test-doc",
    }
    results = []
    for name, remote_name in mapping.items():
        entry = by_step.get(remote_name)
        if entry is None:
            results.append(StepResult(name, True, "failed", log_tail=f"VM summary.json has no '{remote_name}' step; exec exit {code}"))
            continue
        status = "passed" if entry["exitCode"] == 0 else "failed"
        results.append(StepResult(name, True, status, entry["exitCode"], round(entry["wallSeconds"], 3), NATIVE_COMMANDS[name]))
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--skip",
        default="",
        help="Comma-separated step names to skip for local iteration (fmt, clippy-linux, test-linux, "
        "doc-links, file-length, brand, module-boundaries, license-notices, asset-hashes, script-tests, "
        "actionlint, xwin-clippy, dashboard, native). land.py refuses evidence that skipped a required step.",
    )
    parser.add_argument("--start-vm", action="store_true", help="Start the Windows VM if it is currently stopped.")
    parser.add_argument(
        "--base",
        default="main",
        help="Branch to diff HEAD against for change detection (default: main).",
    )
    parser.add_argument(
        "--playwright-module",
        default=os.environ.get("VOSU_PLAYWRIGHT_MODULE", ""),
        help="Path to playwright/index.mjs for the dashboard smoke (default: $VOSU_PLAYWRIGHT_MODULE).",
    )
    args = parser.parse_args()
    skip = {s.strip() for s in args.skip.split(",") if s.strip()}

    porcelain = git("status", "--porcelain", check=True)
    if porcelain:
        print("error: working tree is not clean; commit or stash before running prepush.py", file=sys.stderr)
        return 1
    sha = git("rev-parse", "HEAD", check=True)
    print(f"Gating commit {sha}")

    evidence_dir = EVIDENCE_DIR / sha
    evidence_dir.mkdir(parents=True, exist_ok=True)
    log_dir = evidence_dir / "logs"
    log_dir.mkdir(exist_ok=True)

    results = []

    def maybe_skip(name, required, command):
        if name in skip:
            return StepResult(name, required, "skipped", command=command)
        return None

    for name, cmd, env_overrides, unset in LINUX_STEPS:
        skipped = maybe_skip(name, True, " ".join(cmd))
        if skipped:
            results.append(skipped)
            continue
        print(f"running {name} ...")
        results.append(run_linux_step(name, cmd, log_dir, env_overrides, unset))

    paths = changed_paths(args.base)
    if "actionlint" in skip:
        results.append(StepResult("actionlint", True, "skipped", command=ACTIONLINT_CMD_TEXT))
    else:
        applicable = workflows_changed(paths)
        print(f"running actionlint ... (applicable={applicable})")
        results.append(run_actionlint(log_dir, applicable))

    skipped = maybe_skip("xwin-clippy", True, " ".join(XWIN_CLIPPY_CMD))
    if skipped:
        results.append(skipped)
    else:
        print("running xwin-clippy ...")
        results.append(
            run_linux_step(
                "xwin-clippy",
                XWIN_CLIPPY_CMD,
                log_dir,
                env_overrides=CARGO_ENV,
                extra_env_unset=("C_INCLUDE_PATH",),
            )
        )

    if "dashboard" in skip:
        results.append(StepResult("dashboard", True, "skipped", command="dashboard smokes"))
    else:
        applicable = ui_changed(paths)
        print(f"running dashboard smokes ... (applicable={applicable})")
        results.append(run_dashboard(log_dir, applicable, args.playwright_module))

    if "native" in skip:
        for name in ("native-clippy", "native-test-workspace", "native-test-doc"):
            results.append(StepResult(name, True, "skipped", command=NATIVE_COMMANDS[name]))
    else:
        print("running native Windows CI in the VM ...")
        results.extend(run_native(sha, log_dir, evidence_dir, args.start_vm))

    evidence = {
        "sha": sha,
        "generated_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "steps": [r.to_json() for r in results],
    }
    required_failed = [r for r in results if r.required and r.status != "passed"]
    evidence["all_required_passed"] = not required_failed
    evidence_path = EVIDENCE_DIR / f"{sha}.json"
    evidence_path.write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")

    print()
    for r in results:
        marker = "REQUIRED" if r.required else "optional"
        print(f"  {r.name:24s} {r.status:14s} {marker:9s} exit={r.exit_code} wall={r.wall_seconds}")
    print(f"\nEvidence written to {evidence_path}")

    if required_failed:
        print(f"\nFAILED: {len(required_failed)} required step(s) did not pass: {', '.join(r.name for r in required_failed)}", file=sys.stderr)
        return 1
    print("\nAll required steps passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
