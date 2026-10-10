#!/usr/bin/env python3
"""Gate version builds and package unsigned development artifacts (Python 3.11+)."""

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
import zipfile

# Preserve repository-relative paths used by the attribution document.
ATTRIBUTION = (
    "LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "THIRD_PARTY_NOTICES.md",
    "crates/vosu-pp/LICENSE", "crates/vosu-pp/LICENSE-MIT",
    "crates/vosu-pp/LICENSE-APACHE", "crates/vosu-pp/THIRD_PARTY_LICENSES.txt",
    "tools/oracle/reference/lazer-calculator/LICENSE",
    "Cargo.lock", "assets/SHA256SUMS",
    "crates/vosu-pp/rosu-pp-api/LICENSE", "crates/vosu-pp/rosu-map-api/LICENSE",
    "docs/provenance-audit.md",
)
TARGETS = {
    "linux": ("x86_64-unknown-linux-gnu", "vosu", ["linux-process-events"]),
    "windows": ("x86_64-pc-windows-msvc", "vosu.exe", []),
}


def package_version(text):
    version = tomllib.loads(text)["package"]["version"]
    # SemVer's filename-safe alphabet; Cargo remains authoritative for validity.
    if not isinstance(version, str) or not re.fullmatch(
        r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version
    ):
        raise ValueError("root package.version must be an explicit filename-safe SemVer string")
    return version


def commit_sha(value):
    if not re.fullmatch(r"[0-9a-f]{40}", value) or value == "0" * 40:
        raise ValueError("comparison requires a nonzero full commit SHA; use workflow_dispatch for a new branch")
    return value


def detect(args):
    version = package_version(Path("Cargo.toml").read_text(encoding="utf-8"))
    sha = commit_sha(subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip())
    if args.event == "workflow_dispatch":
        build = True
    elif args.event == "push":
        before = args.before
        if before is None:
            event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
            before = event.get("before", "")
        before = commit_sha(before)
        present = subprocess.run(["git", "cat-file", "-e", f"{before}^{{commit}}"], capture_output=True)
        if present.returncode:
            # Fetch only the event's base, never full history or a guessed HEAD^.
            subprocess.run(["git", "fetch", "--no-tags", "--depth=1", "origin", before], check=True)
        old = subprocess.check_output(["git", "show", f"{before}:Cargo.toml"], text=True, encoding="utf-8")
        build = package_version(old) != version
    else:
        raise ValueError("only push and workflow_dispatch events are supported")
    values = {
        "build": str(build).lower(), "version": version, "sha": sha,
        "week": datetime.now(timezone.utc).strftime("%G-%V"),
    }
    lines = "".join(f"{key}={value}\n" for key, value in values.items())
    print(lines, end="")
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write(lines)


def package(args):
    version = package_version(Path("Cargo.toml").read_text(encoding="utf-8"))
    sha = commit_sha(args.sha)
    target, executable, features = TARGETS[args.platform]
    binary = Path("target") / target / "release" / executable
    sources = [(binary, executable)] + [(Path(name), name) for name in ATTRIBUTION]
    for source, _ in sources:
        if not source.is_file():
            raise ValueError(f"missing artifact input: {source}")
    destination = args.destination
    if destination.exists() and any(destination.iterdir()):
        raise ValueError(f"smoke destination must be empty: {destination}")
    args.output.mkdir(parents=True, exist_ok=True)
    archive = args.output / f"vosu-unsigned-dev-v{version}-{sha}-{target}.zip"
    metadata = {
        "kind": "unsigned-development-build", "version": version, "commit": sha,
        "target": target, "default_features": False, "features": features,
        "toolchain": tomllib.loads(Path("rust-toolchain.toml").read_text(encoding="utf-8"))["toolchain"]["channel"],
    }
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=6) as file:
        for source, name in sources:
            file.write(source, name)
        file.writestr("BUILD-INFO.json", json.dumps(metadata, indent=2) + "\n")
    # Inspect and extract the exact ZIP that will be uploaded, not the build tree.
    expected = {name for _, name in sources} | {"BUILD-INFO.json"}
    with zipfile.ZipFile(archive) as file:
        names = file.namelist()
        if len(names) != len(expected) or set(names) != expected:
            raise ValueError("unexpected development archive members")
        if file.testzip() is not None:
            raise ValueError("development archive CRC check failed")
        destination.mkdir(parents=True, exist_ok=True)
        file.extractall(destination)
        if args.platform == "linux":
            (destination / executable).chmod(0o755)
    print(archive)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(required=True)
    gate = commands.add_parser("detect")
    gate.add_argument("--event", default=os.environ.get("GITHUB_EVENT_NAME"))
    gate.add_argument("--before", help="push base SHA; otherwise read GITHUB_EVENT_PATH")
    gate.set_defaults(run=detect)
    create = commands.add_parser("package")
    create.add_argument("--platform", choices=TARGETS, required=True)
    create.add_argument("--sha", required=True)
    create.add_argument("--output", type=Path, default=Path("dist"))
    create.add_argument("--destination", type=Path, default=Path("version-smoke"))
    create.set_defaults(run=package)
    args = parser.parse_args()
    try:
        args.run(args)
    except (KeyError, OSError, ValueError, subprocess.CalledProcessError, zipfile.BadZipFile) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
