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
def _stable_file_version(path: Path) -> str:
    if sys.platform != "win32":
        raise ValueError("stable client identity requires native Windows PE version APIs")
    import ctypes
    from ctypes import wintypes
    version = ctypes.WinDLL("version", use_last_error=True)
    size = version.GetFileVersionInfoSizeW(str(path), None)
    if not size:
        raise ValueError("stable client executable has no PE file version")
    data = ctypes.create_string_buffer(size)
    if not version.GetFileVersionInfoW(str(path), 0, size, data):
        raise ValueError("cannot read stable client PE file version")
    pointer, length = ctypes.c_void_p(), wintypes.UINT()
    if not version.VerQueryValueW(data, "\\\\", ctypes.byref(pointer), ctypes.byref(length)):
        raise ValueError("stable client PE version root is missing")
    values = ctypes.cast(pointer, ctypes.POINTER(wintypes.DWORD * 13)).contents
    ms, ls = values[2], values[3]
    return f"{ms >> 16}.{ms & 0xffff}.{ls >> 16}.{ls & 0xffff}"


def _derive_client_version(
    client: Path, kind: str
) -> tuple[str, dict[str, str] | None]:
    if kind == "lazer":
        if client.name.lower() not in {"osu!", "osu!.exe", "osulazer.exe"}:
            raise ValueError("configured client is not a supported lazer executable")
        deps = client.with_name("osu!.deps.json").resolve(strict=True)
        deps_bytes = deps.read_bytes()
        libraries = json.loads(deps_bytes).get("libraries", {})
        versions = sorted(
            key.split("/", 1)[1].split("-", 1)[0]
            for key in libraries if key.startswith("osu!/"))
        if len(versions) != 1 or not versions[0]:
            raise ValueError("lazer deps file must identify exactly one osu! version")
        return versions[0], {
            "path": str(deps),
            "sha256": hashlib.sha256(deps_bytes).hexdigest(),
        }
    if client.name.lower() != "osu!.exe" \
            or client.with_name("osu!.deps.json").exists():
        raise ValueError("stable client must be osu!.exe without lazer deps metadata")
    return _stable_file_version(client), None


def _git_command(repository: Path, *arguments: str) -> list[str]:
    return [
        str(_resolved_trusted_command("git")),
        "-c", "core.hooksPath=/dev/null",
        "-c", "core.fsmonitor=false",
        "-c", "core.filemode=false",
        "-c", f"safe.directory={repository.resolve()}",
        "-C", str(repository),
        *arguments,
    ]

def _git_environment() -> dict[str, str]:
    environment = _scrubbed_environment()
    environment["GIT_CONFIG_NOSYSTEM"] = "1"
    environment["GIT_CONFIG_GLOBAL"] = os.devnull
    return environment




