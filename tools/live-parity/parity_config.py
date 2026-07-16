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
def config_schema() -> dict[str, Any]:
    daemon = {
        "type": "object", "required": ["base_url", "command"],
        "properties": {
            "base_url": {"type": "string", "pattern": r"^http://127\.0\.0\.1:[0-9]+$"},
            "command": {"type": "array", "minItems": 1, "items": {"type": "string", "minLength": 1}},
            "cwd": {"type": "string", "minLength": 1},
            "env": {"type": "object", "additionalProperties": {"type": ["string", "number", "boolean"]}},
        },
        "additionalProperties": False,
    }
    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object",
        "required": ["client_executable", "client_args",
                     "client_expected_sha256", "client_expected_version",
                     "os", "client", "poll_rate_ms", "daemons"],
        "properties": {
            "source_mode": {"enum": ["node-build", "native-reference"]},
            "source_directory": {"type": "string", "minLength": 1},
            "source_reference": {
                "type": "object",
                "required": ["executable", "sha256", "version"],
                "properties": {
                    "executable": {"type": "string", "minLength": 1},
                    "sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                    "version": {"type": "string", "const": PINNED_SOURCE_VERSION},
                },
                "additionalProperties": False,
            },
            "client_executable": {"type": "string", "minLength": 1},
            "client_args": {"type": "array", "items": {"type": "string"}},
            "client_launch_mode": {"enum": ["launch", "external"]},
            "client_expected_sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            "client_expected_version": {"type": "string", "minLength": 1},
            "os": {"enum": ["windows", "linux"]}, "client": {"enum": ["stable", "lazer"]},
            "poll_rate_ms": {"type": "integer", "const": 150},
            "daemons": {"type": "object", "required": ["source", "vosu"],
                        "properties": {"source": daemon, "vosu": daemon}, "additionalProperties": False},
        },
        "allOf": [{
            "oneOf": [
                {
                    "required": ["source_directory"],
                    "properties": {
                        "source_mode": {"enum": ["node-build"]},
                    },
                    "not": {"required": ["source_reference"]},
                },
                {
                    "required": ["source_mode", "source_reference"],
                    "properties": {
                        "source_mode": {"const": "native-reference"},
                    },
                    "not": {"required": ["source_directory"]},
                },
            ],
        }],
        "oneOf": [
            {"properties": {"os": {"const": "windows"}, "client": {"const": "stable"}}},
            {"properties": {"os": {"const": "windows"}, "client": {"const": "lazer"}}},
            {"properties": {"os": {"const": "linux"}, "client": {"const": "lazer"}}},
        ],
        "additionalProperties": False,
    }
