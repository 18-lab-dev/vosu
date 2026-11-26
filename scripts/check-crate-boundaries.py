#!/usr/bin/env python3
"""Enforce the recorded workspace-crate dependency graph (scripts/crate-boundaries.txt)
against `cargo metadata`. Every workspace crate must be listed (even with no
internal dependencies), and every path dependency between workspace crates
must be a recorded edge, so a new crate or a new cross-crate dependency is a
deliberate, reviewed change alongside
docs/v2/implementation-plan.md section 2 and docs/architecture/source-map.md.

Vendored, provenance-pinned trees (crates/vosu-pp/rosu-*-api) are out of
scope, matching scripts/check-file-length.py.
Also check that live-parity tooling pins the app version reported by Cargo.
"""

import argparse
import json
from pathlib import Path
import runpy
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
RULES = ROOT / "scripts/crate-boundaries.txt"

EXCLUDED_MANIFEST_DIRS = {
    ROOT / "crates/vosu-pp/rosu-pp-api",
    ROOT / "crates/vosu-pp/rosu-map-api",
}


def cargo_metadata():
    output = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--offline"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return json.loads(output)


def workspace_packages(metadata):
    """In-scope workspace member packages, keyed by crate name."""
    members = set(metadata["workspace_members"])
    packages = {}
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        if Path(package["manifest_path"]).parent in EXCLUDED_MANIFEST_DIRS:
            continue
        packages[package["name"]] = package
    return packages


def build_graph(metadata):
    """Return (crate names, edges) for path dependencies between workspace crates."""
    packages = workspace_packages(metadata)
    names = set(packages)
    edges = set()
    for name, package in packages.items():
        for dependency in package["dependencies"]:
            target = dependency["name"]
            if "path" in dependency and target in names and target != name:
                edges.add((name, target))
    return names, edges


def load_rules(path):
    if not path.exists():
        return set(), set()
    crates = set()
    edges = set()
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if "->" in line:
            source, _, target = line.partition("->")
            source = source.strip()
            target = target.strip()
            if not source or not target:
                raise ValueError(f"{path.name}:{lineno}: expected 'source -> target'")
            edges.add((source, target))
            crates.add(source)
            crates.add(target)
        else:
            crates.add(line)
    return crates, edges


def write_rules(path, names, edges):
    lines = sorted(names - {source for source, _ in edges} - {target for _, target in edges})
    lines += [f"{source} -> {target}" for source, target in sorted(edges)]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")


def check(metadata, rules_path=RULES):
    names, edges = build_graph(metadata)
    allowed_crates, allowed_edges = load_rules(rules_path)
    errors = []
    # M4a is a hard architectural boundary, not an allowlist that --update can
    # accidentally bless. Test-only producer/host fixtures are permitted.
    forbidden = {"vosu-core", "vosu-osu", "vosu-osu-state", "vosu-osu-reader", "vosu-sys"}
    # M5 R0: the published SDK owns the platform-neutral memory vocabulary
    # below the native boundary, and the reducer reads it with no native edge.
    sdk_forbidden = {"vosu-core", "vosu-net", "vosu-surface", "vosu-sys"}
    packages = workspace_packages(metadata)
    # AF.9 (S0): Cargo has no unpublished features, so the published SDK
    # carries no feature and no workspace crate (not even a dev-dependency:
    # its packaged tests must build from the .crate alone), and everything
    # workspace-internal lives in the never-published vosu-plugin-internal.
    sdk = packages.get("vosu-plugin-api")
    if sdk is not None:
        if sdk.get("features"):
            errors.append(
                "AF.9: vosu-plugin-api declares Cargo features "
                f"{sorted(sdk['features'])}; Cargo has no unpublished features"
            )
        if sdk.get("publish") == []:
            errors.append("AF.9: vosu-plugin-api is the published SDK; it cannot be publish = false")
    internal = packages.get("vosu-plugin-internal")
    if internal is not None and internal.get("publish") != []:
        errors.append("AF.9: vosu-plugin-internal must stay publish = false")
    for source, package in packages.items():
        for dependency in package["dependencies"]:
            target = dependency["name"]
            if "path" not in dependency or target not in names:
                continue
            if source == "vosu-plugin-api":
                errors.append(
                    f"AF.9: SDK edge vosu-plugin-api -> {target} needs a workspace crate the "
                    "published package cannot carry"
                )
            if (
                target == "vosu-plugin-internal"
                and dependency.get("kind") != "dev"
                and package.get("publish") != []
            ):
                errors.append(
                    f"AF.9: publishable {source} depends on the unpublished vosu-plugin-internal"
                )
            if source == "vosu-osu" and target == "vosu-surface":
                errors.append("M4a: vosu-osu -> vosu-surface revives the removed snapshot bridge")
            if dependency.get("kind") == "dev":
                continue
            # M7 C0: the one other edge is the shared legacy filter value type
            # (`vosu_surface::filter::Filter`) core's latest-only transport stores.
            if source == "vosu-adapter-support" and target not in ("vosu-osu-model", "vosu-surface"):
                errors.append(f"M4a: wire support cannot depend on {target}")
            elif source.startswith("vosu-adapter-") and target in forbidden:
                errors.append(f"M4a: production adapter edge {source} -> {target} is forbidden")
            if source == "vosu-plugin-api" and target in sdk_forbidden:
                errors.append(
                    f"M5: SDK edge vosu-plugin-api -> {target} puts native code below the SDK"
                )
            if source in {"vosu-osu-reader", "vosu-osu-state"} and target == "vosu-sys":
                # Normal, build and optional (feature-gated) edges all land
                # here; only a dev-dependency is skipped above.
                errors.append(
                    f"M5: {source} -> vosu-sys revives a removed native edge; route reads/scans "
                    "through the plugin ABI and publication through vosu-net (R0/R1)"
                )
    for name in sorted(names - allowed_crates):
        errors.append(
            f"{rules_path.name}: workspace crate {name!r} is not listed; add it (with its "
            "allowed dependencies, if any) in the same change that introduces it"
        )
    for source, target in sorted(edges - allowed_edges):
        errors.append(
            f"{rules_path.name}: new crate edge {source} -> {target} is not recorded; add it "
            "deliberately alongside docs/v2/implementation-plan.md section 2 and "
            "docs/architecture/source-map.md"
        )
    for source, target in sorted(allowed_edges - edges):
        errors.append(
            f"{rules_path.name}: {source} -> {target} is no longer a real Cargo dependency; "
            "remove the stale rule"
        )
    for name in sorted(allowed_crates - names):
        errors.append(f"{rules_path.name}: {name!r} is listed but is not a workspace crate; remove it")
    return errors, edges, names


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--update", action="store_true", help="rewrite the rules file to match the current graph"
    )
    args = parser.parse_args()
    metadata = cargo_metadata()
    if args.update:
        names, edges = build_graph(metadata)
        write_rules(RULES, names, edges)
        print(f"Recorded {len(names)} crates and {len(edges)} crate edges")
        return 0
    errors, edges, names = check(metadata)
    app_version = workspace_packages(metadata)["vosu"]["version"]
    parity_path = ROOT / "tools/live-parity/live_parity.py"
    parity_version = runpy.run_path(str(parity_path))["PINNED_VOSU_VERSION"]
    if parity_version != app_version:
        errors.append(f"{parity_path.relative_to(ROOT)}: PINNED_VOSU_VERSION is {parity_version}, "
                      f"but Cargo.toml defines the vosu app version as {app_version}; update the pin")
    for error in errors:
        print(error, file=sys.stderr)
    print(f"Checked {len(names)} crates and {len(edges)} crate edges; {len(errors)} errors")
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