def _validate_source_repository(repository: Path) -> str:
    git = str(_resolved_trusted_command("git"))
    environment = _git_environment()
    hook_config = subprocess.run(
        [git, "-C", str(repository), "config", "--show-origin", "--get-all",
         "core.hooksPath"],
        check=False, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=environment)
    filter_config = subprocess.run(
        [git, "-C", str(repository), "config", "--show-origin", "--get-regexp",
         r"^filter\."],
        check=False, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=environment)
    attributes_config = subprocess.run(
        [git, "-C", str(repository), "config", "--show-origin", "--get-all",
         "core.attributesFile"],
        check=False, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=environment)
    if hook_config.returncode not in (0, 1):
        raise ValueError("cannot inspect source Git hook configuration")
    if hook_config.stdout.strip():
        raise ValueError("source repository configures core.hooksPath")
    if filter_config.returncode not in (0, 1) \
            or attributes_config.returncode not in (0, 1):
        raise ValueError("cannot inspect source Git filter configuration")
    if filter_config.stdout.strip() or attributes_config.stdout.strip():
        raise ValueError("source repository configures Git filters/attributes")

    def git_directory(name: str) -> Path:
        value = subprocess.run(
            _git_command(repository, "rev-parse", name), check=True, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=environment).stdout.strip()
        path = Path(value)
        return (path if path.is_absolute() else repository / path).resolve()

    git_dir = git_directory("--git-dir")
    common_dir = git_directory("--git-common-dir")
    info_attributes = [
        directory / "info/attributes" for directory in {git_dir, common_dir}
        if (directory / "info/attributes").exists()
    ]
    if info_attributes:
        raise ValueError("source repository contains Git info attributes")
    lock_names = {"index.lock", "HEAD.lock", "config.lock", "packed-refs.lock",
                  "shallow.lock"}
    locks = [
        path for directory in {git_dir, common_dir}
        for path in directory.iterdir()
        if path.name in lock_names
    ]
    if locks:
        raise ValueError(
            "source repository contains Git lock files: "
            + ", ".join(sorted(path.name for path in locks)))
    hooks = common_dir / "hooks"
    active_hooks = [] if not hooks.exists() else [
        path for path in hooks.iterdir()
        if path.is_file() and not path.name.endswith(".sample")
    ]
    if active_hooks:
        raise ValueError(
            "source repository contains active Git hooks: "
            + ", ".join(sorted(path.name for path in active_hooks)))

    revision = subprocess.run(
        _git_command(repository, "rev-parse", "HEAD"), check=True, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=environment).stdout.strip()
    status = subprocess.run(
        _git_command(repository, "status", "--porcelain=v1",
                     "--untracked-files=all"),
        check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=environment).stdout
    if status:
        raise ValueError("source repository has tracked or untracked changes")
    pinned_lock = subprocess.run(
        _git_command(repository, "show",
                     f"{PINNED_SOURCE_REVISION}:pnpm-lock.yaml"),
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=environment).stdout
    working_lock = (repository / "pnpm-lock.yaml").read_bytes()
    if working_lock != pinned_lock:
        raise ValueError("source root pnpm-lock.yaml differs from pinned revision")
    return revision


def _verify_pinned_worktree(repository: Path, revision: str) -> None:
    environment = _git_environment()
    symlink_files = False
    if sys.platform == "win32":
        policy = subprocess.run(
            _git_command(repository, "config", "--bool", "--get", "core.symlinks"),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment)
        if policy.returncode not in (0, 1):
            policy.check_returncode()
        symlink_files = policy.returncode == 0 and policy.stdout.strip() == b"false"
    records = subprocess.run(
        _git_command(repository, "ls-tree", "-rz", "--full-tree", revision),
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        env=environment).stdout
    for raw in records.split(b"\0"):
        if not raw:
            continue
        metadata, separator, raw_path = raw.partition(b"\t")
        fields = metadata.split()
        if not separator or len(fields) != 3:
            raise ValueError("pinned source tree contains malformed entry")
        mode, kind, object_id = (field.decode("ascii") for field in fields)
        relative = Path(raw_path.decode(errors="surrogateescape"))
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("pinned source tree contains unsafe path")
        path = repository / relative
        if kind != "blob" or mode not in {"100644", "100755", "120000"}:
            raise ValueError("pinned source tree contains unsupported entry")
        blob = subprocess.run(
            _git_command(repository, "cat-file", "blob", object_id),
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=environment).stdout
        if mode == "120000":
            if path.is_symlink():
                target = os.fsencode(os.readlink(path))
            elif symlink_files and path.is_file():
                # Git for Windows materializes the target blob without requiring
                # symlink privileges when the checkout sets core.symlinks=false.
                target = path.read_bytes()
            else:
                target = None
            if target != blob:
                raise ValueError(
                    f"pinned source symlink differs from Git blob: {relative}")
        elif path.is_symlink() or not path.is_file() or path.read_bytes() != blob:
            raise ValueError(
                f"pinned source file differs from Git blob: {relative}")
        if mode != "120000" and os.name == "posix" and (
                bool(path.stat().st_mode & 0o111) != (mode == "100755")):
            raise ValueError(
                f"pinned source executable mode differs from Git tree: {relative}")