def load_config(path: str) -> dict[str, Any]:
    with open(path, encoding="utf-8") as fh:
        cfg = json.load(fh)
    errors: list[str] = []
    allowed_top = {
        "source_mode", "source_directory", "source_reference",
        "client_executable", "client_args", "client_launch_mode",
        "client_expected_sha256", "client_expected_version",
        "os", "client", "poll_rate_ms", "daemons",
    }
    unknown = set(cfg) - allowed_top
    if unknown:
        errors.append("unknown configuration fields: " + ", ".join(sorted(unknown)))
    source_mode = cfg.get("source_mode", "node-build")
    if source_mode not in {"node-build", "native-reference"}:
        errors.append("source_mode must be node-build or native-reference")
    else:
        cfg["source_mode"] = source_mode
    source_directory = cfg.get("source_directory")
    source_reference = cfg.get("source_reference")
    if source_mode == "node-build":
        if not isinstance(source_directory, str) or not source_directory:
            errors.append("source_directory is required in node-build mode")
        if source_reference is not None:
            errors.append("source_reference is forbidden in node-build mode")
    elif source_mode == "native-reference":
        if source_directory is not None:
            errors.append("source_directory is forbidden in native-reference mode")
        if not isinstance(source_reference, dict):
            errors.append("source_reference is required in native-reference mode")
        else:
            unknown_reference = set(source_reference) - {
                "executable", "sha256", "version"}
            if unknown_reference:
                errors.append(
                    "source_reference has unknown fields: "
                    + ", ".join(sorted(unknown_reference)))
            reference_executable = source_reference.get("executable")
            if (not isinstance(reference_executable, str)
                    or not reference_executable):
                errors.append("source_reference.executable is required")
            elif (not Path(reference_executable).is_absolute()
                  or str(Path(reference_executable).resolve())
                  != reference_executable):
                errors.append(
                    "source_reference.executable must be an absolute canonical path")
            reference_digest = source_reference.get("sha256")
            if (not isinstance(reference_digest, str)
                    or len(reference_digest) != 64
                    or any(char not in "0123456789abcdef"
                           for char in reference_digest)):
                errors.append(
                    "source_reference.sha256 must be 64 lowercase hexadecimal characters")
            if source_reference.get("version") != PINNED_SOURCE_VERSION:
                errors.append(
                    f"source_reference.version must be exactly {PINNED_SOURCE_VERSION}")
    launch_mode = cfg.get("client_launch_mode", "launch")
    if launch_mode not in {"launch", "external"}:
        errors.append("client_launch_mode must be launch or external")
    else:
        cfg["client_launch_mode"] = launch_mode
    os_name, client = str(cfg.get("os", "")).lower(), str(cfg.get("client", "")).lower()
    host_os = "windows" if sys.platform == "win32" else "linux" if sys.platform.startswith("linux") else "unsupported"
    if os_name != host_os:
        errors.append(f"configured os {os_name} does not match native host {host_os}")
    if (os_name, client) not in SUPPORTED_MATRIX:
        errors.append(f"unsupported matrix entry {os_name}/{client}; expected windows/stable, windows/lazer, or linux/lazer")
    if cfg.get("poll_rate_ms") != 150 or isinstance(cfg.get("poll_rate_ms"), bool):
        errors.append("poll_rate_ms must be exactly 150 for the release parity gate")
    for field in ("client_executable", "client_expected_version"):
        if not isinstance(cfg.get(field), str) or not cfg[field]:
            errors.append(f"{field} is required")
    digest = cfg.get("client_expected_sha256")
    if (not isinstance(digest, str) or len(digest) != 64
            or any(char not in "0123456789abcdef" for char in digest)):
        errors.append("client_expected_sha256 must be 64 lowercase hexadecimal characters")
    args = cfg.get("client_args")
    if not isinstance(args, list) or not all(isinstance(arg, str) for arg in args):
        errors.append("client_args must be an array of strings")
    daemons = cfg.get("daemons")
    if not isinstance(daemons, dict) or set(daemons) != {"source", "vosu"}:
        errors.append("daemons must contain exactly source and vosu")
    else:
        ports: list[int] = []
        for name in ("source", "vosu"):
            daemon = daemons[name]
            if not isinstance(daemon, dict):
                errors.append(f"daemons.{name} must be an object")
                continue
            daemon_unknown = set(daemon) - {"base_url", "command", "cwd", "env"}
            if daemon_unknown:
                errors.append(f"daemons.{name} has unknown fields: {', '.join(sorted(daemon_unknown))}")
            command = daemon.get("command")
            if (not isinstance(command, list) or not command
                    or not all(isinstance(item, str) and item for item in command)):
                errors.append(f"daemons.{name}.command must be a nonempty argument array")
            if (name == "source" and source_mode == "native-reference"
                    and ("cwd" in daemon or "env" in daemon)):
                errors.append(
                    "native-reference source daemon must not override cwd or env")
            env_values = daemon.get("env", {})
            if not isinstance(env_values, dict):
                errors.append(f"daemons.{name}.env must be an object")
            else:
                forbidden_env = [
                    key for key in env_values
                    if _is_injection_environment(str(key))
                ]
                if forbidden_env:
                    errors.append(f"daemons.{name}.env contains code-loading overrides: "
                                  + ", ".join(sorted(forbidden_env)))
            url = daemon.get("base_url")
            parsed = urlsplit(url) if isinstance(url, str) else None
            if (parsed is None or parsed.scheme != "http" or parsed.hostname != "127.0.0.1"
                    or parsed.path not in ("", "/") or parsed.query or parsed.fragment):
                errors.append(f"daemons.{name}.base_url must be exactly http://127.0.0.1:<port>")
            try:
                port = parsed.port if parsed else None
                if port is None:
                    raise ValueError
                ports.append(port)
            except ValueError:
                errors.append(f"daemons.{name}.base_url has no valid port")
        if len(ports) == 2 and ports[0] == ports[1]:
            errors.append("source and vosu ports must be distinct")
    if errors:
        raise ValueError("; ".join(errors))
    try:
        cfg["_artifact_identity"] = validate_artifacts(cfg)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError, json.JSONDecodeError) as exc:
        raise ValueError(f"artifact provenance verification failed: {exc}") from exc
    return cfg
