from __future__ import annotations

import bisect
import base64
import argparse
import hashlib
import json
import os
import math
import queue
from pathlib import Path
import shutil
import secrets
import socket
import signal
import struct
import subprocess
import sys
import tempfile
import time
import threading
import http.client
import urllib.error
import urllib.request
import tomllib
from typing import Any
from urllib.parse import urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parent))
from live_parity import (SCHEMAS, PINNED_VOSU_VERSION, PINNED_SOURCE_REVISION,
    PINNED_SOURCE_VERSION, VOSU_BUILD_LABEL, SOURCE_BUILD_LABEL, SUPPORTED_MATRIX,
    Sample, align_active, barrier_of,
    beatmap_time, compare_pair, dump_capture, load_capture, monotonic_violations,
    paused_of, state_of)
from parity_domains import (
    cadence_evidence, confirmed_retry_boundaries, gamebase_path_family,
    infer_reference_capabilities, segment_at,
)
import parity_process as _parity_process
globals().update({name: value for name, value in vars(_parity_process).items() if not name.startswith("__")})
import parity_artifacts as _parity_artifacts
globals().update({name: value for name, value in vars(_parity_artifacts).items() if not name.startswith("__")})
def _windows_msvc_environment(
        environment: dict[str, str], home: Path, cmd: Path | None
) -> tuple[dict[str, str], dict[str, str] | None]:
    if sys.platform != "win32":
        return environment, None
    if cmd is None:
        raise ValueError("protected cmd.exe path is unavailable")
    program_files = os.environ.get("ProgramFiles(x86)")
    if not program_files:
        raise ValueError("ProgramFiles(x86) is unavailable for MSVC discovery")
    vswhere = (
        Path(program_files)
        / "Microsoft Visual Studio/Installer/vswhere.exe"
    ).resolve(strict=True)
    if _windows_command_is_writable(vswhere, vswhere):
        raise ValueError("vswhere is writable by the capture user")
    discovery_environment = dict(environment)
    for name in (
            "PROGRAMDATA", "PROGRAMFILES", "PROGRAMFILES(X86)",
            "COMMONPROGRAMFILES", "COMMONPROGRAMFILES(X86)", "SYSTEMDRIVE"):
        value = os.environ.get(name)
        if value:
            discovery_environment[name] = value
    installation = subprocess.run(
        [
            str(vswhere), "-latest", "-products", "*", "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property", "installationPath",
        ],
        check=True, text=True, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, env=discovery_environment,
    ).stdout.strip()
    if not installation or "\n" in installation or "\r" in installation:
        raise ValueError("cannot resolve one protected MSVC installation")
    root = Path(installation).resolve(strict=True)
    vcvars = (root / "VC/Auxiliary/Build/vcvars64.bat").resolve(strict=True)
    if root not in vcvars.parents or _windows_command_is_writable(vcvars, vcvars):
        raise ValueError("MSVC environment script is not protected")
    script = home / "load-msvc.cmd"
    script.write_text(
        f'@call "{vcvars}" >nul\r\n@set\r\n', encoding="utf-8")
    completed = subprocess.run(
        [str(cmd), "/d", "/c", str(script)],
        check=True, text=True, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, env=discovery_environment,
    )
    compiler_values: dict[str, str] = {}
    exact = {"PATH", "INCLUDE", "LIB", "LIBPATH", "PLATFORM"}
    prefixes = (
        "VC", "VS", "WINDOWS", "UNIVERSALCRT", "UCRT", "FRAMEWORK",
    )
    for line in completed.stdout.splitlines():
        key, separator, value = line.partition("=")
        upper = key.upper()
        if separator and (
                upper in exact or upper.startswith(prefixes)):
            compiler_values[upper] = value
    required = {"PATH", "INCLUDE", "LIB", "VCINSTALLDIR", "VSINSTALLDIR"}
    if not required.issubset({key.upper() for key in compiler_values}):
        raise ValueError("MSVC environment is incomplete")
    merged = dict(discovery_environment)
    merged.update(compiler_values)
    return merged, {
        "vswhere": str(vswhere),
        "vswhere_sha256": sha256_file(vswhere),
        "installation": str(root),
        "vcvars": str(vcvars),
        "vcvars_sha256": sha256_file(vcvars),
    }