def validate_artifacts(cfg: dict[str, Any]) -> dict[str, Any]:
    root = Path(__file__).resolve().parents[2]
    source_mode = cfg["source_mode"]
    source_spec, vosu_spec = cfg["daemons"]["source"], cfg["daemons"]["vosu"]

    cargo = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    if cargo.get("package", {}).get("version") != PINNED_VOSU_VERSION:
        raise ValueError(
            f"vosu package metadata version is not {PINNED_VOSU_VERSION}")

    if source_mode == "node-build":
        source_repository = Path(cfg["source_directory"]).resolve(strict=True)
        revision = _validate_source_repository(source_repository)
        if revision != PINNED_SOURCE_REVISION:
            raise ValueError(
                f"source checkout is at {revision}, not {PINNED_SOURCE_REVISION}")
        pinned_package = subprocess.run(
            _git_command(
                source_repository, "show",
                f"{PINNED_SOURCE_REVISION}:packages/tosu/package.json"),
            check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=_git_environment()).stdout
        source_package = json.loads(pinned_package)
        if source_package.get("version") != PINNED_SOURCE_VERSION:
            raise ValueError(
                f"pinned source package version is not {PINNED_SOURCE_VERSION}")
        canonical_node = _resolved_trusted_command("node")
        source_entrypoint = (
            source_repository / "packages/tosu/dist/index.js").resolve()
        if (_resolved_command(source_spec["command"][0]) != canonical_node
                or len(source_spec["command"]) != 2
                or Path(source_spec["command"][1]).resolve() != source_entrypoint):
            raise ValueError(
                "source daemon command must be exactly canonical Node plus the "
                "pinned upstream tosu packages/tosu/dist/index.js entrypoint")
        source_record = {
            "mode": source_mode,
            "repository": str(source_repository),
            "revision": revision,
            "package_version": source_package["version"],
            "executable": str(canonical_node.resolve(strict=True)),
            "executable_sha256": sha256_file(canonical_node),
        }
    else:
        reference = cfg["source_reference"]
        configured_executable = reference["executable"]
        if not Path(configured_executable).is_absolute():
            raise ValueError(
                "source_reference.executable must be an absolute canonical path")
        executable = Path(configured_executable).resolve(strict=True)
        if configured_executable != str(executable):
            raise ValueError(
                "source_reference.executable must be an absolute canonical path")
        executable_hash = sha256_file(executable)
        if executable_hash != reference["sha256"]:
            raise ValueError(
                "native source reference digest does not match source_reference.sha256")
        if reference["version"] != PINNED_SOURCE_VERSION:
            raise ValueError(
                f"native source reference version is not {PINNED_SOURCE_VERSION}")
        with executable.open("rb") as source:
            magic = source.read(4)
        if ((sys.platform == "win32" and magic[:2] != b"MZ")
                or (sys.platform.startswith("linux") and magic != b"\x7fELF")
                or not file_contains(
                    executable, reference["version"].encode("ascii"))):
            raise ValueError(
                "native source reference is not the declared native release binary")
        expected_command = [str(executable)]
        if source_spec["command"] != expected_command:
            raise ValueError(
                "source daemon command must be exactly the native reference executable")
        source_record = {
            "mode": source_mode,
            "package_version": reference["version"],
            "version": reference["version"],
            "executable": str(executable),
            "executable_sha256": executable_hash,
            "sha256": executable_hash,
            "command": expected_command,
        }

    if len(vosu_spec["command"]) != 1:
        raise ValueError(
            "vosu daemon command must contain only the vosu executable placeholder")

    client = Path(cfg["client_executable"]).resolve(strict=True)
    client_hash = sha256_file(client)
    if client_hash != cfg["client_expected_sha256"]:
        raise ValueError(
            "client executable digest does not match client_expected_sha256")
    client_version, deps_record = _derive_client_version(client, cfg["client"])
    if client_version != cfg["client_expected_version"]:
        raise ValueError(
            "client build version does not match client_expected_version")
    client_record = {
        "executable": str(client), "sha256": client_hash, "kind": cfg["client"],
        "version": client_version,
        "build_identity": f"{cfg['client']}:{client_version}:{client_hash}",
    }
    if deps_record is not None:
        client_record["version_source"] = deps_record
    return {
        "source": source_record,
        "vosu": {"package_version": cargo["package"]["version"]},
        "client": client_record,
    }
