#!/usr/bin/env python3
"""Enforce the recorded workspace-crate dependency graph (scripts/crate-boundaries.txt)
against `cargo metadata`. Every workspace crate must be listed (even with no
internal dependencies), and every path dependency between workspace crates
must be a recorded edge, so a new crate or a new cross-crate dependency is a
deliberate, reviewed change alongside
docs/v2/implementation-plan.md section 2 and docs/architecture/source-map.md.

Vendored, provenance-pinned trees (crates/vosu-pp/rosu-*-api) are out of
scope, matching scripts/check-file-length.py.
"""

import argparse
import json
from pathlib import Path
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
    for error in errors:
        print(error, file=sys.stderr)
    print(f"Checked {len(names)} crates and {len(edges)} crate edges; {len(errors)} errors")
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
