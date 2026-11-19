"""Detect an upstream osu!lazer release that breaks automatic offset generation."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RELEASES_URL = "https://api.github.com/repos/ppy/osu/releases"
APPIMAGE_NAME = "osu.AppImage"
GENERATOR_PROJECT = "tools/lazer-offset-generator/lazer-offset-generator.csproj"
TARGET_PROJECT = "tools/lazer-offset-target/lazer-offset-target.csproj"
RID = "linux-x64"

RUNTIME_PACK_PREFIX = "runtimepack.Microsoft.NETCore.App.Runtime.linux-x64/"
HOSTS = ("libhostfxr.so", "libhostpolicy.so")
CORECLR = "libcoreclr.so"


class CanaryError(Exception):
    """A generation step failed; the message names the broken client/field."""


def select_release(releases, requested_tag=None):
    """Pick the release to canary: an explicit tag, or the newest non-prerelease `-lazer` tag."""
    if requested_tag is not None:
        for release in releases:
            if release.get("tag_name") == requested_tag:
                return release
        raise CanaryError(f"release tag {requested_tag!r} was not found")
    for release in releases:
        if not release.get("prerelease") and release.get("tag_name", "").endswith("-lazer"):
            return release
    raise CanaryError("no non-prerelease '-lazer' release was found")


def find_asset(release, name=APPIMAGE_NAME):
    for asset in release.get("assets", []):
        if asset.get("name") == name:
            return asset
    raise CanaryError(f"release {release.get('tag_name')!r} has no {name} asset")


def expected_sha256(asset):
    """A GitHub release asset's content digest, when the API exposes one (`sha256:<hex>`)."""
    digest = asset.get("digest")
    if not digest:
        return None
    algorithm, _, value = digest.partition(":")
    if algorithm != "sha256" or len(value) != 64:
        return None
    return value.lower()


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def diff_summary(old, new):
    """Informational field-by-field diff between two offsets documents; never fails the run."""
    added_types = sorted(set(new) - set(old) - {"OsuVersion", "GameBaseVtable"})
    removed_types = sorted(set(old) - set(new) - {"OsuVersion", "GameBaseVtable"})
    changed = {}
    for type_name in sorted(set(old) & set(new) - {"OsuVersion", "GameBaseVtable"}):
        old_fields = old[type_name] if isinstance(old[type_name], dict) else {}
        new_fields = new[type_name] if isinstance(new[type_name], dict) else {}
        field_diff = {
            field: (old_fields.get(field), new_fields.get(field))
            for field in set(old_fields) | set(new_fields)
            if old_fields.get(field) != new_fields.get(field)
        }
        if field_diff:
            changed[type_name] = field_diff
    return {
        "added_types": added_types,
        "removed_types": removed_types,
        "changed_fields": changed,
        "old_version": old.get("OsuVersion"),
        "new_version": new.get("OsuVersion"),
        "old_game_base_vtable": old.get("GameBaseVtable"),
        "new_game_base_vtable": new.get("GameBaseVtable"),
    }


def run(args, **kwargs):
    print("+", " ".join(str(a) for a in args), flush=True)
    return subprocess.run(args, check=True, **kwargs)


def fetch_json(url):
    request = urllib.request.Request(url, headers={"Accept": "application/vnd.github+json", "User-Agent": "vosu-lazer-offset-canary"})
    token = os.environ.get("GITHUB_TOKEN")
    if token:
        request.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.loads(response.read())


def download(url, destination):
    request = urllib.request.Request(url, headers={"User-Agent": "vosu-lazer-offset-canary"})
    with urllib.request.urlopen(request, timeout=300) as response, open(destination, "wb") as stream:
        shutil.copyfileobj(response, stream)


def publish_helper(project, destination):
    source = "https://api.nuget.org/v3/index.json"
    run(["dotnet", "restore", str(ROOT / project), "-p:UseAppHost=false", "--locked-mode", "--source", source])
    run([
        "dotnet", "publish", str(ROOT / project), "-c", "Release", "-r", RID,
        "--self-contained", "true", "--no-restore", "-o", str(destination),
    ])


def extract_appimage(appimage_path, work_dir):
    extracted = work_dir / "extracted"
    extracted.mkdir()
    appimage_path.chmod(appimage_path.stat().st_mode | stat.S_IEXEC)
    run([str(appimage_path), "--appimage-extract"], cwd=extracted)
    squashfs_root = extracted / "squashfs-root"
    assembly_dir = squashfs_root / "usr" / "bin"
    if not (assembly_dir / "osu!.dll").is_file() and not (assembly_dir / "osu.Game.dll").is_file():
        raise CanaryError(f"extracted AppImage has no managed assemblies under {assembly_dir}")
    return assembly_dir


