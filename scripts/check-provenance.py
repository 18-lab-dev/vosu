#!/usr/bin/env python3
"""Verify first-party attribution, retained license copies, hashes, and packages.

--package-fixtures exercises packaging with explicit fixture bytes. It does not
compile or execute vosu and must not be reported as a native smoke test.
"""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
import zipfile


BASE = "9c2187e683775485b8551fd6e7cb09b6e6786332"
OLD = "Copyright (c) 2023-2026 Mikhail Babynichev"
APACHE_SHA256 = "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def module(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def manifest_rows(root):
    rows = []
    seen = set()
    for line in (root / "assets/SHA256SUMS").read_text().splitlines(keepends=True):
        match = re.fullmatch(r"([0-9a-f]{64})  (.+)\n?", line)
        if match is None:
            raise ValueError("unexpected asset manifest row")
        expected, name = match.groups()
        if name in seen or Path(name).is_absolute() or ".." in Path(name).parts:
            raise ValueError(f"unsafe or duplicate manifest path: {name}")
        seen.add(name)
        rows.append((expected, name, line))
    if "LICENSE" not in seen:
        raise ValueError("missing LICENSE row")
    return rows


def check(root, refresh):
    rows = manifest_rows(root)
    new = digest(root / "LICENSE")
    old = next(value for value, name, _ in rows if name == "LICENSE")
    for expected, name, _ in rows:
        if refresh and name == "LICENSE":
            continue
        if digest(root / name) != expected:
            raise ValueError(f"checksum mismatch: {name}")
    notices_path = root / "THIRD_PARTY_NOTICES.md"
    notices = notices_path.read_text(encoding="utf-8")
    if refresh:
        if notices.count(old) != 1:
            raise ValueError("notices do not contain exactly one previous root-license digest")
        new_rows = "".join(new + "  LICENSE\n" if name == "LICENSE" else line
                           for _, name, line in rows)
        (root / "assets/SHA256SUMS").write_text(new_rows, encoding="utf-8", newline="\n")
        notices = notices.replace(old, new)
        notices_path.write_text(notices, encoding="utf-8", newline="\n")
    if notices.count(new) != 1:
        raise ValueError("notices must repeat the current LICENSE digest exactly once")

    notice = "Copyright (c) 2026 sxlvia"
    for prefix in ("", "crates/vosu-pp/"):
        combined = (root / (prefix + "LICENSE")).read_text()
        mit = (root / (prefix + "LICENSE-MIT")).read_text()
        apache = (root / (prefix + "LICENSE-APACHE")).read_text()
        if mit not in combined or apache not in combined:
            raise ValueError(f"canonical terms missing from {prefix}LICENSE")
        if digest(root / (prefix + "LICENSE-APACHE")) != APACHE_SHA256:
            raise ValueError("canonical Apache license changed")
        if combined.splitlines().count(notice) != 2 or mit.splitlines().count(notice) != 1:
            raise ValueError(f"first-party notice inconsistent in {prefix}LICENSE / LICENSE-MIT")
        if OLD in combined or OLD in mit:
            raise ValueError("disputed first-party notice remains")
    if (root / "LICENSE-MIT").read_bytes() != (root / "crates/vosu-pp/LICENSE-MIT").read_bytes():
        raise ValueError("first-party MIT copies differ")
    if notice not in notices:
        raise ValueError("first-party notice missing from THIRD_PARTY_NOTICES.md")
    if f'.set("LegalCopyright", "{notice}")' not in (root / "build.rs").read_text():
        raise ValueError("Windows copyright resource differs from the first-party notice")
    if f"LegalCopyright = '{notice}'" not in (root / "scripts/verify-windows-release.ps1").read_text():
        raise ValueError("Windows verifier copyright differs from the first-party notice")
    assembler = module(root / "scripts/assemble-release-license.py", "release_license")
    if (root / "LICENSE").read_bytes() != assembler.expected_license(root):
        raise ValueError("release calculator appendix differs from its source")

    metadata = tomllib.loads((root / "Cargo.toml").read_text())["package"]["metadata"]["vosu"]
    for key, name in (("homepage-css-sha256", "assets/server/homepage.css"),
                      ("lazer-offsets-sha256", "assets/offsets.json")):
        if metadata[key] != digest(root / name):
            raise ValueError(f"Cargo metadata does not match {name}")
    if digest(root / "assets/offsets.json") not in notices:
        raise ValueError("current offset digest missing from notices")
    docs = module(root / "scripts/check-doc-links.py", "audit_doc_links")
    changed_docs = [root / "README.md", notices_path,
                    root / "openwiki/provenance-audit.md",
                    root / "openwiki/decisions/0007-vosu-clean-cutover.md"]
    errors = list(docs.broken_links(changed_docs))
    if errors:
        raise ValueError("\n".join(errors))
    print(json.dumps({"asset_hashes": len(rows), "license_sha256": new,
                      "first_party_notice": notice,
                      "canonical_license_copies": "match", "changed_document_links": "pass"}))


