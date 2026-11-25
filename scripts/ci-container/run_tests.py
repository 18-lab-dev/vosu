#!/usr/bin/env python3
"""Run the Linux nextest suite inside a locally built container that mirrors
ci.yml's `ubuntu-latest` validation runner, so a failure specific to that
distribution's packages is caught before it reaches hosted CI.

scripts/prepush.py calls `run()` for its "test-linux" step whenever podman is
on PATH, and falls back to a plain host `cargo nextest run` otherwise (noting
that fallback in the recorded evidence). Run this file directly to iterate
without going through prepush.py; the plugin-host native tests need the
prepared fixture libraries, so run it under the preparation helper:

    source scripts/dev-env.sh
    export CARGO_TARGET_DIR=/path/to/a/target/dir
    python scripts/plugin-host-fixtures.py -- python scripts/ci-container/run_tests.py
"""

import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent.parent
CONTAINER_DIR = Path(__file__).resolve().parent
CONTAINERFILE = CONTAINER_DIR / "Containerfile"

# Identical to ci.yml's validation-job nextest command; keep both literally
# in sync (scripts/test_ci_container.py checks it).
NEXTEST_CMD = [
    "cargo", "nextest", "run", "--workspace", "--lib", "--bins", "--tests",
    "--examples", "--all-features", "--locked",
]
ARCHIVE_CMD = [
    "cargo", "nextest", "archive", "--workspace", "--lib", "--bins", "--tests",
    "--examples", "--all-features", "--locked", "--archive-file",
]
# The prepared plugin-host fixture libraries (scripts/plugin-host-fixtures.py).
# They live under the target dir, which the container mounts at the same path,
# so the host paths are passed through unchanged.
FIXTURE_VARIABLES = ("VOSU_PLUGIN_HOST_TESTKIT_LIBRARY", "VOSU_PLUGIN_HOST_OSU_LIBRARY")


def podman_path():
    return shutil.which("podman")


def image_tag():
    """Tag the built image with the Containerfile's own content hash, so a
    change to it (including the pinned Ubuntu release) forces a rebuild and
    an unchanged file reuses the cached image."""
    digest = hashlib.sha256(CONTAINERFILE.read_bytes()).hexdigest()[:12]
    return f"localhost/vosu-ci-linux:{digest}"


def image_exists(podman, tag):
    return subprocess.run([podman, "image", "exists", tag], check=False).returncode == 0


def ensure_image(podman, tag):
    """Build the image if its tag is not already cached. Returns the wall time
    spent building (0.0 on a cache hit) and the build log text."""
    if image_exists(podman, tag):
        return 0.0, ""
    started = time.monotonic()
    result = subprocess.run(
        [podman, "build", "-t", tag, "-f", str(CONTAINERFILE), str(CONTAINER_DIR)],
        capture_output=True, text=True, check=False,
    )
    wall = time.monotonic() - started
    if result.returncode != 0:
        raise RuntimeError(f"podman build failed:\n{result.stdout}\n{result.stderr}")
    return wall, result.stdout + result.stderr


def run(root=ROOT, cargo_target_dir=None, cargo_nextest_bin=None, extra_env=None):
    """Build a nextest archive on the host, then replay it as a non-root user
    inside the container, with the source and target dir bind-mounted at
    their own host paths (read-only) so compile-time-baked paths like
    `env!("CARGO_MANIFEST_DIR")`/`CARGO_BIN_EXE_*` still resolve; no
    `--workspace-remap` is needed because the in-container path is identical
    to the one the binaries were built with.

    Returns a dict with `exit_code`, `stdout`, `stderr`, `wall_seconds`, and
    the `image_build_seconds`/`archive_seconds`/`container_seconds`
    breakdown. Raises RuntimeError for a setup failure (podman/cargo-nextest
    missing, image build failure); a test failure instead comes back as a
    nonzero `exit_code` like any other step.

    `extra_env` is added to the host environment; every FIXTURE_VARIABLES
    entry the result holds is also passed into the container."""
    podman = podman_path()
    if podman is None:
        raise RuntimeError("podman not found on PATH")
    cargo_nextest_bin = cargo_nextest_bin or shutil.which("cargo-nextest")
    if not cargo_nextest_bin:
        raise RuntimeError("cargo-nextest not found on PATH")
    cargo_target_dir = Path(cargo_target_dir or os.environ.get("CARGO_TARGET_DIR", "/tmp/vosu-target-prepush")).resolve()
    root = Path(root).resolve()

    tag = image_tag()
    build_wall, _build_log = ensure_image(podman, tag)

    env = dict(os.environ)
    env["CARGO_INCREMENTAL"] = "0"
    env["CARGO_TARGET_DIR"] = str(cargo_target_dir)
    env.update(extra_env or {})

    with tempfile.TemporaryDirectory(prefix="vosu-prepush-src-") as src_dir, \
            tempfile.TemporaryDirectory(prefix="vosu-prepush-archive-") as archive_dir:
        subprocess.run(f"git archive HEAD | tar -x -C {src_dir}", shell=True, cwd=root, check=True)

        archive_file = Path(archive_dir) / "nextest-archive.tar.zst"
        started = time.monotonic()
        archive_result = subprocess.run(
            [*ARCHIVE_CMD, str(archive_file)], cwd=root, env=env, capture_output=True, text=True, check=False,
        )
        archive_wall = time.monotonic() - started
        if archive_result.returncode != 0:
            return {
                "exit_code": archive_result.returncode,
                "stdout": archive_result.stdout,
                "stderr": archive_result.stderr,
                "wall_seconds": build_wall + archive_wall,
                "image_build_seconds": build_wall,
                "archive_seconds": archive_wall,
                "container_seconds": 0.0,
            }

        podman_cmd = [
            podman, "run", "--rm",
            "--userns=keep-id",
            "-v", f"{src_dir}:{root}:ro",
            "-v", f"{cargo_target_dir}:{cargo_target_dir}:ro",
            "-v", f"{archive_file}:/archive.tar.zst:ro",
            "-v", f"{cargo_nextest_bin}:/usr/local/bin/cargo-nextest:ro",
            *[flag for name in FIXTURE_VARIABLES if name in env for flag in ("-e", f"{name}={env[name]}")],
            tag,
            "cargo-nextest", "nextest", "run", "--archive-file", "/archive.tar.zst",
        ]
        started = time.monotonic()
        run_result = subprocess.run(podman_cmd, cwd=root, capture_output=True, text=True, check=False)
        run_wall = time.monotonic() - started

    return {
        "exit_code": run_result.returncode,
        "stdout": run_result.stdout,
        "stderr": run_result.stderr,
        "wall_seconds": build_wall + archive_wall + run_wall,
        "image_build_seconds": build_wall,
        "archive_seconds": archive_wall,
        "container_seconds": run_wall,
    }


def main():
    try:
        outcome = run()
    except RuntimeError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    print(outcome["stdout"])
    print(outcome["stderr"], file=sys.stderr)
    print(
        f"image build {outcome['image_build_seconds']:.1f}s, archive {outcome['archive_seconds']:.1f}s, "
        f"container run {outcome['container_seconds']:.1f}s, total {outcome['wall_seconds']:.1f}s",
        file=sys.stderr,
    )
    return outcome["exit_code"]


if __name__ == "__main__":
    sys.exit(main())
