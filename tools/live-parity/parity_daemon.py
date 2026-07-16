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
from live_parity import (SCHEMAS, PINNED_VOSU_VERSION, PINNED_SOURCE_REVISION, VOSU_BUILD_LABEL,
    SOURCE_BUILD_LABEL, SUPPORTED_MATRIX, Sample, align_active, barrier_of,
    beatmap_time, compare_pair, dump_capture, load_capture, monotonic_violations,
    paused_of, state_of)
from parity_domains import (
    cadence_evidence, confirmed_retry_boundaries, gamebase_path_family,
    infer_reference_capabilities, segment_at,
)
from parity_windows_job import WindowsClientJob, windows_osu_process_snapshot
import parity_process as _parity_process
globals().update({name: value for name, value in vars(_parity_process).items() if not name.startswith("__")})
import parity_artifacts as _parity_artifacts
globals().update({name: value for name, value in vars(_parity_artifacts).items() if not name.startswith("__")})
import parity_probes as _parity_probes
globals().update({name: value for name, value in vars(_parity_probes).items() if not name.startswith("__")})

_WINDOWS_CLIENT_JOBS: dict[int, WindowsClientJob] = {}
def _signal_process_tree(
    process: subprocess.Popen[bytes], *, hard: bool = False
) -> None:
    if process.poll() is not None:
        return
    if os.name == "posix":
        try:
            group = os.getpgid(process.pid)
            if group != process.pid:
                raise ValueError(
                    f"process {process.pid} is not its own process-group leader")
            os.killpg(group, signal.SIGKILL if hard else signal.SIGTERM)
        except ProcessLookupError:
            return
    elif hard:
        process.kill()
    else:
        process.terminate()


def _private_runtime_dir(prefix: str) -> Path:
    parent: Path | None = None
    if sys.platform == "win32":
        local_data = os.environ.get("LOCALAPPDATA")
        if not local_data:
            raise ValueError("LOCALAPPDATA is required for Windows runtime isolation")
        parent = Path(local_data) / "vosu-live-parity"
        parent.mkdir(parents=True, exist_ok=True)
    return Path(tempfile.mkdtemp(prefix=prefix, dir=parent))


