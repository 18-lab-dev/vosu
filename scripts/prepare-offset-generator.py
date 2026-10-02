"""Build the pinned offset helper and create a deterministic versioned bundle."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TAG = "offset-generator-v1"
RIDS = ("linux-x64", "win-x64")


def run(args: list[str], cwd: Path | None = None) -> None:
    print("+", " ".join(args), flush=True)
    subprocess.run(args, cwd=cwd, check=True)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def fingerprint() -> str:
    digest = hashlib.sha256()
    inputs = [
        Path(__file__),
        ROOT / "tools/lazer-offset-generator/lazer-offset-generator.csproj",
        ROOT / "tools/lazer-offset-generator/packages.lock.json",
        ROOT / "tools/lazer-offset-target/lazer-offset-target.csproj",
        ROOT / "tools/lazer-offset-target/packages.lock.json",
        ROOT / "LICENSE",
        ROOT / "LICENSE-MIT",
        ROOT / "LICENSE-APACHE",
        ROOT / "THIRD_PARTY_NOTICES.md",
    ]
    inputs.extend(sorted((ROOT / "tools/lazer-offset-generator").glob("*.cs")))
    inputs.extend(sorted((ROOT / "tools/lazer-offset-target").glob("*.cs")))
    for path in inputs:
        digest.update(path.relative_to(ROOT).as_posix().encode())
        digest.update(path.read_bytes())
    return digest.hexdigest()


def publish_helper(project_name: str, rid: str, destination: Path) -> None:
    project = ROOT / f"tools/{project_name}/{project_name}.csproj"
    source = "https://api.nuget.org/v3/index.json"
    run(["dotnet", "restore", str(project), "-p:UseAppHost=false", "--locked-mode", "--source", source])
    run([
        "dotnet", "publish", str(project), "-c", "Release", "-r", rid,
        "--self-contained", "true", "--no-restore", "-o", str(destination),
    ])


def copy_runtime_licenses(rid: str, staging: Path) -> None:
    assets = json.loads((ROOT / "tools/lazer-offset-target/obj/project.assets.json").read_text())
    for component in ("runtime", "host"):
        package = Path(f"microsoft.netcore.app.{component}.{rid}") / "8.0.29"
        source = next(
            (Path(root) / package for root in assets["packageFolders"] if (Path(root) / package).is_dir()),
            None,
        )
        if source is None:
            raise RuntimeError(f"restored runtime attribution package is missing: {package}")
        destination = staging / "licenses" / package
        destination.mkdir(parents=True)
        for name in ("LICENSE.TXT", "THIRD-PARTY-NOTICES.TXT"):
            shutil.copy2(source / name, destination / name)


def make_bundle(rid: str, output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="vosu-offset-bundle-", dir=output.parent) as temporary:
        temporary_root = Path(temporary)
        staging = temporary_root / "payload"
        staging.mkdir()
        publish_helper("lazer-offset-generator", rid, staging / "generator")
        publish_helper("lazer-offset-target", rid, staging / "target")
        for name in ("LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "THIRD_PARTY_NOTICES.md"):
            shutil.copy2(ROOT / name, staging / name)
        copy_runtime_licenses(rid, staging)
        files = {
            path.relative_to(staging).as_posix(): sha256(path)
            for path in sorted(staging.rglob("*")) if path.is_file()
        }
        manifest = {"rid": rid, "files": files}
        manifest_bytes = (json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n").encode()
        (staging / "bundle.json").write_bytes(manifest_bytes)
        temporary_zip = output.with_suffix(output.suffix + ".tmp")
        with zipfile.ZipFile(temporary_zip, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            for path in sorted(staging.rglob("*")):
                if path.is_file():
                    info = zipfile.ZipInfo(path.relative_to(staging).as_posix(), (1980, 1, 1, 0, 0, 0))
                    info.compress_type = zipfile.ZIP_DEFLATED
                    info.external_attr = 0o100644 << 16
                    archive.writestr(info, path.read_bytes())
        os.replace(temporary_zip, output)




def verified_reuse(output: Path, rid: str, marker: Path, current_fingerprint: str) -> bool:
    if not (output.is_file() and marker.is_file() and marker.read_text().strip() == current_fingerprint):
        return False
    try:
        with zipfile.ZipFile(output) as archive:
            manifest = json.loads(archive.read("bundle.json"))
            files = manifest["files"]
            if not isinstance(files, dict):
                return False
            suffix = ".exe" if rid == "win-x64" else ""
            required = {
                f"generator/lazer-offset-generator{suffix}",
                f"target/lazer-offset-target{suffix}",
            }
            names = archive.namelist()
            expected = set(files) | {"bundle.json"}
            return (
                manifest["rid"] == rid
                and required <= set(files)
                and len(names) == len(expected)
                and set(names) == expected
                and all(
                    isinstance(name, str)
                    and name
                    and "\\" not in name
                    and ":" not in name
                    and not name.startswith("/")
                    and all(part not in ("", ".", "..") for part in name.split("/"))
                    and isinstance(digest, str)
                    and len(digest) == 64
                    and all(character in "0123456789abcdef" for character in digest)
                    and sha256_bytes(archive.read(name)) == digest
                    for name, digest in files.items()
                )
            )
    except (KeyError, OSError, TypeError, ValueError, zipfile.BadZipFile):
        return False


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rid", choices=RIDS, required=True)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    output = (args.output or ROOT / "target/offset-generator" / args.rid / f"{TAG}-{args.rid}.zip").resolve()
    marker = output.with_suffix(".inputs.sha256")
    current_fingerprint = fingerprint()
    checksum_file = output.with_suffix(output.suffix + ".sha256")
    if verified_reuse(output, args.rid, marker, current_fingerprint):
        checksum_file.write_text(f"{sha256(output)}  {output.name}\n")
        print(f"reusing verified {output}")
        return 0
    make_bundle(args.rid, output)
    marker.write_text(current_fingerprint + "\n")
    checksum_file.write_text(f"{sha256(output)}  {output.name}\n")
    print(f"created {output} (sha256 {sha256(output)})")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"prepare-offset-generator: error: {error}", file=sys.stderr)
        raise SystemExit(1)
