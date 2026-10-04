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
import parity_capture as _parity_capture
globals().update({name: value for name, value in vars(_parity_capture).items() if not name.startswith("__")})
def _valid_source_build_commands(commands: Any, pnpm_path: str) -> bool:
    return commands == [
        [pnpm_path, "install", "--frozen-lockfile"],
        [pnpm_path, "--filter", "tosu", "run", "genver"],
        [pnpm_path, "--filter", "tosu", "run", "ts:compile"],
    ]


def _verify_msvc_attestation(value: Any) -> None:
    if sys.platform != "win32":
        raise ValueError("unexpected MSVC attestation on non-Windows")
    if not isinstance(value, dict) or set(value) != {
            "vswhere", "vswhere_sha256", "installation", "vcvars",
            "vcvars_sha256"}:
        raise ValueError("capture lacks exact MSVC attestation")
    program_files_x86 = os.environ.get("ProgramFiles(x86)")
    if not program_files_x86:
        raise ValueError("ProgramFiles(x86) is unavailable for MSVC verification")
    expected_vswhere = (
        Path(program_files_x86)
        / "Microsoft Visual Studio/Installer/vswhere.exe"
    ).resolve(strict=True)
    installation = Path(value["installation"]).resolve(strict=True)
    vswhere = Path(value["vswhere"]).resolve(strict=True)
    vcvars = Path(value["vcvars"]).resolve(strict=True)
    if (vswhere != expected_vswhere
            or vcvars != (installation / "VC/Auxiliary/Build/vcvars64.bat")
            or sha256_file(vswhere) != value["vswhere_sha256"]
            or sha256_file(vcvars) != value["vcvars_sha256"]
            or _windows_command_is_writable(vswhere, vswhere)
            or _windows_command_is_writable(vcvars, vcvars)):
        raise ValueError("capture has untrusted MSVC paths or hashes")
    discovery_environment = dict(os.environ)
    discovered = subprocess.run(
        [str(vswhere), "-latest", "-products", "*", "-requires",
         "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
         "-property", "installationPath"],
        check=True, text=True, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, env=discovery_environment,
    ).stdout.strip()
    if not discovered or Path(discovered).resolve(strict=True) != installation:
        raise ValueError("capture MSVC installation is not current vswhere result")


def _valid_windows_build_path(value: Any, msvc: Any,
                              required_tools: tuple[str, ...]) -> bool:
    if not isinstance(value, str) or not isinstance(msvc, dict):
        return False
    base = _controlled_tool_path(required_tools)
    parts = value.split(os.pathsep)
    base_parts = base.split(os.pathsep)
    # vcvars prepends compiler paths and may append CMake/Ninja after the input PATH.
    starts = [
        index for index in range(len(parts) - len(base_parts) + 1)
        if parts[index:index + len(base_parts)] == base_parts
    ]
    if len(parts) <= len(base_parts) or len(starts) != 1:
        return False
    start = starts[0]
    installation = Path(msvc["installation"]).resolve(strict=True)
    program_files_x86 = os.environ.get("ProgramFiles(x86)")
    windir = os.environ.get("WINDIR")
    roots = [installation]
    if program_files_x86:
        roots.append((Path(program_files_x86) / "Windows Kits").resolve(strict=True))
    if windir:
        roots.append(Path(windir).resolve(strict=True))
    for entry in parts[:start] + parts[start + len(base_parts):]:
        path = Path(entry).resolve(strict=True)
        if not path.is_dir() or not any(root == path or root in path.parents for root in roots):
            return False
        if _windows_command_is_writable(path, path):
            return False
    return True


def _valid_client_process_group(
        value: Any, client: dict[str, Any]) -> bool:
    if not isinstance(value, list) or not value:
        return False
    seen: set[int] = set()
    start_by_pid: dict[int, str] = {}
    for index, record in enumerate(value):
        if (
            not isinstance(record, dict)
            or not isinstance(record.get("pid"), int)
            or record["pid"] <= 0
            or record["pid"] in seen
            or not isinstance(record.get("start_identity"), str)
            or not record["start_identity"]
        ):
            return False
        parent = record.get("parent_pid")
        if index == 0 and (
            parent is not None
            or record["pid"] != client.get("pid")
            or record["start_identity"] != client.get("start_identity")
        ):
            return False
        if index > 0 and parent not in seen:
            return False
        identity_source = record.get("identity_source")
        if identity_source == "process":
            if (
                record.get("executable") != client.get("executable")
                or record.get("sha256") != client.get("sha256")
            ):
                return False
        elif identity_source == "kernel-process-snapshot":
            if (
                index == 0
                or not record["start_identity"].isdigit()
                or parent not in start_by_pid
                or not start_by_pid[parent].isdigit()
                or int(record["start_identity"]) <= int(start_by_pid[parent])
                or str(record.get("image_name", "")).casefold()
                != Path(str(client.get("executable", ""))).name.casefold()
                or record.get("executable") is not None
                or record.get("sha256") is not None
            ):
                return False
        elif identity_source == "private-job-membership":
            observed_ns = record.get("observed_monotonic_ns")
            if (
                index == 0
                or not isinstance(observed_ns, int)
                or observed_ns <= 0
                or record["start_identity"]
                != f"job:{record['pid']}:{observed_ns}"
                or record.get("executable") is not None
                or record.get("sha256") is not None
            ):
                return False
        else:
            return False
        seen.add(record["pid"])
        start_by_pid[record["pid"]] = record["start_identity"]
    return True


