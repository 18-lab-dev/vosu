#!/usr/bin/env python3
"""Enforce the 1,000-physical-line maintained-file limit from docs/architecture/source-map.md.

The limit only ever ratchets down: `--update` may lower a baselined count or
drop an entry that no longer exceeds the limit, never raise one or add a new
file without review.
"""

import argparse
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
BASELINE = ROOT / "scripts/file-length-baseline.txt"
LIMIT = 1000
EXTENSIONS = {".rs", ".py", ".js", ".mjs", ".css", ".ps1", ".sh", ".cs", ".html"}
# Provenance-pinned upstream forks and oracle evidence are not maintained source;
# they change only through their owning tooling, never a manual split.
EXCLUDED_PREFIXES = (
    "crates/vosu-pp/rosu-pp-api/",
    "crates/vosu-pp/rosu-map-api/",
    "tools/oracle/reference/",
)


def tracked_files(root):
    result = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], capture_output=True, check=True)
    return [name for name in result.stdout.decode("utf-8").split("\0") if name]


def candidate_paths(paths):
    for path in paths:
        if path.startswith(EXCLUDED_PREFIXES):
            continue
        if Path(path).suffix in EXTENSIONS:
            yield path


def line_count(root, path):
    with (root / path).open("rb") as handle:
        return sum(1 for _ in handle)


def load_baseline(path):
    if not path.exists():
        return {}
    entries = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            continue
        count_text, _, name = line.partition(" ")
        entries[name] = int(count_text)
    return entries


def write_baseline(path, entries):
    body = "".join(f"{count} {name}\n" for name, count in sorted(entries.items()))
    path.write_text(body, encoding="utf-8", newline="\n")


def current_counts(root, tracked):
    return {path: line_count(root, path) for path in candidate_paths(tracked)}


def check(root, baseline_path, tracked=None):
    """Return (errors, current_counts). An empty error list means the tree passes."""
    tracked = tracked_files(root) if tracked is None else tracked
    baseline = load_baseline(baseline_path)
    current = current_counts(root, tracked)
    errors = []
    for path in sorted(current):
        count = current[path]
        if count <= LIMIT:
            continue
        limit = baseline.get(path)
        if limit is None:
            errors.append(
                f"{path}: {count} lines exceeds the 1,000-line maintained-file limit "
                "(docs/architecture/source-map.md); split along the module boundaries in "
                "docs/architecture/source-map.md, or add a reviewed baseline entry with "
                "scripts/check-file-length.py --update"
            )
        elif count > limit:
            errors.append(
                f"{path}: {count} lines exceeds its baseline of {limit} (grew); split along the "
                "module boundaries in docs/architecture/source-map.md, then run "
                "scripts/check-file-length.py --update"
            )
    for path in sorted(baseline):
        if path not in current:
            errors.append(
                f"{path}: baselined in scripts/file-length-baseline.txt but no longer tracked or "
                "no longer a maintained-code file; delete its line (the ratchet only tightens)"
            )
        elif current[path] <= LIMIT:
            errors.append(
                f"{path}: now {current[path]} lines, at or below the 1,000-line limit; delete its "
                "stale entry from scripts/file-length-baseline.txt (the ratchet only tightens)"
            )
    return errors, current


def update(root, baseline_path, tracked=None):
    """Rewrite the baseline, refusing to raise a count or add an unreviewed entry."""
    tracked = tracked_files(root) if tracked is None else tracked
    baseline = load_baseline(baseline_path)
    current = current_counts(root, tracked)
    refusals = []
    new_baseline = {}
    for path, count in current.items():
        if count <= LIMIT:
            continue
        if path not in baseline:
            refusals.append(
                f"{path}: {count} lines has no existing baseline entry; --update cannot add new "
                "entries, split the file or add the line by hand after review"
            )
            continue
        if count > baseline[path]:
            refusals.append(
                f"{path}: current {count} lines exceeds its baseline of {baseline[path]}; --update "
                "cannot raise a baseline, split the file along the module boundaries in "
                "docs/architecture/source-map.md"
            )
            continue
        new_baseline[path] = count
    if refusals:
        return refusals, None
    write_baseline(baseline_path, new_baseline)
    return [], new_baseline


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--update", action="store_true", help="lower baseline counts and drop stale entries")
    args = parser.parse_args()
    if args.update:
        errors, result = update(ROOT, BASELINE)
        if errors:
            for error in errors:
                print(error, file=sys.stderr)
            return 1
        print(f"Refreshed baseline for {len(result)} files")
        return 0
    errors, current = check(ROOT, BASELINE)
    for error in errors:
        print(error, file=sys.stderr)
    over_limit = sum(1 for count in current.values() if count > LIMIT)
    print(f"Checked {len(current)} maintained code files; {over_limit} baselined over the 1,000-line limit; {len(errors)} errors")
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
