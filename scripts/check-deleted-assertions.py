#!/usr/bin/env python3
"""Flag test assertions removed by this change without acknowledgement.

Repeated review rounds have caught a fixer deleting a failing test's
assertion instead of fixing the cause. This diffs HEAD against the merge-base
with `origin/<base>` and lists every `assert!`/`assert_eq!`/`assert_ne!`/
`debug_assert*`/`prop_assert*` line removed from test code: a `#[cfg(test)]`
module (by brace-matched span, same heuristic as
`scripts/check-lint-expectations.py`), or a file under a `tests/` directory,
named `tests.rs`, or ending `_tests.rs`.

An assertion whose exact text (normalized for whitespace) was also added
somewhere else in the same diff is a move, not a deletion, and does not
count. A real removal fails the check unless some commit in the range carries
an `Assertions-Removed: <reason>` trailer, so the deletion is deliberate and
recorded rather than silent.
"""

import argparse
from collections import Counter
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]

ASSERTION_PATTERN = re.compile(r"\b(?:assert|debug_assert|prop_assert)(?:_eq|_ne)?!\s*\(")
TRAILER_PATTERN = re.compile(r"^Assertions-Removed:\s*\S", re.MULTILINE)

FILE_HEADER = re.compile(r"^diff --git a/(.*) b/(.*)$")
HUNK_HEADER = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
CFG_TEST_MOD = re.compile(r"#\[cfg\(test\)\]")
MOD_DECL = re.compile(r"\bmod\s+\w+")


def normalize(text):
    return " ".join(text.strip().split())


def is_test_path(path):
    parts = Path(path).parts
    name = Path(path).name
    return "tests" in parts or name == "tests.rs" or name.endswith("_tests.rs")


def cfg_test_mod_spans(lines):
    """0-based inclusive (start, end) line-index spans of `#[cfg(test)] mod ... { }` blocks."""
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


def test_line_numbers(path, content):
    """1-based line numbers of `path`'s `content` that count as test code."""
    lines = content.splitlines()
    if is_test_path(path):
        return set(range(1, len(lines) + 1))
    numbers = set()
    for start, end in cfg_test_mod_spans(lines):
        numbers.update(range(start + 1, end + 2))
    return numbers


def parse_diff(diff_text):
    """Return a list of {old_path, new_path, removed: [(line_no, text)], added: [(line_no, text)]}."""
    files = []
    current = None
    old_line = new_line = 0
    for line in diff_text.splitlines():
        file_match = FILE_HEADER.match(line)
        if file_match:
            current = {"old_path": file_match.group(1), "new_path": file_match.group(2), "removed": [], "added": []}
            files.append(current)
            continue
        hunk_match = HUNK_HEADER.match(line)
        if hunk_match:
            old_line = int(hunk_match.group(1))
            new_line = int(hunk_match.group(3))
            continue
        if current is None:
            continue
        if line.startswith("--- ") or line.startswith("+++ "):
            continue
        if line.startswith("-"):
            current["removed"].append((old_line, line[1:]))
            old_line += 1
        elif line.startswith("+"):
            current["added"].append((new_line, line[1:]))
            new_line += 1
        elif line.startswith(" "):
            old_line += 1
            new_line += 1
    return files


def flagged_removed_assertions(files, pre_contents):
    """Removed test-code assertion lines not matched by an identical addition elsewhere.

    `files` is `parse_diff()`'s output. `pre_contents` maps each entry's
    `old_path` to its file content at the merge-base (entries with no content,
    e.g. newly added files, contribute no removals).
    Returns a list of (path, old_line_no, text).
    """
    added_counter = Counter()
    for entry in files:
        for _line_no, text in entry["added"]:
            if ASSERTION_PATTERN.search(text):
                added_counter[normalize(text)] += 1

    removed = []
    for entry in files:
        content = pre_contents.get(entry["old_path"])
        if content is None:
            continue
        allowed = test_line_numbers(entry["old_path"], content)
        report_path = entry["new_path"] if entry["new_path"] != "/dev/null" else entry["old_path"]
        for line_no, text in entry["removed"]:
            if line_no not in allowed:
                continue
            if not ASSERTION_PATTERN.search(text):
                continue
            removed.append((report_path, line_no, normalize(text), text.strip()))

    flagged = []
    consumed = Counter()
    for path, line_no, normalized, text in removed:
        if consumed[normalized] < added_counter[normalized]:
            consumed[normalized] += 1
            continue
        flagged.append((path, line_no, text))
    return flagged


def has_removal_trailer(commit_messages):
    return any(TRAILER_PATTERN.search(message) for message in commit_messages)


def _git(args, cwd):
    completed = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, check=False)
    return completed.returncode, completed.stdout, completed.stderr


def collect(root, base):
    """Returns (flagged, trailer_found, error). `error` is set only when git plumbing failed."""
    code, _out, err = _git(["fetch", "origin", base], root)
    if code != 0:
        return [], False, f"could not fetch origin/{base}: {err.strip()}"
    code, merge_base, err = _git(["merge-base", f"origin/{base}", "HEAD"], root)
    merge_base = merge_base.strip()
    if code != 0 or not merge_base:
        return [], False, f"could not compute a merge-base with origin/{base}: {err.strip()}"
    code, diff_text, err = _git(["diff", "--no-color", "--no-renames", "-U0", merge_base, "HEAD", "--", "*.rs"], root)
    if code != 0:
        return [], False, f"git diff failed: {err.strip()}"

    files = parse_diff(diff_text)
    pre_contents = {}
    for entry in files:
        old_path = entry["old_path"]
        if old_path == "/dev/null" or old_path in pre_contents:
            continue
        code, content, _err = _git(["show", f"{merge_base}:{old_path}"], root)
        if code == 0:
            pre_contents[old_path] = content

    flagged = flagged_removed_assertions(files, pre_contents)

    code, log_text, err = _git(["log", f"{merge_base}..HEAD", "--format=%B%x00"], root)
    if code != 0:
        return flagged, False, f"git log failed: {err.strip()}"
    messages = [message for message in log_text.split("\x00") if message.strip()]
    return flagged, has_removal_trailer(messages), None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="main", help="Branch to diff HEAD against (default: main).")
    args = parser.parse_args()

    flagged, trailer_found, error = collect(ROOT, args.base)
    if error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    if not flagged:
        print("No test assertions were removed without an equivalent addition elsewhere in the diff.")
        return 0
    if trailer_found:
        print(f"{len(flagged)} test assertion(s) were removed, acknowledged by an Assertions-Removed: trailer:", file=sys.stderr)
        for path, line_no, text in flagged:
            print(f"  {path}:{line_no}: {text}", file=sys.stderr)
        return 0

    print(f"{len(flagged)} test assertion(s) were removed without being added elsewhere in the diff:", file=sys.stderr)
    for path, line_no, text in flagged:
        print(f"  {path}:{line_no}: {text}", file=sys.stderr)
    print(
        "If this removal is deliberate (the test itself is being retired or rewritten), add an "
        "'Assertions-Removed: <reason>' trailer to a commit message in this range; otherwise restore "
        "the assertion or fix the cause it was guarding.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