def build_release_artifacts(
        cfg: dict[str, Any], capture_path: str) -> dict[str, Any]:
    """Build vosu in a fresh root and attest the selected upstream source reference."""
    root = Path(__file__).resolve().parents[2]
    source_mode = cfg["source_mode"]
    source_tools = (
        ("git", "node", "pnpm", "python", "powershell")
        if sys.platform == "win32"
        else ("git", "node", "pnpm", "python")
    )
    rust_tools = (
        ("cargo", "rustc", "cmd")
        if sys.platform == "win32"
        else ("cargo", "rustc")
    )
    tool_names = (
        (*source_tools, *rust_tools)
        if source_mode == "node-build"
        else rust_tools
    )
    toolchain = {
        name: _resolved_trusted_command(name)
        for name in tool_names
    }
    if (toolchain["cargo"].resolve() == toolchain["rustc"].resolve()
            or any(path.resolve().name.lower().startswith("rustup")
                   for path in (toolchain["cargo"], toolchain["rustc"]))):
        raise ValueError("rustup proxy toolchains are forbidden")
    for ancestor in (root, *root.parents):
        if ((ancestor / ".cargo/config").exists()
                or (ancestor / ".cargo/config.toml").exists()):
            raise ValueError(
                f"Cargo configuration is forbidden in build ancestor: {ancestor}")

    capture_root = Path(capture_path).resolve()
    started = time.monotonic_ns()
    source_attestation: dict[str, Any]
    if source_mode == "node-build":
        source_dir, vosu_target = _fresh_build_roots(capture_path)
        repository = Path(
            cfg["_artifact_identity"]["source"]["repository"]).resolve(strict=True)
        revision = _validate_source_repository(repository)
        if revision != PINNED_SOURCE_REVISION:
            raise ValueError("source repository moved away from the pinned revision")
        git_environment = _git_environment()
        subprocess.run(
            _git_command(repository, "worktree", "prune"), check=True,
            env=git_environment)
        subprocess.run(
            _git_command(
                repository, "worktree", "add", "--detach", "--force",
                str(source_dir), PINNED_SOURCE_REVISION),
            check=True, env=git_environment)
        revision = subprocess.run(
            _git_command(source_dir, "rev-parse", "HEAD"), check=True,
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=git_environment).stdout.strip()
        if revision != PINNED_SOURCE_REVISION:
            raise ValueError("detached source worktree is not the pinned revision")
        status = subprocess.run(
            _git_command(source_dir, "status", "--porcelain=v1",
                         "--untracked-files=all"),
            check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=git_environment).stdout
        if status:
            raise ValueError("detached source worktree is not clean")
        _verify_pinned_worktree(source_dir, PINNED_SOURCE_REVISION)
        source_build_home = (
            source_dir.parent / "source-build-home"
            if sys.platform == "win32"
            else Path(str(capture_root) + ".source-build-home")
        )
        source_environment = _controlled_build_environment(
            source_build_home, {"PYTHON": str(toolchain["python"])}, tool_names)
        source_environment["HUSKY"] = "0"
        source_environment, source_msvc = _windows_msvc_environment(
            source_environment, source_build_home, toolchain["cmd"])
        entrypoint = source_dir / "packages/tosu/dist/index.js"
        dist = entrypoint.parent
        shutil.rmtree(dist, ignore_errors=True)
        pnpm = str(toolchain["pnpm"])
        source_commands = [
        # These package identifiers belong to the pinned upstream tosu checkout.
            [pnpm, "install", "--frozen-lockfile"],
            [pnpm, "--filter", "tosu", "run", "genver"],
            [pnpm, "--filter", "tosu", "run", "ts:compile"],
        ]
        source_lock = (source_dir / "pnpm-lock.yaml").read_bytes()
        for command in source_commands:
            subprocess.run(
                command, check=True, cwd=source_dir, env=source_environment)
        if (source_dir / "pnpm-lock.yaml").read_bytes() != source_lock:
            raise ValueError("pinned source build modified the root lockfile")
        post_build_status = subprocess.run(
            _git_command(source_dir, "status", "--porcelain=v1",
                         "--untracked-files=all"),
            check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=git_environment).stdout
        if post_build_status:
            raise ValueError(
                "pinned source build modified tracked or untracked source")
        if not entrypoint.is_file():
            raise ValueError(
                "pinned upstream tosu build did not produce packages/tosu/dist/index.js")
        source_manifest = _tree_manifest(dist)
        cfg["daemons"]["source"]["command"] = [
            str(toolchain["node"]), str(entrypoint)]
        cfg["_artifact_identity"]["source"].update({
            "checkout": str(source_dir),
            "entrypoint": str(entrypoint),
            "sha256": sha256_file(entrypoint),
            "dist_manifest": source_manifest,
        })
        source_attestation = {
            "source_revision": PINNED_SOURCE_REVISION,
            "source_build_home": str(source_build_home),
            "source_environment_keys": sorted(source_environment),
            "source_path": source_environment["PATH"],
            "source_commands": source_commands,
            "source_dist_manifest": source_manifest,
            "source_lock_sha256": sha256_file(source_dir / "pnpm-lock.yaml"),
            "node_version": _command_version(
                [str(toolchain["node"]), "--version"], source_environment,
                cwd=source_dir),
            "pnpm_version": _command_version(
                [str(toolchain["pnpm"]), "--version"], source_environment,
                cwd=source_dir),
            "node_executable_sha256": sha256_file(toolchain["node"]),
            "source_msvc": source_msvc,
        }
    else:
        vosu_target = Path(str(capture_root) + ".vosu-target")
        if vosu_target.exists():
            shutil.rmtree(vosu_target)
        source = cfg["_artifact_identity"]["source"]
        executable = Path(source["executable"]).resolve(strict=True)
        expected_command = [str(executable)]
        if (source.get("mode") != "native-reference"
                or source.get("command") != expected_command
                or cfg["daemons"]["source"]["command"] != expected_command
                or cfg["source_reference"].get("executable") != str(executable)
                or cfg["source_reference"].get("version") != source.get("version")
                or source.get("version") != PINNED_SOURCE_VERSION
                or sha256_file(executable) != source.get("sha256")
                or source.get("sha256") != source.get("executable_sha256")
                or cfg["source_reference"].get("sha256") != source.get("sha256")):
            raise ValueError("native source reference identity changed before build")
        source_attestation = {
            "source_reference": {
                "executable": str(executable),
                "sha256": source["sha256"],
                "version": source["version"],
                "command": expected_command,
            },
        }

    target_triple = ("x86_64-pc-windows-msvc" if sys.platform == "win32"
                     else "x86_64-unknown-linux-gnu")
    cargo = str(toolchain["cargo"])
    vosu_command = [
        cargo, "build", "--release", "--locked", "--no-default-features",
        "--target", target_triple,
    ]
    vosu_build_home = (
        vosu_target.parent / "vosu-build-home"
        if sys.platform == "win32"
        else Path(str(capture_root) + ".vosu-build-home")
    )
    vosu_environment = _controlled_build_environment(
        vosu_build_home,
        {"CARGO_TARGET_DIR": str(vosu_target),
         "CARGO_HOME": str(vosu_build_home / "cargo"),
         "RUSTC": str(toolchain["rustc"])},
        tool_names)
    vosu_environment, vosu_msvc = _windows_msvc_environment(
        vosu_environment, vosu_build_home, toolchain.get("cmd"))
    subprocess.run(vosu_command, cwd=root, check=True, env=vosu_environment)
    vosu = (
        vosu_target / target_triple / "release"
        / ("vosu.exe" if sys.platform == "win32" else "vosu")
    ).resolve(strict=True)
    vosu_hash = sha256_file(vosu)
    with vosu.open("rb") as executable:
        magic = executable.read(4)
    if ((sys.platform == "win32" and magic[:2] != b"MZ")
            or (sys.platform.startswith("linux") and magic != b"\x7fELF")
            or not file_contains(vosu, PINNED_VOSU_VERSION.encode("ascii"))):
        raise ValueError("fresh vosu build is not the pinned native release binary")

    cfg["daemons"]["vosu"]["command"] = [str(vosu)]
    cfg["_artifact_identity"]["vosu"].update({
        "executable": str(vosu),
        "sha256": vosu_hash,
        "target_triple": target_triple,
    })
    completed = time.monotonic_ns()
    attestation = {
        "started_monotonic_ns": started,
        "completed_monotonic_ns": completed,
        "source_mode": source_mode,
        "toolchain": {
            name: {"path": str(path), "sha256": sha256_file(path)}
            for name, path in toolchain.items()
        },
        **source_attestation,
        "vosu_command": vosu_command,
        "vosu_target_dir": str(vosu_target),
        "vosu_build_home": str(vosu_build_home),
        "cargo_home": vosu_environment["CARGO_HOME"],
        "vosu_environment_keys": sorted(vosu_environment),
        "vosu_path": vosu_environment["PATH"],
        "rustc_path": vosu_environment["RUSTC"],
        "vosu_executable_sha256": vosu_hash,
        "vosu_msvc": vosu_msvc,
        "rustc_version": _command_version(
            [str(toolchain["rustc"]), "--version", "--verbose"],
            vosu_environment),
        "cargo_version": _command_version(
            [cargo, "--version", "--verbose"], vosu_environment),
        "cargo_lock_sha256": sha256_file(root / "Cargo.lock"),
        "cargo_toml_sha256": sha256_file(root / "Cargo.toml"),
    }
    cfg["_build_attestation"] = attestation
    return attestation
