"""Copy-client mode for golden captures (M4b O0 dry-run finding 1, owner amendment 2026-11-25).

The client's install folder is a root it declares (the v2 `folders.game`), so a capture
names it. A golden client therefore runs from a byte-identical copy inside the task-owned
capture root. A lazer copy also gets an empty `framework.ini`, which makes osu!framework
treat it as a portable install: its whole profile (realm, files store, logs, caches) stays
in the copy instead of the shared default `%APPDATA%\\osu` (finding 8).
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import shutil
from typing import Any

from parity_process import sha256_file


class ClientRootError(ValueError):
    """The client is not a verified copy inside the capture root."""


def inside(root: str, path: str) -> bool:
    root, path = (os.path.normcase(os.path.realpath(p)) for p in (root, path))
    return path != root and os.path.commonpath([root, path]) == root


def client_copy_manifest(source: str, copy: str) -> dict[str, Any]:
    """Every file of the installed client exists byte-identical in its copy."""
    lines, total = [], 0
    base = Path(source).resolve(strict=True)
    for path in sorted(p for p in base.rglob("*") if p.is_file()):
        rel = path.relative_to(base).as_posix()
        digest = sha256_file(path)
        twin = Path(copy, rel)
        if not twin.is_file() or sha256_file(twin) != digest:
            raise ClientRootError(f"client copy differs from the installed client at {rel}")
        lines.append(f"{digest}  {rel}")
        total += path.stat().st_size
    return {"files": len(lines), "bytes": total,
            "manifest_sha256": hashlib.sha256("\n".join(lines).encode("utf-8")).hexdigest()}


def check_client_root(cfg: dict[str, Any]) -> dict[str, Any]:
    """The attested client must run from inside the capture root (and, with
    `golden.client_source`, be a byte-identical copy of that install)."""
    executable = cfg["_artifact_identity"]["client"].get("executable")
    golden = cfg["golden"]
    if executable is None:
        return {"checked": False}
    client_dir = os.path.dirname(executable)
    if not inside(golden["capture_root"], client_dir):
        raise ClientRootError("the client runs from outside the capture root, so its install path "
                              "would appear in captures; copy it with `run.py golden-client-copy`")
    record: dict[str, Any] = {"checked": True, "inside_capture_root": True}
    if golden.get("client_source"):
        record["copy"] = client_copy_manifest(golden["client_source"], client_dir)
    return record


def golden_client_copy(cfg: dict[str, Any]) -> int:
    """Copy `golden.client_source` to the configured executable's directory."""
    golden = cfg.get("golden") or {}
    source, executable = golden.get("client_source"), cfg.get("client_executable")
    if not source or not executable or not golden.get("capture_root"):
        raise ClientRootError("golden-client-copy needs golden.client_source, golden.capture_root "
                              "and client_executable")
    destination = Path(executable).parent
    if not inside(golden["capture_root"], str(destination)):
        raise ClientRootError("the client copy must lie inside the capture root")
    if destination.exists():
        raise ClientRootError("the client copy directory exists; it is never replaced")
    shutil.copytree(source, destination)
    if cfg.get("client") == "lazer":
        (destination / "framework.ini").touch(exist_ok=False)
    record = client_copy_manifest(source, str(destination))
    if sha256_file(Path(executable)) != cfg.get("client_expected_sha256"):
        raise ClientRootError("the copied client executable does not match client_expected_sha256")
    print(json.dumps({"client_copy": destination.name, **record}, sort_keys=True))
    return 0