def runtime_pack(deps):
    """The single matching Microsoft.NETCore.App runtime-pack entry in a deps.json document."""
    target = deps["runtimeTarget"]["name"]
    entries = deps["targets"][target]
    packs = [(name, pack) for name, pack in entries.items() if name.startswith(RUNTIME_PACK_PREFIX)]
    if len(packs) != 1:
        raise CanaryError("runtime dependencies must contain exactly one matching self-contained runtime pack")
    name, pack = packs[0]
    return target, name, pack


def runtime_files(pack):
    files = set()
    for kind in ("runtime", "native"):
        files.update((pack.get(kind) or {}).keys())
    return files


def include_framework_overrides(client_deps, client_target, helper_pack, client_pack):
    """Pull in the client's own app-dependency files that fill gaps the helper's framework needs."""
    helper_runtime = set((helper_pack.get("runtime") or {}).keys())
    runtime = client_pack.setdefault("runtime", {})
    entries = client_deps["targets"][client_target]
    pending = []
    for name, library in entries.items():
        files = library.get("runtime") or {}
        if any(Path(path).name in helper_runtime and Path(path).name not in runtime for path in files):
            pending.append((name, library))
    visited = set()
    while pending:
        name, library = pending.pop()
        if name in visited:
            continue
        visited.add(name)
        for path, metadata in (library.get("runtime") or {}).items():
            runtime.setdefault(Path(path).name, metadata)
        for dependency_name, dependency_version in (library.get("dependencies") or {}).items():
            key = f"{dependency_name}/{dependency_version}"
            entry = entries.get(key)
            if entry is None:
                raise CanaryError("framework dependency is absent from client manifest")
            pending.append((key, entry))


def link_or_copy(source, destination):
    try:
        os.link(source, destination)
    except OSError:
        shutil.copy2(source, destination)


def stage_client_runtime(payload, assembly, work):
    """Give each helper the client's exact CLR and framework instead of its own build
    runtime: a newer game can reference framework APIs unavailable to the helper's SDK,
    and CLR forks can have different object layouts from stock .NET. Mirrors
    src/runtime/offset_generator/client_runtime.rs so this canary exercises the same
    staging the daemon runs at generation time, not a different path."""
    client_deps = json.loads((assembly / "osu!.deps.json").read_text())
    config = json.loads((assembly / "osu!.runtimeconfig.json").read_text())
    client_target, client_name, client_pack = runtime_pack(client_deps)
    client_files_base = runtime_files(client_pack)
    for name in (*HOSTS, CORECLR, "System.Private.CoreLib.dll"):
        if name not in client_files_base or not (assembly / name).is_file():
            raise CanaryError("client self-contained runtime is incomplete")
    staged = work / "client-runtime"
    staged.mkdir()
    for helper in ("generator", "target"):
        name = f"lazer-offset-{helper}"
        source = payload / helper
        destination = staged / helper
        destination.mkdir()
        deps_name = f"{name}.deps.json"
        config_name = f"{name}.runtimeconfig.json"
        deps = json.loads((source / deps_name).read_text())
        old_target, old_name, old_pack = runtime_pack(deps)
        old_files = runtime_files(old_pack)
        client_pack_copy = json.loads(json.dumps(client_pack))
        include_framework_overrides(client_deps, client_target, old_pack, client_pack_copy)
        client_files = runtime_files(client_pack_copy)
        for entry in source.iterdir():
            if entry.is_file() and entry.name not in (deps_name, config_name) and entry.name not in old_files and entry.name not in client_files:
                # Apphosts canonicalize the managed entry point's path. A symlink
                # would silently bind it back to the original .NET 8 bundle.
                link_or_copy(entry, destination / entry.name)
        for filename in client_files:
            client_source = assembly / filename
            if filename in ("createdump", "createdump.exe") and not client_source.exists():
                continue
            if not client_source.is_file():
                raise CanaryError(f"client runtime pack file is missing: {filename}")
            if filename in HOSTS:
                link_or_copy(client_source, destination / filename)
            else:
                os.symlink(client_source, destination / filename)
        entries = dict(deps["targets"][old_target])
        entries.pop(old_name, None)
        entries[client_name] = client_pack_copy
        deps["targets"] = {client_target: entries}
        deps["runtimeTarget"] = client_deps["runtimeTarget"]
        libraries = deps["libraries"]
        libraries.pop(old_name, None)
        libraries[client_name] = client_deps["libraries"][client_name]
        (destination / deps_name).write_text(json.dumps(deps))
        (destination / config_name).write_text(json.dumps(config))
    return staged


