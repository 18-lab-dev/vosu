#!/usr/bin/env python3
"""Create or verify the two-entry vosu daemon release archives."""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
import tomllib
import zipfile
from pathlib import Path
from typing import NamedTuple

# SemVer 2.0.0 without build metadata: numeric parts and numeric prerelease
# identifiers have no leading zeros; identifiers are non-empty ASCII
# alphanumerics or hyphens. Explicit classes keep Unicode digits out.
_NUMBER = r"(?:0|[1-9][0-9]*)"
_IDENTIFIER = rf"(?:{_NUMBER}|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
VERSION_RE = re.compile(
    rf"(?P<core>{_NUMBER}\.{_NUMBER}\.{_NUMBER})(?:-(?P<prerelease>{_IDENTIFIER}(?:\.{_IDENTIFIER})*))?"
)
# dpkg, rpm and pacman must all order a prerelease below its release and agree
# with SemVer among prereleases. That holds for one alphabetic label followed
# by numeric counters (alpha, beta.2, rc.1); other SemVer prereleases are
# rejected for distribution packaging rather than rendered ambiguously.
DISTRIBUTION_PRERELEASE_RE = re.compile(rf"[A-Za-z]+(?:\.{_NUMBER})*")
PLATFORM_EXECUTABLE = {"windows": "vosu.exe", "linux": "vosu"}
PLUGIN_HEADER_ASSET = "vosu_plugin.h"
DEFAULT_PLUGIN_HEADER = Path("include/vosu_plugin.h")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def verify_plugin_header(staged: Path, source: Path = DEFAULT_PLUGIN_HEADER) -> str:
    if not source.is_file():
        raise ValueError(f"C plugin ABI header source does not exist: {source}")
    if not staged.is_file():
        raise ValueError(f"staged C plugin ABI header does not exist: {staged}")
    expected_bytes = source.read_bytes()
    if not expected_bytes:
        raise ValueError(f"C plugin ABI header source is empty: {source}")
    actual_bytes = staged.read_bytes()
    expected_sha = sha256_bytes(expected_bytes)
    actual_sha = sha256_bytes(actual_bytes)
    if actual_sha != expected_sha or actual_bytes != expected_bytes:
        raise ValueError(
            f"{staged} does not match {source} "
            f"(expected sha256 {expected_sha}, got {actual_sha})"
        )
    return actual_sha


def parse_version(version: str) -> tuple[str, str | None]:
    """Split a release version into its core and optional prerelease."""
    match = VERSION_RE.fullmatch(version)
    if match is None:
        raise ValueError(
            "release version must be <major>.<minor>.<patch>[-<prerelease>] SemVer without"
            f" leading zeros or build metadata, got {version!r}"
        )
    return match["core"], match["prerelease"]


def version_from_tag(tag: str, manifest: Path) -> str:
    if not tag.startswith("v"):
        raise ValueError(f"release tag must be v<major>.<minor>.<patch>[-<prerelease>], got {tag!r}")
    version = tag[1:]
    parse_version(version)
    with manifest.open("rb") as file:
        package_version = tomllib.load(file)["package"]["version"]
    if version != package_version:
        raise ValueError(
            f"tag version {version!r} does not match Cargo package version {package_version!r}"
        )
    return version


def is_prerelease(version: str) -> bool:
    return parse_version(version)[1] is not None


class DistributionPackage(NamedTuple):
    # nfpm packager that builds this format.
    packager: str
    # Release asset name; it spells the SemVer version, which GitHub keeps as uploaded.
    asset: str
    # Distribution-native upstream version recorded in the package metadata.
    version: str


def distribution_packages(version: str) -> tuple[DistributionPackage, ...]:
    """Render the deb, rpm and Arch packages of one release version.

    A SemVer hyphen is the Debian revision separator and is invalid in an RPM
    Version or an Arch pkgver, so a prerelease is rendered natively: dpkg and
    rpm sort `~` before everything, and pacman sorts a trailing alphabetic
    segment before the bare release. `~` stays out of asset names because
    GitHub renames it on upload. Stable versions render unchanged.
    """
    core, prerelease = parse_version(version)
    if prerelease is None:
        debian = rpm = arch = core
    elif DISTRIBUTION_PRERELEASE_RE.fullmatch(prerelease):
        debian = rpm = f"{core}~{prerelease}"
        arch = f"{core}{prerelease}"
    else:
        raise ValueError(
            f"prerelease {prerelease!r} cannot be ordered consistently by dpkg, rpm and pacman;"
            " use an alphabetic label with optional numeric counters such as alpha.1"
        )
    return (
        DistributionPackage("deb", f"vosu_{version}_amd64.deb", debian),
        DistributionPackage("rpm", f"vosu-{version}-1.x86_64.rpm", rpm),
        DistributionPackage("archlinux", f"vosu-{version}-1-x86_64.pkg.tar.zst", arch),
    )


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
    version = version_from_tag(args.tag, args.manifest)
    # Fail at the first workflow step, not after the native builds, when the
    # Linux packages cannot carry this version.
    distribution_packages(version)
    print(version)


def release_flags(version: str) -> dict[str, str]:
    """GitHub release fields for the draft the release workflow creates.

    A prerelease is never the repository's latest release, so 1.x updaters and
    `/releases/latest` consumers keep seeing the newest stable release. An
    empty `make_latest` leaves a stable release on GitHub's default.
    """
    if is_prerelease(version):
        return {"prerelease": "true", "make_latest": "false"}
    return {"prerelease": "false", "make_latest": ""}


