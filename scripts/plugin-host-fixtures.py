#!/usr/bin/env python3
"""Build and stage the M5 plugin-host fixture libraries before the root native
test (`docs/testing/verification.md`, "M5 test and fixture conventions").

Usage:
    python scripts/plugin-host-fixtures.py [--target TRIPLE] [--release-daemon] [--env-file PATH] [-- COMMAND...]

Builds the fixture examples with `--message-format=json-render-diagnostics`
and takes each library path from its `compiler-artifact` record, never from a
guessed file name or hash suffix. Each library is then copied to
`<artifact dir>/plugin-host-fixtures/<file name>`, so a later Cargo build of
the same example with other features (the workspace `--all-features
--examples` test build) cannot replace the bytes a test runs. The copy's
SHA-256 is printed as an `ARTIFACT` receipt.

The staged paths are exported as VOSU_PLUGIN_HOST_TESTKIT_LIBRARY,
VOSU_PLUGIN_HOST_OSU_LIBRARY, VOSU_M7_API_LIBRARY and VOSU_M7_TARGET.

`--release-daemon` also builds the real prerelease release daemon the
`approval` module runs (catalog-v2.md A5.5): the packaged feature set
(`--release --no-default-features`, plus `linux-process-events` on Linux),
never `test-harness`, after preparing the offset-generator bundle a release
build embeds (`scripts/prepare-offset-generator.py`, which needs the .NET
SDK). Its executable is staged the same way and exported as
VOSU_PLUGIN_APPROVAL_DAEMON.

- with `-- COMMAND...`, the command runs with every variable set and its
  exit code is returned;
- with `--env-file PATH`, `NAME=VALUE` lines are appended to PATH (the
  `$GITHUB_ENV` format, which the local gates read too);
- otherwise shell `export` lines are printed.

The tests never run Cargo themselves; they fail when a library variable is
unset.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# (environment variable, cargo package, example target, extra cargo arguments)
FIXTURES = (
    ("VOSU_PLUGIN_HOST_TESTKIT_LIBRARY", "vosu-plugin-testkit", "testkit_process_plugin", ()),
    ("VOSU_PLUGIN_HOST_OSU_LIBRARY", "vosu-osu", "osu_process_plugin", ("--features", "test-harness")),
    ("VOSU_M7_API_LIBRARY", "vosu-plugin-testkit", "api_v1_plugin", ("--features", "test-harness")),
)
TARGET = ("VOSU_M7_TARGET", "vosu-plugin-testkit", "api_v1_target", ())
LIBRARY_SUFFIXES = (".so", ".dll", ".dylib")
STAGE_DIR = "plugin-host-fixtures"


class FixtureError(RuntimeError):
    pass


def build_command(package, example, extra, target=None):
    """The exact `cargo build` argv for one fixture."""
    command = ["cargo", "build", "--locked", "-p", package, "--example", example, *extra]
    if target:
        command += ["--target", target]
    command.append("--message-format=json-render-diagnostics")
    return command


def select_library(stdout, example):
    """The one dynamic library Cargo reported for `example`'s cdylib.

    Reads `compiler-artifact` records only; a missing record, a target that
    is not a cdylib, or anything but exactly one library file is an error.
    """
    found = []
    for line in stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        record = json.loads(line)
        if record.get("reason") != "compiler-artifact":
            continue
        target = record.get("target", {})
        if target.get("name") != example or "example" not in target.get("kind", []):
            continue
        if "cdylib" not in target.get("crate_types", []):
            raise FixtureError(f"{example} is not built as a cdylib: {target.get('crate_types')}")
        found.extend(name for name in record.get("filenames", []) if name.endswith(LIBRARY_SUFFIXES))
    if len(found) != 1:
        raise FixtureError(f"expected exactly one library for {example}, Cargo reported {found}")
    return Path(found[0])


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def stage(library):
    """Copies `library` (or the release daemon) into its directory's fixture
    stage, permission bits included, returning the copy."""
    staged = library.parent / STAGE_DIR / library.name
    staged.parent.mkdir(parents=True, exist_ok=True)
    temporary = staged.with_name(staged.name + ".tmp")
    shutil.copy(library, temporary)
    os.replace(temporary, staged)
    return staged


def env_lines(paths):
    return [f"{name}={path}" for name, path in paths.items()]


def prime_image(path):
    """Windows only: loads and frees `path` once, returning the load time in
    milliseconds, or None elsewhere.

    Windows evaluates every never-seen image hash on its first load (the VM
    runs Smart App Control in evaluation mode with user-mode code integrity):
    a fresh 0.8 MB DLL took 0.5-1.1 s to load even in a Defender-excluded
    directory, while a same-hash copy at another path took about 12 ms. Each
    test loads its own copy of these libraries inside the plugin host's
    5-second startup deadline, so the one-time per-build evaluation happens
    here instead. Neither fixture runs code at load without a control file
    beside it, and none exists beside the staged copy."""
    if os.name != "nt":
        return None
    import ctypes
    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.LoadLibraryExW.restype = wintypes.HMODULE
    kernel32.LoadLibraryExW.argtypes = (wintypes.LPCWSTR, wintypes.HANDLE, wintypes.DWORD)
    kernel32.FreeLibrary.argtypes = (wintypes.HMODULE,)
    started = time.monotonic()
    handle = kernel32.LoadLibraryExW(str(path), None, 0)
    elapsed = (time.monotonic() - started) * 1000
    if not handle:
        raise FixtureError(f"loading {path} failed with error {ctypes.get_last_error()}")
    kernel32.FreeLibrary(handle)
    return elapsed


def prepare(target=None, cargo="cargo"):
    """Builds and stages every fixture; returns {variable: staged path}."""
    paths = {}
    for variable, package, example, extra in (*FIXTURES, TARGET):
        command = build_command(package, example, extra, target)
        command[0] = cargo
        print(f"$ {shlex.join(command)}", file=sys.stderr, flush=True)
        completed = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, text=True, check=False)
        if completed.returncode != 0:
            raise FixtureError(f"building {example} failed with exit {completed.returncode}")
        is_target = variable == TARGET[0]
        artifact = (select_executable(completed.stdout, example, kind="example")
                    if is_target else select_library(completed.stdout, example))
        staged = stage(artifact)
        print(f"ARTIFACT name={example} sha256={sha256(staged)} path={staged}", file=sys.stderr, flush=True)
        primed = None if is_target else prime_image(staged)
        if primed is not None:
            print(f"PRIMED name={example} first_load_ms={primed:.0f}", file=sys.stderr, flush=True)
        paths[variable] = staged
    return paths


DAEMON_VARIABLE = "VOSU_PLUGIN_APPROVAL_DAEMON"


def windows_target(target=None):
    return "windows" in target if target else os.name == "nt"


def daemon_build_command(target=None):
    """The exact `cargo build` argv for the release daemon: the packaged
    release feature set (.github/workflows/release.yml), no test-harness."""
    command = ["cargo", "build", "--locked", "--release", "-p", "vosu", "--bin", "vosu", "--no-default-features"]
    if not windows_target(target):
        command += ["--features", "linux-process-events"]
    if target:
        command += ["--target", target]
    command.append("--message-format=json-render-diagnostics")
    return command


def select_executable(stdout, name, kind="bin"):
    """The one executable Cargo reported for binary target `name`."""
    found = []
    for line in stdout.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        record = json.loads(line)
        if record.get("reason") != "compiler-artifact":
            continue
        target = record.get("target", {})
        if target.get("name") != name or kind not in target.get("kind", []):
            continue
        if record.get("executable"):
            found.append(record["executable"])
    if len(found) != 1:
        raise FixtureError(f"expected exactly one executable for {name}, Cargo reported {found}")
    return Path(found[0])


def prepare_daemon(target=None, cargo="cargo"):
    """Builds and stages the release daemon; returns {variable: staged path}."""
    rid = "win-x64" if windows_target(target) else "linux-x64"
    offsets = [sys.executable, str(ROOT / "scripts/prepare-offset-generator.py"), "--rid", rid]
    print(f"$ {shlex.join(offsets)}", file=sys.stderr, flush=True)
    if subprocess.run(offsets, cwd=ROOT, stdout=sys.stderr, check=False).returncode != 0:
        raise FixtureError("preparing the offset-generator bundle failed")
    command = daemon_build_command(target)
    command[0] = cargo
    print(f"$ {shlex.join(command)}", file=sys.stderr, flush=True)
    completed = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, text=True, check=False)
    if completed.returncode != 0:
        raise FixtureError(f"building the release daemon failed with exit {completed.returncode}")
    staged = stage(select_executable(completed.stdout, "vosu"))
    print(f"ARTIFACT name=vosu-release sha256={sha256(staged)} path={staged}", file=sys.stderr, flush=True)
    # One run outside any test, so Windows evaluates the new image hash
    # here and not inside a test's daemon startup.
    started = time.monotonic()
    version = subprocess.run([str(staged), "--version"], stdout=subprocess.PIPE, text=True, check=False, timeout=120)
    if version.returncode != 0:
        raise FixtureError(f"{staged} --version failed with exit {version.returncode}")
    elapsed = (time.monotonic() - started) * 1000
    print(f"PRIMED name=vosu-release version={version.stdout.strip()!r} first_run_ms={elapsed:.0f}",
          file=sys.stderr, flush=True)
    return {DAEMON_VARIABLE: staged}


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--target", help="cargo --target triple, matching the test build's own --target")
    parser.add_argument("--release-daemon", action="store_true",
                        help=f"also build and stage the release daemon as {DAEMON_VARIABLE}")
    parser.add_argument("--env-file", help="append NAME=VALUE lines to this file (the $GITHUB_ENV format)")
    parser.add_argument("command", nargs="*", help="command to run with every variable set (after `--`)")
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(sys.argv[1:] if argv is None else argv)
    cargo = os.environ.get("CARGO", "cargo")
    try:
        paths = prepare(args.target, cargo)
        if args.release_daemon:
            paths.update(prepare_daemon(args.target, cargo))
    except FixtureError as error:
        print(f"plugin-host-fixtures.py: {error}", file=sys.stderr)
        return 2
    if args.env_file:
        with open(args.env_file, "a", encoding="utf-8") as handle:
            handle.writelines(line + "\n" for line in env_lines(paths))
    if args.command:
        env = {**os.environ, **{name: str(path) for name, path in paths.items()}}
        return subprocess.run(args.command, cwd=ROOT, env=env, check=False).returncode
    if not args.env_file:
        for name, path in paths.items():
            print(f"export {name}={shlex.quote(str(path))}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
