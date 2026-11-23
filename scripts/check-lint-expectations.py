#!/usr/bin/env python3
"""Check production Rust code for unexplained clippy lint-expectation attributes.

`clippy.toml` disallows direct file writes, unbounded channels, and blocking
sleeps (`disallowed_methods`/`disallowed_types`, see AGENTS.md "Rules enforced
by lints and checks"). A site that needs an exception adds a per-site
`#[expect(clippy::disallowed_methods, reason = "...")]` (or `disallowed_types`).
Every such attribute in production code must match a
`scripts/lint-expectations.txt` entry: path, lint, a maximum count, a category,
and a one-line reason, ratcheting down exactly like `scripts/brand-allowlist.txt`
/ `scripts/check-brand.py`.

Test code is exempt the same way `clippy.toml`'s crate-level
`#![cfg_attr(test, expect(...))]` exempts it, so this check never counts:
  - a `#[cfg(test)]` module's attributes (by brace-matched span, from the
    nearest `#[cfg(test)]` to its `mod ... { }` block's closing brace);
  - a crate/module-wide `#![cfg_attr(test, expect(clippy::lint, ...))]`
    attribute, since it only applies to `cfg(test)` builds;
  - any file under a `tests/` or `benches/` directory, since every file
    there is itself test or benchmark code.
"""

import argparse
import fnmatch
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
ALLOWLIST = ROOT / "scripts" / "lint-expectations.txt"
LINTS = ("disallowed_methods", "disallowed_types")
CATEGORIES = {"bounded-poll", "disposable-io", "bounded-queue", "benchmark-helper"}

LINT_PATTERN = re.compile(r"clippy::(disallowed_methods|disallowed_types)\b")
CFG_ATTR_TEST = re.compile(r"cfg_attr\s*\(\s*test\s*,\s*expect\s*\(")
CFG_TEST_MOD = re.compile(r"#\[cfg\(test\)\]")
MOD_DECL = re.compile(r"\bmod\s+\w+")
ATTR_LOOKBACK = 8


def tracked_rust_files(root):
    result = subprocess.run(["git", "-C", str(root), "ls-files", "-z", "*.rs"], capture_output=True, check=True)
    return sorted(name for name in result.stdout.decode("utf-8").split("\0") if name)


def is_test_or_bench_dir(path):
    parts = Path(path).parts
    return "tests" in parts or "benches" in parts


def cfg_test_mod_spans(lines):
    """Line-index spans (inclusive, 0-based) covered by a `#[cfg(test)] mod ... { }` block."""
    spans = []
    i, n = 0, len(lines)
    while i < n:
        if CFG_TEST_MOD.search(lines[i]):
            j = i + 1
            while j < n and (lines[j].strip() == "" or lines[j].strip().startswith("#[")):
                j += 1
            if j < n and MOD_DECL.search(lines[j]):
                depth = 0
                started = False
                k = j
                while k < n:
                    depth += lines[k].count("{") - lines[k].count("}")
                    if "{" in lines[k]:
                        started = True
                    if started and depth <= 0:
                        spans.append((i, k))
                        break
                    k += 1
                i = k + 1
                continue
        i += 1
    return spans


def in_spans(idx, spans):
    return any(start <= idx <= end for start, end in spans)


def is_crate_test_gated(lines, idx):
    """True when the attribute containing line `idx` is `cfg_attr(test, expect(...))`."""
    window = " ".join(line.strip() for line in lines[max(0, idx - ATTR_LOOKBACK) : idx + 1])
    return bool(CFG_ATTR_TEST.search(window))


def production_counts(path, text):
    """Return {lint: count} of non-test-exempt expect attributes in one file's text."""
    counts = {}
    if is_test_or_bench_dir(path):
        return counts
    lines = text.splitlines()
    spans = cfg_test_mod_spans(lines)
    for idx, line in enumerate(lines):
        match = LINT_PATTERN.search(line)
        if not match:
            continue
        lint = match.group(1)
        if is_crate_test_gated(lines, idx):
            continue
        if in_spans(idx, spans):
            continue
        counts[lint] = counts.get(lint, 0) + 1
    return counts


def current_counts(root, tracked):
    counts = {}
    for path in tracked:
        try:
            text = (root / path).read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for lint, count in production_counts(path, text).items():
            if count:
                counts[(path, lint)] = count
    return counts


class AllowlistError(ValueError):
    pass


def load_allowlist(path):
    """Return an ordered list of (glob, lint, max_count, category, reason)."""
    if not path.exists():
        return []
    entries = []
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(" ", 4)
        if len(parts) != 5:
            raise AllowlistError(f"{path.name}:{lineno}: expected 'path lint max-count category reason'")
        glob, lint, count_text, category, reason = parts
        if lint not in LINTS:
            raise AllowlistError(f"{path.name}:{lineno}: lint {lint!r} is not one of {sorted(LINTS)}")
        if not count_text.isdigit():
            raise AllowlistError(f"{path.name}:{lineno}: max count {count_text!r} is not a non-negative integer")
        if category not in CATEGORIES:
            raise AllowlistError(f"{path.name}:{lineno}: category {category!r} is not one of {sorted(CATEGORIES)}")
        if not reason:
            raise AllowlistError(f"{path.name}:{lineno}: missing reason text")
        entries.append((glob, lint, int(count_text), category, reason))
    return entries


def matching_entry(entries, path, lint):
    for entry in entries:
        if entry[1] == lint and fnmatch.fnmatchcase(path, entry[0]):
            return entry
    return None


def check(root, allowlist_path, tracked=None):
    tracked = tracked_rust_files(root) if tracked is None else tracked
    entries = load_allowlist(allowlist_path)
    current = current_counts(root, tracked)
    errors = []
    for path, lint in sorted(current):
        count = current[(path, lint)]
        entry = matching_entry(entries, path, lint)
        if entry is None:
            errors.append(
                f"{path}: {count} unexplained clippy::{lint} expect attribute(s) in production code; "
                "add a scripts/lint-expectations.txt entry (path lint max-count category reason) or "
                "remove the exception"
            )
            continue
        if count > entry[2]:
            errors.append(
                f"{path}: {count} clippy::{lint} expect attribute(s) exceeds its "
                f"scripts/lint-expectations.txt maximum of {entry[2]} ({entry[3]}); review the new "
                "sites and, if still justified, raise the recorded maximum deliberately"
            )
    for glob, lint, max_count, _category, _reason in entries:
        matched = [path for (path, path_lint) in current if path_lint == lint and fnmatch.fnmatchcase(path, glob)]
        if not matched:
            errors.append(
                f"scripts/lint-expectations.txt: {glob!r} ({lint}) matches no tracked file with that "
                "expect attribute anymore; delete the stale entry (the allowlist only tightens)"
            )
            continue
        observed_max = max(current[(path, lint)] for path in matched)
        if observed_max < max_count:
            errors.append(
                f"scripts/lint-expectations.txt: {glob!r} ({lint}) allows up to {max_count} but the "
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
    print(f"Checked {len(current)} production clippy lint-expectation site(s); {len(errors)} errors")
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