def write_release_flags(args: argparse.Namespace) -> None:
    flags = release_flags(version_from_tag(args.tag, args.manifest))
    print("".join(f"{key}={value}\n" for key, value in flags.items()), end="")


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


def package_header(args: argparse.Namespace) -> Path:
    version_from_tag(args.tag, args.manifest)
    if not args.header.is_file():
        raise ValueError(f"C plugin ABI header does not exist: {args.header}")
    args.output.mkdir(parents=True, exist_ok=True)
    destination = args.output / PLUGIN_HEADER_ASSET
    destination.write_bytes(args.header.read_bytes())
    verify_plugin_header(destination, args.header)
    print(destination)
    return destination


def verify_release_set(args: argparse.Namespace) -> None:
    version = version_from_tag(args.tag, args.manifest)
    archives = {
        f"vosu-windows-v{version}.zip": "windows",
        f"vosu-linux-v{version}.zip": "linux",
    }
    expected = (
        set(archives)
        | {package.asset for package in distribution_packages(version)}
        | {PLUGIN_HEADER_ASSET}
    )
    actual = {path.name for path in args.directory.iterdir() if path.is_file()}
    if actual != expected:
        raise ValueError(
            f"release directory must contain exactly {sorted(expected)}; found {sorted(actual)}"
        )
    for name, platform in archives.items():
        verify_archive(args.directory / name, platform, args.license)
    verify_plugin_header(args.directory / PLUGIN_HEADER_ASSET, args.header)
    print("\n".join(sorted(expected)))


REPOSITORY_URL = "https://github.com/18-lab-dev/vosu"
MARKDOWN_LINK = re.compile(r"(!?\[[^\]\n]*\]\()([^)\s]+)(\))")


def pin_repository_links(markdown: str, tag: str) -> str:
    """Point repository-relative link targets at the tagged tree.

    CHANGELOG.md links are relative to the repository root; on a release page
    they would resolve against the releases URL instead of the source files.
    """

    def pin(match: re.Match[str]) -> str:
        target = match.group(2)
        if re.match(r"[a-z][a-z0-9+.-]*:", target, re.I) or target.startswith(("#", "/")):
            return match.group(0)
        return f"{match.group(1)}{REPOSITORY_URL}/blob/{tag}/{target.removeprefix('./')}{match.group(3)}"

    return MARKDOWN_LINK.sub(pin, markdown)


def release_notes(tag: str, changelog: str, signed: bool) -> str:
    version = tag.removeprefix("v")
    section = re.search(
        # The version must end the heading's first word: `\b` would let a
        # `## 2.0.0` lookup match the `## 2.0.0-alpha.1` section.
        rf"^## {re.escape(version)}(?![0-9A-Za-z.+-])[^\n]*\n(.*?)(?=^## |\Z)", changelog, re.S | re.M
    )
    if section is None:
        raise ValueError(f"CHANGELOG.md has no section for {version}")
    base = f"{REPOSITORY_URL}/blob/{tag}"
    if signed:
        signing = (
            f"The Windows executable is signed under the [code signing policy]({base}/CODE_SIGNING_POLICY.md)."
            " Free code signing provided by [SignPath.io](https://about.signpath.io),"
            " certificate by [SignPath Foundation](https://signpath.org)."
        )
    else:
        signing = (
            "SignPath signing is not active yet, so this release's Windows executable is unsigned."
            f" See the [code signing policy]({base}/CODE_SIGNING_POLICY.md)."
        )
    return (
        pin_repository_links(section.group(1).strip(), tag)
        + f"\n\n## Code signing policy\n\n{signing}"
        + f"\n\n[Privacy policy]({base}/PRIVACY.md) · [Security and installation disclosure]({base}/docs/security.md)\n"
    )


def write_release_notes(args: argparse.Namespace) -> None:
    version_from_tag(args.tag, args.manifest)
    notes = release_notes(args.tag, args.changelog.read_text(encoding="utf-8"), args.signed)
    args.output.write_text(notes, encoding="utf-8")
    print(args.output)


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

    header = subparsers.add_parser(
        "package-header", help="stage and verify the standalone C plugin ABI header asset"
    )
    header.add_argument("--tag", required=True)
    header.add_argument("--header", type=Path, default=DEFAULT_PLUGIN_HEADER)
    header.add_argument("--output", type=Path, required=True)
    header.set_defaults(run=package_header)

    verify = subparsers.add_parser("verify-set", help="inspect the complete release asset set")
    verify.add_argument("--tag", required=True)
    verify.add_argument("--directory", type=Path, required=True)
    verify.add_argument("--license", type=Path, default=Path("LICENSE"))
    verify.add_argument("--header", type=Path, default=DEFAULT_PLUGIN_HEADER)
    verify.set_defaults(run=verify_release_set)

    flags = subparsers.add_parser(
        "release-flags", help="print the GitHub release prerelease/make_latest fields as key=value lines"
    )
    flags.add_argument("--tag", required=True)
    flags.set_defaults(run=write_release_flags)

    notes = subparsers.add_parser("notes", help="write release notes from the tag's CHANGELOG section")
    notes.add_argument("--tag", required=True)
    notes.add_argument("--changelog", type=Path, default=Path("CHANGELOG.md"))
    notes.add_argument("--signed", action="store_true")
    notes.add_argument("--output", type=Path, required=True)
    notes.set_defaults(run=write_release_notes)
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