def _controlled_tool_path(
        required_tools: tuple[str, ...] = ("git", "node", "pnpm", "cargo", "rustc")
) -> str:
    toolchain = {
        tool: _resolved_trusted_command(tool)
        for tool in required_tools
    }
    tool_dirs = list(dict.fromkeys(
        str(path.parent) for path in toolchain.values()))
    tool_dirs.extend(
        directory for directory in os.defpath.split(os.pathsep)
        if directory and directory not in tool_dirs)
    controlled_path = os.pathsep.join(tool_dirs)
    for tool, expected in toolchain.items():
        selected = shutil.which(tool, path=controlled_path)
        if selected is None or Path(selected).resolve() != expected.resolve():
            raise ValueError(
                f"controlled PATH shadows attested {tool} executable")
    return controlled_path


def _controlled_build_environment(
        home: Path, extra: dict[str, Any] | None = None,
        required_tools: tuple[str, ...] = ("git", "node", "pnpm", "cargo", "rustc")
) -> dict[str, str]:
    if home.exists():
        shutil.rmtree(home)
    home.mkdir(parents=True)
    controlled_path = _controlled_tool_path(required_tools)
    values: dict[str, Any] = {
        "HOME": home, "USERPROFILE": home, "XDG_CONFIG_HOME": home / ".config",
        "PATH": controlled_path,
    }
    values.update(extra or {})
    return _scrubbed_environment(values, trusted_internal=True)


def _command_version(
        command: list[str], environment: dict[str, str], *,
        cwd: Path | None = None) -> str:
    return subprocess.run(
        command, check=True, text=True, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, env=environment, cwd=cwd).stdout.strip()


def _tree_manifest(root: Path) -> list[dict[str, Any]]:
    resolved_root = root.resolve(strict=True)
    manifest: list[dict[str, Any]] = []
    for path in sorted(resolved_root.rglob("*")):
        if path.is_symlink():
            raise ValueError(f"source dist contains unsafe symlink: {path}")
        if not path.is_file():
            continue
        if resolved_root not in path.resolve().parents:
            raise ValueError(f"source dist contains unsafe file: {path}")
        manifest.append({
            "path": path.relative_to(resolved_root).as_posix(),
            "size": path.stat().st_size,
            "sha256": sha256_file(path),
        })
    if not manifest:
        raise ValueError("source dist is empty")
    return manifest
def _fresh_build_roots(capture_path: str) -> tuple[Path, Path]:
    capture_root = Path(capture_path).resolve()
    if sys.platform == "win32":
        identity = hashlib.sha256(
            os.fsencode(str(capture_root))).hexdigest()[:8]
        local_data = os.environ.get("LOCALAPPDATA")
        if not local_data:
            raise ValueError("LOCALAPPDATA is required for Windows builds")
        build_root = Path(local_data) / "tp" / identity
        source_dir = build_root / "s"
        vosu_target = build_root / "r"
    else:
        source_dir = Path(str(capture_root) + ".source-worktree")
        vosu_target = Path(str(capture_root) + ".vosu-target")
    for directory in (source_dir, vosu_target):
        if directory.exists():
            shutil.rmtree(directory)
    return source_dir, vosu_target