def launch(cfg: dict[str, Any]) -> tuple[dict[str, subprocess.Popen[bytes]], dict[str, dict[str, Any]]]:
    processes: dict[str, subprocess.Popen[bytes]] = {}
    evidence: dict[str, dict[str, Any]] = {}
    runtime_dirs: list[Path] = []
    seeded_configs: list[Path] = []
    for name in ("source", "vosu"):
        port = urlsplit(cfg["daemons"][name]["base_url"]).port
        assert port is not None
        if not _port_is_unused(port):
            raise ValueError(f"{name} launch port {port} is already occupied")
    try:
        for name in ("source", "vosu"):
            spec = cfg["daemons"][name]
            port = urlsplit(spec["base_url"]).port
            assert port is not None
            env = _scrubbed_environment(spec.get("env"))
            env.update({
                "SERVER_IP": "127.0.0.1", "SERVER_PORT": str(port),
                "POLL_RATE": str(cfg["poll_rate_ms"]),
                "ENABLE_AUTOUPDATE": "false", "OPEN_DASHBOARD_ON_STARTUP": "false",
            })
            cwd = spec.get("cwd")
            runtime_dir: Path | None = None
            seeded_config: Path | None = None
            daemon_config = "\n".join([
                "DEBUG_LOG=false",
                "ENABLE_AUTOUPDATE=false",
                "OPEN_DASHBOARD_ON_STARTUP=false",
                "",
                "SHOW_MP_COMMANDS=false",
                "CALCULATE_PP=true",
                "READ_MANIA_SCROLL_SPEED=true",
                "",
                "ENABLE_KEY_OVERLAY=true",
                "ENABLE_INGAME_OVERLAY=false",
                "",
                f"POLL_RATE={cfg['poll_rate_ms']}",
                "PRECISE_DATA_POLL_RATE=10",
                "",
                "INGAME_OVERLAY_KEYBIND=Control + Shift + Space",
                "INGAME_OVERLAY_MAX_FPS=60",
                "",
                "SERVER_IP=127.0.0.1",
                f"SERVER_PORT={port}",
                "ALLOWED_IPS=127.0.0.1,localhost,absolute",
                "",
                "STATIC_FOLDER_PATH=./static",
                "",
            ])
            if sys.platform == "win32":
                if name == "source":
                    runtime_dir = _private_runtime_dir(
                        "vosu-live-parity-source-")
                    runtime_dirs.append(runtime_dir)
                    cwd = str(runtime_dir)
                    config_path = runtime_dir / "vosu.env"
                else:
                    config_path = Path(
                        spec["command"][0]).resolve(strict=True).parent / "vosu.env"
                    if config_path.exists():
                        raise ValueError(
                            "fresh vosu artifact directory contains vosu.env")
                    seeded_config = config_path
                    seeded_configs.append(config_path)
            else:
                runtime_dir = _private_runtime_dir(
                    f"vosu-live-parity-{name}-")
                runtime_dirs.append(runtime_dir)
                xdg_config = runtime_dir / ".config"
                env.update({
                    "HOME": str(runtime_dir),
                    "USERPROFILE": str(runtime_dir),
                    "XDG_CONFIG_HOME": str(xdg_config),
                    "XDG_DATA_HOME": str(runtime_dir / ".local" / "share"),
                    "XDG_CACHE_HOME": str(runtime_dir / ".cache"),
                })
                config_path = xdg_config / "vosu" / "vosu.env"
            config_path.parent.mkdir(parents=True, exist_ok=True)
            config_path.write_text(daemon_config, encoding="utf-8")
            process = subprocess.Popen(
                spec["command"], cwd=cwd, env=env,
                stdout=None, stderr=None, start_new_session=True)
            if runtime_dir is not None:
                setattr(process, "_live_parity_runtime_dir", runtime_dir)
            if seeded_config is not None:
                setattr(process, "_live_parity_seeded_config", seeded_config)
            processes[name] = process
            start = _process_start_identity(process.pid)
            artifact = cfg.get("_artifact_identity", {}).get(name)
            allow_exclusive = (
                sys.platform.startswith("linux")
                and name == "source"
                and isinstance(artifact, dict)
                and artifact.get("mode") == "native-reference")
            ownership_mode: str | None = None
            observed_owner: int | None = None
            deadline = time.monotonic() + 15
            while ownership_mode is None:
                observed_owner = _socket_owner(port)
                if observed_owner == process.pid:
                    ownership_mode = "pid"
                elif (allow_exclusive and observed_owner is None
                      and not _port_is_unused(port)):
                    ownership_mode = "exclusive-port-hash"
                elif process.poll() is not None:
                    raise ValueError(
                        f"{name} configured child exited before owning port {port}")
                elif time.monotonic() >= deadline:
                    raise ValueError(
                        f"{name} configured child did not own port {port} within 15 seconds")
                else:
                    time.sleep(0.025)
            expected_path = (
                artifact.get("executable") if isinstance(artifact, dict) else None)
            expected_hash = (
                artifact.get("sha256") if name == "vosu"
                else artifact.get("executable_sha256")
            ) if isinstance(artifact, dict) else None
            if ownership_mode == "exclusive-port-hash":
                if not isinstance(expected_path, str) or not isinstance(
                        expected_hash, str):
                    raise ValueError(
                        "native reference lacks executable hash provenance")
                executable = Path(expected_path).resolve(strict=True)
                actual_hash = sha256_file(executable)
            else:
                executable = _process_executable(process.pid)
                actual_hash = sha256_file(executable)
            if (isinstance(artifact, dict)
                    and (str(executable) != expected_path
                         or actual_hash != expected_hash)):
                raise ValueError(
                    f"{name} launched executable does not match fresh artifact")
            now = time.monotonic_ns()
            evidence[name] = {
                "mode": "launch",
                "ownership_mode": ownership_mode,
                "pid": process.pid,
                "start_identity": start,
                "port": port,
                "executable": str(executable),
                "executable_sha256": actual_hash,
                "environment_keys": sorted(env),
                "ownership_checks": [{
                    "before_monotonic_ns": now,
                    "after_monotonic_ns": now,
                    "socket_owner_pid": observed_owner,
                    "port_exclusive": ownership_mode == "exclusive-port-hash",
                    "phase": "listen",
                }],
            }
        return processes, evidence
    except Exception:
        for process in processes.values():
            _signal_process_tree(process)
        for process in processes.values():
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                _signal_process_tree(process, hard=True)
        for runtime_dir in runtime_dirs:
            shutil.rmtree(runtime_dir, ignore_errors=True)
        for config_path in seeded_configs:
            config_path.unlink(missing_ok=True)
        raise