def packages(root):
    def run(*args, cwd=root):
        subprocess.run([sys.executable, *map(str, args)], cwd=cwd, check=True,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    version = tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"]
    tag = "v" + version
    development = module(root / "scripts/version-build.py", "audit_version_build")
    with tempfile.TemporaryDirectory(prefix="vosu-packaging-fixtures-") as directory:
        temporary = Path(directory)
        binary = temporary / "EXPLICIT_TEST_FIXTURE_NOT_A_DAEMON"
        binary.write_bytes(b"packaging fixture; not a native vosu binary\n")
        binary.chmod(0o755)
        output = temporary / "tag-releases"
        for platform, executable in (("linux", "vosu"), ("windows", "vosu.exe")):
            run("scripts/package-release.py", "package", "--platform", platform,
                "--tag", tag, "--binary", binary, "--output", output)
            archive = output / f"vosu-{platform}-{tag}.zip"
            run("scripts/package-release.py", "extract", "--platform", platform,
                "--tag", tag, "--archive", archive,
                "--destination", temporary / f"tag-{platform}-extract")
            with zipfile.ZipFile(archive) as file:
                if set(file.namelist()) != {executable, "LICENSE"} or file.testzip() is not None:
                    raise ValueError("tag archive members or CRC invalid")
                if file.read("LICENSE") != (root / "LICENSE").read_bytes():
                    raise ValueError("tag archive has stale license")
        run("scripts/package-release.py", "verify-set", "--tag", tag, "--directory", output)

        dev_root = temporary / "development-checkout"
        for name in (*development.ATTRIBUTION, "Cargo.toml", "rust-toolchain.toml",
                     "scripts/version-build.py"):
            destination = dev_root / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(root / name, destination)
        for platform, (target, executable, features) in development.TARGETS.items():
            destination = dev_root / "target" / target / "release" / executable
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(binary, destination)
            out = temporary / f"development-{platform}"
            run("scripts/version-build.py", "package", "--platform", platform,
                "--sha", BASE, "--output", out,
                "--destination", temporary / f"dev-{platform}-extract", cwd=dev_root)
            archives = list(out.glob("*.zip"))
            if len(archives) != 1:
                raise ValueError("expected one development archive")
            with zipfile.ZipFile(archives[0]) as file:
                expected = {executable, "BUILD-INFO.json", *development.ATTRIBUTION}
                if set(file.namelist()) != expected or file.testzip() is not None:
                    raise ValueError("development archive members or CRC invalid")
                for name in development.ATTRIBUTION:
                    if file.read(name) != (root / name).read_bytes():
                        raise ValueError(f"development archive has stale {name}")
                info = json.loads(file.read("BUILD-INFO.json"))
                if info["target"] != target or info["features"] != features:
                    raise ValueError("development build metadata mismatch")
        print("Packaging fixtures: Linux and Windows tag/development archives pass; no native build or smoke test performed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--refresh-license-hash", action="store_true")
    parser.add_argument("--package-fixtures", action="store_true")
    args = parser.parse_args()
    try:
        root = args.root.resolve()
        check(root, args.refresh_license_hash)
        if args.package_fixtures:
            packages(root)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