def generate_offsets(staged, assembly_dir, template, output):
    generator = staged / "generator" / "lazer-offset-generator"
    target = staged / "target" / "lazer-offset-target"
    result = subprocess.run(
        [str(generator), "--assemblies-only", "--assembly-dir", str(assembly_dir),
         "--template", str(template), "--output", str(output)],
        capture_output=True, text=True,
    )
    if result.returncode != 0:
        raise CanaryError(
            "assembly-only offset generation failed (the daemon's embedded generator runs "
            f"this same step at runtime): {result.stderr.strip()}"
        )
    signature = subprocess.run(
        [str(target), "--game-base-signature", "--assembly-dir", str(assembly_dir)],
        capture_output=True, text=True,
    )
    if signature.returncode != 0:
        raise CanaryError(
            f"owned target could not derive GameBaseVtable: {signature.stderr.strip()}"
        )
    sig = json.loads(signature.stdout)
    generated = json.loads(output.read_text())
    if generated.get("OsuVersion") != sig.get("OsuVersion") or not sig.get("GameBaseVtable"):
        raise CanaryError(
            "owned target did not supply a nonzero GameBaseVtable for the generated OsuVersion"
        )
    generated["GameBaseVtable"] = sig["GameBaseVtable"]
    output.write_text(json.dumps(generated, indent=1) + "\n")
    return generated


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", help="exact osu!lazer release tag to canary, e.g. 2026.1117.0-lazer; default: newest non-prerelease -lazer tag")
    parser.add_argument("--template", type=Path, default=ROOT / "assets/offsets.json")
    parser.add_argument("--work-dir", type=Path, help="scratch directory; default a fresh temporary directory")
    parser.add_argument("--keep", action="store_true", help="keep the work directory instead of deleting it")
    args = parser.parse_args(argv)

    try:
        releases = fetch_json(RELEASES_URL)
        release = select_release(releases, args.tag)
        tag = release["tag_name"]
        print(f"Canarying osu!lazer release {tag}")
        asset = find_asset(release)
        digest = expected_sha256(asset)

        work_dir = args.work_dir or Path(tempfile.mkdtemp(prefix="vosu-lazer-offset-canary-"))
        work_dir.mkdir(parents=True, exist_ok=True)
        try:
            appimage_path = work_dir / APPIMAGE_NAME
            print(f"Downloading {asset['browser_download_url']}")
            download(asset["browser_download_url"], appimage_path)
            actual_digest = sha256_file(appimage_path)
            if digest is not None:
                if actual_digest != digest:
                    raise CanaryError(
                        f"osu.AppImage sha256 mismatch for {tag}: expected {digest}, got {actual_digest}"
                    )
                print(f"osu.AppImage sha256 verified against release digest: {actual_digest}")
            else:
                print(f"osu.AppImage sha256 (release exposes no digest to verify against): {actual_digest}")

            assembly_dir = extract_appimage(appimage_path, work_dir)

            generator_dir = work_dir / "generator"
            target_dir = work_dir / "target"
            publish_helper(GENERATOR_PROJECT, generator_dir)
            publish_helper(TARGET_PROJECT, target_dir)
            staged = stage_client_runtime(work_dir, assembly_dir, work_dir)

            output_path = work_dir / "offsets.json"
            generated = generate_offsets(staged, assembly_dir, args.template, output_path)

            generated_digest = hashlib.sha256(output_path.read_bytes()).hexdigest()
            print(f"Generated offsets for {tag}: sha256 {generated_digest}")

            template_offsets = json.loads(args.template.read_text())
            summary = diff_summary(template_offsets, generated)
            print(json.dumps(summary, indent=1))
        finally:
            if not args.keep:
                shutil.rmtree(work_dir, ignore_errors=True)
    except CanaryError as error:
        print(f"lazer offset canary failed for the client it generated against: {error}", file=sys.stderr)
        return 1
    except urllib.error.URLError as error:
        print(f"lazer offset canary could not reach GitHub: {error}", file=sys.stderr)
        return 1
    print("lazer offset canary: offset generation succeeded")
    return 0


if __name__ == "__main__":
    sys.exit(main())