def _linux_osu_process(proc: Path) -> bool:
    try:
        stat = (proc / "stat").read_text(encoding="ascii")
        if stat[stat.rfind(")") + 2:].split()[0] == "Z":
            return False
    except (OSError, IndexError):
        pass

    names = {"osu!", "osu!.exe", "osulazer.exe"}
    candidates: set[str] = set()
    try:
        candidates.add((proc / "comm").read_text(
            encoding="utf-8", errors="replace").strip().lower())
    except OSError:
        pass
    try:
        command = (proc / "cmdline").read_bytes().split(b"\0", 1)[0]
        if command:
            candidates.add(Path(os.fsdecode(command)).name.lower())
    except OSError:
        pass
    try:
        executable = Path(os.readlink(proc / "exe")).name.lower()
        if executable.endswith(" (deleted)"):
            executable = executable[:-10]
        candidates.add(executable)
    except OSError:
        pass
    return bool(candidates & names)


def _running_osu_processes(
        client_job: WindowsClientJob | None = None) -> dict[int, dict[str, Any]]:
    if sys.platform.startswith("linux"):
        return {
            int(proc.name): {
                "pid": int(proc.name),
                "parent_pid": None,
                "start_identity": None,
                "executable": None,
            }
            for proc in Path("/proc").iterdir()
            if proc.name.isdigit() and _linux_osu_process(proc)
        }
    if sys.platform != "win32":
        raise ValueError("unsupported host")
    import ctypes
    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.argtypes = [
        wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.QueryFullProcessImageNameW.argtypes = [
        wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR,
        ctypes.POINTER(wintypes.DWORD)]
    kernel32.QueryFullProcessImageNameW.restype = wintypes.BOOL
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel32.CloseHandle.restype = wintypes.BOOL
    processes: dict[int, dict[str, Any]] = {}
    for pid, record in windows_osu_process_snapshot().items():
        process = kernel32.OpenProcess(0x1000, False, pid)
        if not process:
            processes[pid] = {
                **record,
                "executable": None,
                "sha256": None,
                "identity_source": "kernel-process-snapshot",
            }
            continue
        try:
            size = wintypes.DWORD(32768)
            buffer = ctypes.create_unicode_buffer(size.value)
            if not kernel32.QueryFullProcessImageNameW(
                    process, 0, buffer, ctypes.byref(size)):
                processes[pid] = {
                    **record,
                    "executable": None,
                    "sha256": None,
                    "identity_source": "kernel-process-snapshot",
                }
                continue
            processes[pid] = {
                **record,
                "executable": buffer.value,
                "identity_source": "process",
            }
        finally:
            kernel32.CloseHandle(process)
    return processes


def _running_osu_pids() -> set[int]:
    return set(_running_osu_processes())


def _client_process_records(client: dict[str, Any]) -> list[dict[str, Any]]:
    records = client.get("approved_processes")
    if isinstance(records, list) and records:
        return records
    if not isinstance(client.get("pid"), int) \
            or not isinstance(client.get("start_identity"), str):
        return []
    root = {
        "pid": client["pid"],
        "start_identity": client["start_identity"],
        "parent_pid": None,
        "executable": client.get("executable"),
        "sha256": client.get("sha256"),
        "identity_source": "process",
    }
    client["approved_processes"] = [root]
    return client["approved_processes"]


def _adopt_windows_client_descendant(
        client: dict[str, Any], process: dict[str, Any],
        client_job: WindowsClientJob | None = None) -> bool:
    if sys.platform != "win32":
        return False
    records = _client_process_records(client)
    pid = process["pid"]
    parent_pid = process["parent_pid"]
    parent = next(
        (record for record in records if record["pid"] == parent_pid), None)
    if parent is None:
        return False
    identity_source = process.get("identity_source", "process")
    job_record = client_job.observed(pid) if client_job else None
    if identity_source == "private-job-membership":
        observed_ns = process.get("observed_monotonic_ns")
        if (
            job_record is None
            or job_record.get("observed_monotonic_ns") != observed_ns
            or process.get("start_identity") != f"job:{pid}:{observed_ns}"
            or process.get("executable") is not None
            or process.get("sha256") is not None
        ):
            return False
        records.append({
            "pid": pid,
            "start_identity": process["start_identity"],
            "parent_pid": parent_pid,
            "executable": None,
            "sha256": None,
            "identity_source": identity_source,
            "observed_monotonic_ns": observed_ns,
        })
        return True
    if identity_source == "kernel-process-snapshot":
        expected = client.get("executable")
        start_identity = process.get("start_identity")
        if (
            not isinstance(expected, str)
            or not isinstance(start_identity, str)
            or not start_identity.isdigit()
            or (
                parent.get("identity_source") != "private-job-membership"
                and int(start_identity) <= int(parent["start_identity"])
            )
            or os.path.normcase(process.get("image_name", ""))
            != os.path.normcase(Path(expected).name)
            or process.get("executable") is not None
            or process.get("sha256") is not None
        ):
            return False
        records.append({
            "pid": pid,
            "start_identity": start_identity,
            "parent_pid": parent_pid,
            "executable": None,
            "sha256": None,
            "identity_source": identity_source,
            "image_name": process["image_name"],
        })
        return True
    if identity_source != "process":
        return False
    start_identity = process["start_identity"]
    if (
        parent.get("identity_source", "process") == "process"
        and int(start_identity) <= int(parent["start_identity"])
    ):
        return False
    executable = Path(process["executable"]).resolve(strict=True)
    expected = client.get("executable")
    expected_hash = client.get("sha256")
    if (not isinstance(expected, str)
            or os.path.normcase(str(executable)) != os.path.normcase(expected)
            or sha256_file(executable) != expected_hash):
        return False
    records.append({
        "pid": pid,
        "start_identity": start_identity,
        "parent_pid": parent_pid,
        "executable": str(executable),
        "sha256": expected_hash,
        "identity_source": identity_source,
    })
    return True


def _client_record_alive(record: dict[str, Any]) -> bool:
    if record.get("identity_source") == "private-job-membership":
        return False
    return _process_alive(record["pid"], record["start_identity"])


def _client_job(client: dict[str, Any]) -> WindowsClientJob | None:
    pid = client.get("pid")
    return _WINDOWS_CLIENT_JOBS.get(pid) if isinstance(pid, int) else None


def _close_windows_client_job(client: dict[str, Any]) -> None:
    pid = client.get("pid")
    job = _WINDOWS_CLIENT_JOBS.pop(pid, None) if isinstance(pid, int) else None
    if job is not None:
        job.close()


def _assert_client_population(client: dict[str, Any], results_ready: bool) -> bool:
    records = _client_process_records(client)
    alive = {
        record["pid"] for record in records if _client_record_alive(record)
    }
    client_job = _client_job(client)
    snapshot = _running_osu_processes(client_job)
    pending = set(snapshot) - alive
    while pending:
        adopted: set[int] = set()
        for pid in sorted(pending):
            if _adopt_windows_client_descendant(
                    client, snapshot[pid], client_job):
                adopted.add(pid)
        if not adopted:
            break
        pending -= adopted
    unauthorized = sorted(pending)
    if unauthorized:
        raise ValueError(
            "unapproved osu client process appeared during capture: "
            f"{unauthorized}")
    any_alive = any(_client_record_alive(record) for record in records)
    if not results_ready and not any_alive:
        raise ValueError("exact launched osu client exited before both result barriers")
    return any_alive


def _terminate_client_descendants(
        client: dict[str, Any], *, hard: bool = False) -> None:
    for record in reversed(_client_process_records(client)[1:]):
        if not _client_record_alive(record):
            continue
        try:
            os.kill(
                record["pid"],
                signal.SIGKILL if hard and os.name == "posix" else signal.SIGTERM,
            )
        except (OSError, ProcessLookupError):
            pass


def _wait_for_client_descendants(
        client: dict[str, Any], timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    descendants = [
        record for record in _client_process_records(client)[1:]
        if record.get("identity_source") != "private-job-membership"
    ]
    while time.monotonic() < deadline:
        if not any(_client_record_alive(record) for record in descendants):
            return True
        time.sleep(0.025)
    return not any(_client_record_alive(record) for record in descendants)


def _configure_linux_client() -> None:
    """Bind the client lifetime and ptrace access to the parity orchestrator."""
    import ctypes
    import signal

    pr_set_pdeathsig = 1
    pr_set_ptracer = 0x59616D61
    orchestrator_pid = os.getppid()
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(pr_set_pdeathsig, signal.SIGTERM, 0, 0, 0) != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))
    if os.getppid() != orchestrator_pid:
        os._exit(128 + signal.SIGTERM)
    if libc.prctl(pr_set_ptracer, orchestrator_pid, 0, 0, 0) != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def _launch_client(
    cfg: dict[str, Any],
) -> tuple[subprocess.Popen[bytes] | None, dict[str, Any]]:
    mode = cfg.get("client_launch_mode", "launch")
    existing = _running_osu_pids()
    if mode == "external":
        if len(existing) != 1:
            raise ValueError(
                "external client mode requires exactly one running osu client: "
                f"{sorted(existing)}"
            )
        process: subprocess.Popen[bytes] | None = None
        pid = next(iter(existing))
    else:
        if existing:
            raise ValueError(
                f"pre-existing osu client processes are forbidden: {sorted(existing)}"
            )
        process = None
        pid = -1

    artifact = cfg["_artifact_identity"]["client"]
    executable_path = Path(artifact["executable"]).resolve(strict=True)
    if (
        str(executable_path) != artifact["executable"]
        or sha256_file(executable_path) != artifact["sha256"]
    ):
        raise ValueError("configured osu client artifact changed before launch")
    version_source = artifact.get("version_source")
    if version_source is not None:
        version_path = Path(version_source["path"]).resolve(strict=True)
        if sha256_file(version_path) != version_source["sha256"]:
            raise ValueError("configured osu client version source changed before launch")

    command = [artifact["executable"], *cfg["client_args"]]
    client_environment: dict[str, str] | None = None
    if mode == "launch":
        client_environment = _scrubbed_environment()
        if sys.platform == "win32":
            process, client_job = WindowsClientJob.launch(
                command,
                cwd=str(Path(artifact["executable"]).parent),
                env=client_environment,
            )
            _WINDOWS_CLIENT_JOBS[process.pid] = client_job
        else:
            process = subprocess.Popen(
                command,
                cwd=str(Path(artifact["executable"]).parent),
                env=client_environment,
                stdout=None,
                stderr=None,
                start_new_session=True,
                preexec_fn=_configure_linux_client,
            )
        pid = process.pid

    try:
        start = _process_start_identity(pid)
        if process is not None:
            time.sleep(0.1)
            if process.poll() is not None:
                raise ValueError("configured osu client exited immediately")
        executable = _process_executable(pid)
        if (
            (mode == "launch" and str(executable) != artifact["executable"])
            or sha256_file(executable) != artifact["sha256"]
        ):
            raise ValueError("attested osu client process does not match configured artifact")
        runtime_version: str | None = None
        runtime_version_source: dict[str, str] | None = None
        if mode == "external":
            runtime_version, runtime_version_source = _derive_client_version(
                executable, cfg["client"]
            )
            expected_version_source = artifact.get("version_source")
            version_source_matches = (
                runtime_version_source is None
                and expected_version_source is None
            ) or (
                isinstance(runtime_version_source, dict)
                and isinstance(expected_version_source, dict)
                and runtime_version_source.get("sha256")
                == expected_version_source.get("sha256")
            )
            if runtime_version != artifact["version"] or not version_source_matches:
                raise ValueError(
                    "external osu client runtime version evidence does not match "
                    "the configured artifact"
                )
            external_environment_keys = _process_environment_keys(pid)
            if not _valid_environment_keys(external_environment_keys):
                raise ValueError(
                    "external osu client has a code-loading environment override"
                )
        artifact.update({
            "pid": pid,
            "start_identity": start,
            "approved_processes": [{
                "pid": pid,
                "start_identity": start,
                "parent_pid": None,
                "executable": str(executable),
                "sha256": artifact["sha256"],
                "identity_source": "process",
            }],
        })
        if not _assert_client_population(artifact, False):
            raise ValueError("attested osu client exited during launch")
        evidence = {
            "mode": mode,
            "pid": pid,
            "start_identity": start,
            "executable": str(executable),
            "sha256": artifact["sha256"],
            "version": artifact["version"],
            "process_command": _process_cmdline(pid),
            "expected_args": cfg["client_args"],
            "approved_processes": artifact["approved_processes"],
        }
        if mode == "external":
            evidence.update(
                {
                    "runtime_version": runtime_version,
                    "runtime_version_source": runtime_version_source,
                    "environment_keys": external_environment_keys,
                }
            )
        if mode == "launch":
            evidence.update(
                {
                    "command": command,
                    "environment_keys": sorted(client_environment or {}),
                    "private_job": sys.platform == "win32",
                    "atomic_process_snapshot": sys.platform == "win32",
                }
            )
        artifact.update({"pid": pid, "start_identity": start})
        return process, evidence
    except Exception:
        _close_windows_client_job(artifact)
        if process is not None:
            _signal_process_tree(process)
        raise




def _assert_runtime_ownership(
        evidence: dict[str, dict[str, Any]], phase: str = "capture") -> None:
    before = time.monotonic_ns()
    for record in evidence.values():
        _assert_one_owner(record, phase, before)


def _barrier_is_authorized(barrier: str | None, client: dict[str, Any]) -> bool:
    return barrier != "process-exit" or not _process_alive(client["pid"], client["start_identity"])
