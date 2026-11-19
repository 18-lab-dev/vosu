#!/usr/bin/env python3
"""Check tracked files for unexplained old-brand (`tosu`) occurrences.

Every occurrence must sit in an allowlisted file or glob with a recorded
reason category from AGENTS.md (upstream, migration, parity, provenance,
history, third-party-catalog, fixture) and a maximum count that only ever ratchets
down; a dropped count or a vanished file makes the allowlist entry stale.
"""

import argparse
import fnmatch
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
ALLOWLIST = ROOT / "scripts/brand-allowlist.txt"
OLD_BRAND = re.compile(r"tosu", re.IGNORECASE)
CATEGORIES = {"upstream", "migration", "parity", "provenance", "history", "third-party-catalog", "fixture"}


def tracked_files(root):
    result = subprocess.run(["git", "-C", str(root), "ls-files", "-z"], capture_output=True, check=True)
    return [name for name in result.stdout.decode("utf-8").split("\0") if name]


def occurrences(root, path):
    try:
        text = (root / path).read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError):
        return None
    return len(OLD_BRAND.findall(text))


def current_counts(root, tracked):
    counts = {}
    for path in tracked:
        count = occurrences(root, path)
        if count:
            counts[path] = count
    return counts


class AllowlistError(ValueError):
    pass


def load_allowlist(path):
    """Return an ordered list of (glob, max_count, category, reason)."""
    if not path.exists():
        return []
    entries = []
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(" ", 3)
        if len(parts) != 4:
            raise AllowlistError(f"{path.name}:{lineno}: expected 'glob max-count category reason'")
        glob, count_text, category, reason = parts
        if not count_text.isdigit():
            raise AllowlistError(f"{path.name}:{lineno}: max count {count_text!r} is not a non-negative integer")
        if category not in CATEGORIES:
            raise AllowlistError(f"{path.name}:{lineno}: category {category!r} is not one of {sorted(CATEGORIES)}")
        if not reason:
            raise AllowlistError(f"{path.name}:{lineno}: missing reason text")
        entries.append((glob, int(count_text), category, reason))
    return entries


def matching_entry(entries, path):
    for entry in entries:
        if fnmatch.fnmatchcase(path, entry[0]):
            return entry
    return None


def check(root, allowlist_path, tracked=None):
    tracked = tracked_files(root) if tracked is None else tracked
    entries = load_allowlist(allowlist_path)
    # The allowlist names the files it explains; counting it would make every entry self-referential.
    excluded = allowlist_path.resolve().relative_to(root.resolve()).as_posix() if allowlist_path.resolve().is_relative_to(root.resolve()) else None
    current = current_counts(root, [path for path in tracked if path != excluded])
    errors = []
    for path in sorted(current):
        count = current[path]
        entry = matching_entry(entries, path)
        if entry is None:
            errors.append(
                f"{path}: {count} unexplained occurrence(s) of the old brand 'tosu'; this is a "
                "rename defect unless it is upstream, migration, parity, provenance, history, "
                "third-party-catalog, or fixture material, in which case add a scripts/brand-allowlist.txt entry"
            )
            continue
        if count > entry[1]:
            errors.append(
                f"{path}: {count} occurrences exceeds its scripts/brand-allowlist.txt maximum of "
                f"{entry[1]} ({entry[0]}); review the new occurrences and, if still explained, raise "
                "the recorded maximum deliberately"
            )
    for glob, max_count, _category, _reason in entries:
        matched_paths = [path for path in current if fnmatch.fnmatchcase(path, glob)]
        if not matched_paths:
            errors.append(
                f"scripts/brand-allowlist.txt: {glob!r} matches no tracked file with an old-brand "
                "occurrence anymore; delete the stale entry (the allowlist only tightens)"
            )
            continue
        observed_max = max(current[path] for path in matched_paths)
        if observed_max < max_count:
            errors.append(
                f"scripts/brand-allowlist.txt: {glob!r} allows up to {max_count} occurrences but the "
                f"highest current match has {observed_max}; lower the recorded maximum (the allowlist "
                "only tightens)"
            )
    return errors, current


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    try:
        errors, current = check(ROOT, ALLOWLIST)
    except AllowlistError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    for error in errors:
        print(error, file=sys.stderr)
    print(f"Checked {len(current)} tracked files with old-brand occurrences; {len(errors)} errors")
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
