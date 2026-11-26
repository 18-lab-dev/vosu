#!/usr/bin/env python3
"""Reject changes to the packaged SDK under a version already on crates.io."""

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parents[1]
CRATE = "vosu-plugin-api"
SDK_DIR = Path("crates") / CRATE


def package_files(archive, version):
    """Hash every package file except Cargo's commit-specific VCS metadata."""
    root = PurePosixPath(f"{CRATE}-{version}")
    files = {}
    with tarfile.open(archive, "r:gz") as package:
        for member in package:
            path = PurePosixPath(member.name).relative_to(root)
            if not member.isfile() or path == PurePosixPath(".") or ".." in path.parts:
                raise ValueError(f"invalid SDK archive member: {member.name}")
            name = path.as_posix()
            if name == ".cargo_vcs_info.json":
                continue
            if name in files:
                raise ValueError(f"duplicate SDK archive member: {member.name}")
            with package.extractfile(member) as source:
                files[name] = hashlib.file_digest(source, "sha256").hexdigest()
    return files


def package_sdk(root, target, version):
    subprocess.run(
        ["cargo", "package", "--no-verify", "--locked", "--offline", "--allow-dirty",
         "-p", CRATE, "--target-dir", str(target)],
        cwd=root,
        check=True,
    )
    return target / "package" / f"{CRATE}-{version}.crate"


def check(root=ROOT):
    sdk = root / SDK_DIR
    with (sdk / "Cargo.toml").open("rb") as manifest:
        version = tomllib.load(manifest)["package"]["version"]
    with (sdk / "published.toml").open("rb") as ledger:
        published = tomllib.load(ledger)
    if version not in published:
        print(f"{CRATE} {version} is not in the publication ledger")
        return 0

    expected = published[version]["files"]
    with tempfile.TemporaryDirectory(prefix="vosu-sdk-published-") as tmp:
        archive = package_sdk(root, Path(tmp), version)
        actual = package_files(archive, version)
    if actual == expected:
        print(f"{CRATE} {version} matches the published package contents")
        return 0

    print(f"{CRATE} {version} differs from the published package; bump the SDK version "
          "and regenerate Cargo.lock.", file=sys.stderr)
    for label, paths in (
        ("removed", expected.keys() - actual.keys()),
        ("added", actual.keys() - expected.keys()),
        ("changed", {name for name in expected.keys() & actual.keys() if expected[name] != actual[name]}),
    ):
        for name in sorted(paths):
            print(f"  {label}: {name}", file=sys.stderr)
    return 1


def ledger_entry(archive):
    version = archive.stem.removeprefix(f"{CRATE}-")
    if archive.suffix != ".crate" or version == archive.stem or not version:
        raise ValueError(f"expected a {CRATE}-<version>.crate archive")
    with archive.open("rb") as source:
        checksum = hashlib.file_digest(source, "sha256").hexdigest()
    files = package_files(archive, version)
    print(f'\n["{version}"]\nregistry_sha256 = "{checksum}"\n\n["{version}".files]')
    for name, digest in sorted(files.items()):
        print(f'{json.dumps(name, ensure_ascii=False)} = "{digest}"')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ledger-entry", type=Path, metavar="CRATE", help="print a ledger entry for a published archive")
    args = parser.parse_args()
    try:
        if args.ledger_entry is None:
            return check()
        ledger_entry(args.ledger_entry)
        return 0
    except (KeyError, OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