def verify_capture_artifacts(
        metadata: dict[str, Any], samples: list[Sample]) -> None:
    artifacts = metadata.get("artifacts")
    if not isinstance(artifacts, dict):
        raise ValueError("capture lacks derived artifact provenance")
    source = artifacts.get("source", {})
    vosu = artifacts.get("vosu", {})
    client = artifacts.get("client", {})
    source_mode = source.get("mode", "node-build")
    checkout: Path | None = None
    manifest: list[dict[str, Any]] | None = None
    if source_mode == "node-build":
        checkout = Path(source["checkout"]).resolve(strict=True)
        # Reject repository-local filters and attributes before worktree reads.
        revision = _validate_source_repository(checkout)
        if revision != PINNED_SOURCE_REVISION or source.get("revision") != revision:
            raise ValueError(
                "captured source worktree is not the clean pinned revision")
        _verify_pinned_worktree(checkout, PINNED_SOURCE_REVISION)
        entrypoint = Path(source["entrypoint"]).resolve(strict=True)
        if sha256_file(entrypoint) != source.get("sha256"):
            raise ValueError(f"captured artifact hash changed: {entrypoint}")
        manifest = _tree_manifest(entrypoint.parent)
        if manifest != source.get("dist_manifest"):
            raise ValueError("captured source dist closure changed")
    elif source_mode == "native-reference":
        executable = Path(source["executable"]).resolve(strict=True)
        expected_command = [str(executable)]
        if (source.get("executable") != str(executable)
                or source.get("command") != expected_command
                or source.get("version") != PINNED_SOURCE_VERSION
                or source.get("package_version") != PINNED_SOURCE_VERSION
                or source.get("sha256") != source.get("executable_sha256")
                or sha256_file(executable) != source.get("sha256")):
            raise ValueError("captured native source reference identity changed")
    else:
        raise ValueError("capture has invalid source mode")
    for record, field in ((vosu, "executable"), (client, "executable")):
        path = Path(record[field]).resolve(strict=True)
        if sha256_file(path) != record.get("sha256"):
            raise ValueError(f"captured artifact hash changed: {path}")
    version_source = client.get("version_source")
    if version_source:
        path = Path(version_source["path"]).resolve(strict=True)
        if sha256_file(path) != version_source.get("sha256"):
            raise ValueError("captured client version source changed")
    client_path = Path(client["executable"]).resolve(strict=True)
    client_kind = client.get("kind")
    if (not isinstance(client_kind, str)
            or (metadata.get("os"), metadata.get("client")) not in SUPPORTED_MATRIX
            or metadata.get("client") != client_kind):
        raise ValueError("capture has invalid client matrix identity")
    derived_version, derived_version_source = _derive_client_version(
        client_path, client_kind)
    expected_build_identity = (
        f"{client_kind}:{derived_version}:{client.get('sha256')}")
    if (client.get("version") != derived_version
            or client.get("version_source") != derived_version_source
            or client.get("build_identity") != expected_build_identity):
        raise ValueError("captured client version/build identity is not derived")
    if source.get("package_version") != PINNED_SOURCE_VERSION:
        raise ValueError(
            f"captured source package version is not pinned {PINNED_SOURCE_VERSION}")
    if vosu.get("package_version") != PINNED_VOSU_VERSION:
        raise ValueError(
            f"captured vosu package version is not pinned {PINNED_VOSU_VERSION}")

    ownership = metadata.get("process_ownership")
    if not isinstance(ownership, dict) or set(ownership) != {"source", "vosu"}:
        raise ValueError("capture lacks source/vosu ownership evidence")
    for name, record in ownership.items():
        artifact = source if name == "source" else vosu
        expected_hash = (artifact.get("executable_sha256")
                         if name == "source" else artifact.get("sha256"))
        checks = record.get("ownership_checks")
        environment_keys = record.get("environment_keys")
        ownership_mode = record.get("ownership_mode")
        exclusive = ownership_mode == "exclusive-port-hash"
        exclusive_allowed = (
            name == "source"
            and source_mode == "native-reference"
            and sys.platform.startswith("linux"))
        if (not isinstance(record.get("pid"), int)
                or not isinstance(record.get("start_identity"), str)
                or not isinstance(record.get("port"), int)
                or ownership_mode not in {"pid", "exclusive-port-hash"}
                or (exclusive and not exclusive_allowed)
                or not isinstance(checks, list) or len(checks) < 3
                or not _valid_environment_keys(environment_keys)
                or record.get("executable") != artifact.get("executable")
                or record.get("executable_sha256") != expected_hash):
            raise ValueError(f"capture has invalid {name} ownership evidence")
        connection_phases: set[str] = set()
        for check in checks:
            if not isinstance(check, dict):
                raise ValueError(
                    f"capture has invalid {name} socket-owner timeline")
            phase = check.get("phase")
            socket_owner = check.get("socket_owner_pid")
            owner_valid = (
                socket_owner in (None, record["pid"])
                if exclusive
                else socket_owner == record["pid"])
            if (not owner_valid
                    or check.get("port_exclusive") is not exclusive
                    or not isinstance(phase, str)
                    or not isinstance(check.get("before_monotonic_ns"), int)
                    or not isinstance(check.get("after_monotonic_ns"), int)
                    or check["after_monotonic_ns"] < check["before_monotonic_ns"]):
                raise ValueError(
                    f"capture has invalid {name} socket-owner timeline")
            if "connection_owner_pid" in check:
                connection_owner = check.get("connection_owner_pid")
                connection_owner_valid = (
                    connection_owner in (None, record["pid"])
                    if exclusive
                    else connection_owner == record["pid"])
                if (not connection_owner_valid
                        or check.get("server_port") != record["port"]
                        or not isinstance(check.get("client_port"), int)):
                    raise ValueError(
                        f"capture has invalid {name} connection-owner evidence")
                connection_phases.add(phase)
        required_connection_phases = {
            *(f"http/{schema}/connection" for schema in SCHEMAS),
            *(f"websocket/{schema}/handshake-after" for schema in SCHEMAS),
        }
        if not required_connection_phases.issubset(connection_phases):
            raise ValueError(
                f"capture lacks all-schema {name} connection-owner evidence")
        daemon_samples = [sample for sample in samples if sample.daemon == name]
        for schema in SCHEMAS:
            http_timestamps = sorted(
                sample.monotonic_ns for sample in daemon_samples
                if sample.schema == schema and sample.transport == "http")
            connection_checks = sorted(
                (check for check in checks
                 if check["phase"] == f"http/{schema}/connection"),
                key=lambda check: check["after_monotonic_ns"])
            if not _http_samples_attested(connection_checks, http_timestamps):
                raise ValueError(
                    f"capture has reused or unattested {name} HTTP samples")
            websocket_samples = sorted(
                (sample for sample in daemon_samples
                 if sample.schema == schema
                 and sample.transport == "websocket"),
                key=lambda sample: sample.monotonic_ns)
            handshake_checks = [
                check for check in checks
                if check["phase"] == f"websocket/{schema}/handshake-after"
                and "connection_owner_pid" in check
            ]
            if (not websocket_samples
                    or not handshake_checks
                    or any(
                        not any(
                            check["after_monotonic_ns"] <= sample.monotonic_ns
                            for check in handshake_checks)
                        for sample in websocket_samples)):
                raise ValueError(
                    f"capture has reused or unattested {name} websocket samples")

    attestation = metadata.get("build_attestation")
    if not isinstance(attestation, dict):
        raise ValueError("capture lacks valid immediate-build attestation")
    expected_tool_names = (
        ("git", "node", "pnpm", "python", "powershell", "cargo", "rustc", "cmd")
        if sys.platform == "win32" and source_mode == "node-build"
        else ("cargo", "rustc", "cmd")
        if sys.platform == "win32"
        else ("git", "node", "pnpm", "python", "cargo", "rustc")
        if source_mode == "node-build"
        else ("cargo", "rustc")
    )
    toolchain_records = attestation.get("toolchain")
    toolchain_valid = (
        isinstance(toolchain_records, dict)
        and set(toolchain_records) == set(expected_tool_names))
    if toolchain_valid:
        for tool_name in expected_tool_names:
            trusted_path = _resolved_trusted_command(tool_name)
            record = toolchain_records[tool_name]
            if (not isinstance(record, dict)
                    or record.get("path") != str(trusted_path)
                    or record.get("sha256") != sha256_file(trusted_path)):
                toolchain_valid = False
                break
    root = Path(__file__).resolve().parents[2]
    target_dir = Path(attestation.get("vosu_target_dir", "")).resolve(strict=True)
    vosu_path = Path(vosu["executable"]).resolve(strict=True)
    vosu_command = attestation.get("vosu_command")
    vosu_build_home = Path(
        attestation.get("vosu_build_home", "")).resolve(strict=True)
    cargo_home = Path(attestation.get("cargo_home", "")).resolve(strict=True)
    trusted_build_environment = {"CARGO_TARGET_DIR", "CARGO_HOME", "RUSTC"}
    source_build_environment = trusted_build_environment | {"PYTHON"}
    started = attestation.get("started_monotonic_ns")
    completed = attestation.get("completed_monotonic_ns")
    if sys.platform == "win32":
        _verify_msvc_attestation(attestation.get("vosu_msvc"))
        if source_mode == "node-build":
            _verify_msvc_attestation(attestation.get("source_msvc"))
    elif attestation.get("vosu_msvc") is not None:
        raise ValueError("capture has unexpected MSVC attestation")
    expected_vosu_build_arguments = [
        "build", "--release", "--locked", "--no-default-features",
        "--target", vosu.get("target_triple"),
    ]
    if sys.platform.startswith("linux"):
        expected_vosu_build_arguments.extend(["--features", "linux-process-events"])
    if (not toolchain_valid
            or attestation.get("source_mode") != source_mode
            or not isinstance(started, int)
            or not isinstance(completed, int)
            or completed < started
            or not _valid_environment_keys(
                attestation.get("vosu_environment_keys"), trusted_build_environment)
            or (
                not _valid_windows_build_path(
                    attestation.get("vosu_path"), attestation.get("vosu_msvc"),
                    expected_tool_names)
                if sys.platform == "win32"
                else attestation.get("vosu_path")
                != _controlled_tool_path(expected_tool_names))
            or attestation.get("rustc_path") != str(
                _resolved_trusted_command("rustc"))
            or _resolved_trusted_command("cargo").resolve()
            == _resolved_trusted_command("rustc").resolve()
            or any(path.resolve().name.lower().startswith("rustup")
                   for path in (_resolved_trusted_command("cargo"),
                                _resolved_trusted_command("rustc")))
            or attestation.get("vosu_executable_sha256") != vosu.get("sha256")
            or not isinstance(vosu_command, list)
            or not vosu_command
            or vosu_command[0] != toolchain_records["cargo"]["path"]
            or vosu_command[1:] != expected_vosu_build_arguments
            or target_dir not in vosu_path.parents
            or cargo_home.parent != vosu_build_home
            or (cargo_home / "config").exists()
            or (cargo_home / "config.toml").exists()
            or attestation.get("cargo_lock_sha256")
            != sha256_file(root / "Cargo.lock")
            or attestation.get("cargo_toml_sha256")
            != sha256_file(root / "Cargo.toml")
            or not all(isinstance(attestation.get(key), str) and attestation[key]
                       for key in ("rustc_version", "cargo_version"))):
        raise ValueError("capture lacks valid immediate-build attestation")
    if source_mode == "node-build":
        if checkout is None or manifest is None:
            raise ValueError("capture lacks pinned source-build artifacts")
        source_environment_keys = attestation.get("source_environment_keys")
        commands = attestation.get("source_commands")
        pnpm = (
            commands[0][0]
            if isinstance(commands, list) and commands and commands[0]
            else None)
        if (not _valid_environment_keys(
                    source_environment_keys, source_build_environment)
                or (
                    sys.platform == "win32"
                    and not _valid_windows_build_path(
                        attestation.get("source_path"),
                        attestation.get("source_msvc"), expected_tool_names))
                or (
                    sys.platform != "win32"
                    and attestation.get("source_path")
                    != attestation.get("vosu_path"))
                or attestation.get("source_revision") != PINNED_SOURCE_REVISION
                or attestation.get("source_dist_manifest") != manifest
                or attestation.get("source_lock_sha256")
                != sha256_file(checkout / "pnpm-lock.yaml")
                or attestation.get("node_executable_sha256")
                != source.get("executable_sha256")
                or not all(
                    isinstance(attestation.get(key), str) and attestation[key]
                    for key in ("node_version", "pnpm_version"))
                or not isinstance(pnpm, str)
                or pnpm != toolchain_records["pnpm"]["path"]
                or not _valid_source_build_commands(
                    commands, toolchain_records["pnpm"]["path"])
                or "source_reference" in attestation):
            raise ValueError("capture lacks exact pinned source-build attestation")
    else:
        expected_reference = {
            "executable": source["executable"],
            "sha256": source["sha256"],
            "version": source["version"],
            "command": source["command"],
        }
        node_only_fields = {
            "source_revision", "source_build_home", "source_environment_keys",
            "source_path", "source_commands", "source_dist_manifest",
            "source_lock_sha256", "node_version", "pnpm_version",
            "node_executable_sha256", "source_msvc",
        }
        if (attestation.get("source_reference") != expected_reference
                or node_only_fields.intersection(attestation)):
            raise ValueError(
                "capture lacks exact native source-reference attestation")

    launch_evidence = metadata.get("client_launch")
    launch_mode = (
        launch_evidence.get("mode") if isinstance(launch_evidence, dict) else None
    )
    launch_command = (
        launch_evidence.get("command") if isinstance(launch_evidence, dict) else None
    )
    process_command = (
        launch_evidence.get("process_command")
        if isinstance(launch_evidence, dict)
        else None
    )
    expected_args = (
        launch_evidence.get("expected_args")
        if isinstance(launch_evidence, dict)
        else None
    )
    client_environment_keys = (
        launch_evidence.get("environment_keys")
        if isinstance(launch_evidence, dict)
        else None
    )
    requires_private_job = (
        metadata.get("os") == "windows" and launch_mode == "launch")
    launch_evidence_valid = (
        launch_mode == "launch"
        and isinstance(launch_command, list)
        and isinstance(process_command, list)
        and len(launch_command) == len(process_command)
        and bool(launch_command)
        and Path(process_command[0]).resolve() == Path(launch_command[0]).resolve()
        and process_command[1:] == launch_command[1:] == expected_args
        and launch_command[0] == client.get("executable")
        and launch_evidence.get("executable") == client.get("executable")
        and launch_evidence.get("private_job", False) is requires_private_job
        and launch_evidence.get(
            "atomic_process_snapshot", False) is requires_private_job
        and _valid_environment_keys(client_environment_keys)
    )
    runtime_version_source = (
        launch_evidence.get("runtime_version_source")
        if isinstance(launch_evidence, dict)
        else None
    )
    expected_version_source = client.get("version_source")
    external_version_source_valid = (
        runtime_version_source is None
        and expected_version_source is None
    ) or (
        isinstance(runtime_version_source, dict)
        and isinstance(expected_version_source, dict)
        and runtime_version_source.get("sha256")
        == expected_version_source.get("sha256")
    )
    external_evidence_valid = (
        launch_mode == "external"
        and launch_command is None
        and _valid_environment_keys(client_environment_keys)
        and isinstance(process_command, list)
        and bool(process_command)
        and isinstance(expected_args, list)
        and process_command[1:] == expected_args
        and isinstance(launch_evidence.get("executable"), str)
        and Path(launch_evidence["executable"]).is_absolute()
        and launch_evidence.get("runtime_version") == client.get("version")
        and external_version_source_valid
    )
    approved_processes = client.get("approved_processes")
    if (
        not isinstance(launch_evidence, dict)
        or not _valid_client_process_group(approved_processes, client)
        or launch_evidence.get("approved_processes") != approved_processes
        or launch_evidence.get("pid") != client.get("pid")
        or launch_evidence.get("start_identity") != client.get("start_identity")
        or launch_evidence.get("sha256") != client.get("sha256")
        or launch_evidence.get("version") != client.get("version")
        or not (launch_evidence_valid or external_evidence_valid)
    ):
        raise ValueError("capture lacks exact attested-client identity evidence")
    exit_evidence = metadata.get("client_process_exit")
    schemas = exit_evidence.get("exact_not_ready_schemas", {}) \
        if isinstance(exit_evidence, dict) else {}
    if (not isinstance(exit_evidence, dict)
            or exit_evidence.get("observed_terminated") is not True
            or not isinstance(exit_evidence.get("observed_monotonic_ns"), int)
            or exit_evidence.get("pid") != client.get("pid")
            or exit_evidence.get("start_identity") != client.get("start_identity")
            or exit_evidence.get("approved_processes") != approved_processes
            or any(schemas.get(name) != sorted(SCHEMAS) for name in ("source", "vosu"))):
        raise ValueError("capture lacks exact all-schema client-exit evidence")
