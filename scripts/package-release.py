#!/usr/bin/env python3
"""Create or verify the two-entry vosu daemon release archives."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
import zipfile
from pathlib import Path

TAG_RE = re.compile(r"^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
PLATFORM_EXECUTABLE = {"windows": "vosu.exe", "linux": "vosu"}


def version_from_tag(tag: str, manifest: Path) -> str:
    match = TAG_RE.fullmatch(tag)
    if match is None:
        raise ValueError(f"release tag must be stable v<major>.<minor>.<patch>, got {tag!r}")

    version = tag[1:]
    with manifest.open("rb") as file:
        package_version = tomllib.load(file)["package"]["version"]
    if version != package_version:
        raise ValueError(
            f"tag version {version!r} does not match Cargo package version {package_version!r}"
        )
    return version


def expected_names(platform: str) -> set[str]:
    return {PLATFORM_EXECUTABLE[platform], "LICENSE"}


def verify_archive(archive: Path, platform: str, license_path: Path = Path("LICENSE")) -> None:
    expected = expected_names(platform)
    with zipfile.ZipFile(archive) as file:
        infos = file.infolist()
        names = [entry.filename for entry in infos]
        if len(infos) != 2 or set(names) != expected or len(names) != len(set(names)):
            raise ValueError(
                f"{archive} must contain exactly {sorted(expected)} at its root; found {names}"
            )
        if any("/" in name or "\\" in name for name in names):
            raise ValueError(f"{archive} contains a non-root entry: {names}")
        if any(entry.is_dir() for entry in infos):
            raise ValueError(f"{archive} must not contain directory entries")
        if file.testzip() is not None:
            raise ValueError(f"{archive} failed its CRC check")
        if file.read("LICENSE") != license_path.read_bytes():
            raise ValueError(f"{archive} LICENSE does not match {license_path}")


def validate_tag(args: argparse.Namespace) -> None:
    print(version_from_tag(args.tag, args.manifest))


def extract_archive(args: argparse.Namespace) -> None:
    version = version_from_tag(args.tag, args.manifest)
    expected_name = f"vosu-{args.platform}-v{version}.zip"
    if args.archive.name != expected_name:
        raise ValueError(
            f"archive must be named {expected_name!r}, got {args.archive.name!r}"
        )
    verify_archive(args.archive, args.platform, args.license)
    if args.destination.exists() and any(args.destination.iterdir()):
        raise ValueError(f"extraction destination is not empty: {args.destination}")
    args.destination.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(args.archive) as file:
        file.extractall(args.destination)
        if args.platform == "linux":
            executable = args.destination / PLATFORM_EXECUTABLE[args.platform]
            archived_mode = file.getinfo(executable.name).external_attr >> 16
            executable.chmod(archived_mode or 0o755)
    print(args.destination / PLATFORM_EXECUTABLE[args.platform])


def package(args: argparse.Namespace) -> Path:
    version = version_from_tag(args.tag, args.manifest)
    executable_name = PLATFORM_EXECUTABLE[args.platform]
    expected_archive_name = f"vosu-{args.platform}-v{version}.zip"
    archive = args.output / expected_archive_name

    if not args.binary.is_file():
        raise ValueError(f"release executable does not exist: {args.binary}")
    if not args.license.is_file():
        raise ValueError(f"vendored license does not exist: {args.license}")

    args.output.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as file:
        file.write(args.binary, executable_name)
        file.write(args.license, "LICENSE")

    verify_archive(archive, args.platform, args.license)
    print(archive)
    return archive


def verify_release_set(args: argparse.Namespace) -> None:
    version = version_from_tag(args.tag, args.manifest)
    expected = {
        f"vosu-windows-v{version}.zip": "windows",
        f"vosu-linux-v{version}.zip": "linux",
    }
    packages = {
        f"vosu_{version}_amd64.deb",
        f"vosu-{version}-1.x86_64.rpm",
        f"vosu-{version}-1-x86_64.pkg.tar.zst",
        f"vosu-bin-aur-v{version}.tar.gz",
    }
    actual = {path.name for path in args.directory.iterdir() if path.is_file()}
    if actual != set(expected) | packages:
        raise ValueError(
            f"release directory must contain exactly {sorted(set(expected) | packages)}; found {sorted(actual)}"
        )
    for name, platform in expected.items():
        verify_archive(args.directory / name, platform, args.license)
    print("\n".join(sorted(set(expected) | packages)))


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument(
        "--manifest", type=Path, default=Path("Cargo.toml"), help="root Cargo manifest"
    )
    subparsers = result.add_subparsers(dest="command", required=True)

    validate = subparsers.add_parser(
        "validate-tag", help="validate release tag syntax and Cargo package version"
    )
    validate.add_argument("--tag", required=True)
    validate.set_defaults(run=validate_tag)

    extract = subparsers.add_parser(
        "extract", help="validate and cleanly extract one release archive"
    )
    extract.add_argument("--platform", choices=sorted(PLATFORM_EXECUTABLE), required=True)
    extract.add_argument("--tag", required=True)
    extract.add_argument("--archive", type=Path, required=True)
    extract.add_argument("--destination", type=Path, required=True)
    extract.add_argument("--license", type=Path, default=Path("LICENSE"))
    extract.set_defaults(run=extract_archive)

    create = subparsers.add_parser("package", help="create and inspect one release archive")
    create.add_argument("--platform", choices=sorted(PLATFORM_EXECUTABLE), required=True)
    create.add_argument("--tag", required=True)
    create.add_argument("--binary", type=Path, required=True)
    create.add_argument("--license", type=Path, default=Path("LICENSE"))
    create.add_argument("--output", type=Path, required=True)
    create.set_defaults(run=package)

    verify = subparsers.add_parser("verify-set", help="inspect the complete release asset set")
    verify.add_argument("--tag", required=True)
    verify.add_argument("--directory", type=Path, required=True)
    verify.add_argument("--license", type=Path, default=Path("LICENSE"))
    verify.set_defaults(run=verify_release_set)
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        args.run(args)
    except (KeyError, OSError, ValueError, zipfile.BadZipFile) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
