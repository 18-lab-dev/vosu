#!/usr/bin/env python3
"""Land a prepush-gated commit on main via a ci/<topic> branch and hosted CI."""

import argparse
from pathlib import Path
import json
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
EVIDENCE_DIR = ROOT / "target" / "prepush"

# Every step name scripts/prepush.py is expected to record. A step missing
# from evidence entirely is treated the same as a failed required step: land
# refuses rather than trusting an incomplete or hand-edited evidence file.
EXPECTED_STEPS = {
    "fmt",
    "clippy-linux",
    "test-linux",
    "catalog-forge",
    "doc-links",
    "file-length",
    "brand",
    "module-boundaries",
    "license-notices",
    "asset-hashes",
    "script-tests",
    "actionlint",
    "xwin-clippy",
    "standalone-crates",
    "sdk-package",
    "dashboard",
    "native-clippy",
    "native-test-workspace",
    "native-test-doc",
}

# Single seam: tests monkeypatch land.RUN to avoid touching the network while
# still exercising the real git-based refusal logic below through sh().
RUN = subprocess.run


class GateRefused(Exception):
    """A precondition for landing was not met; the plan must not proceed."""


def sh(cmd, cwd=ROOT, check=True):
    completed = RUN(cmd, cwd=cwd, capture_output=True, text=True)
    if check and completed.returncode != 0:
        raise GateRefused(f"`{' '.join(cmd)}` failed: {completed.stderr.strip() or completed.stdout.strip()}")
    return completed.stdout.strip()


def ensure_clean_tree(cwd=ROOT):
    if sh(["git", "status", "--porcelain"], cwd=cwd):
        raise GateRefused("working tree is not clean")


def head_sha(cwd=ROOT):
    return sh(["git", "rev-parse", "HEAD"], cwd=cwd)


def load_evidence(sha, evidence_dir=EVIDENCE_DIR):
    path = evidence_dir / f"{sha}.json"
    if not path.exists():
        raise GateRefused(f"no prepush evidence at {path}; run scripts/prepush.py first")
    return json.loads(path.read_text(encoding="utf-8"))


def validate_evidence(evidence, sha):
    if evidence.get("sha") != sha:
        raise GateRefused(f"evidence sha {evidence.get('sha')} does not match HEAD {sha}")
    steps = {s["name"]: s for s in evidence.get("steps", [])}
    missing = EXPECTED_STEPS - steps.keys()
    if missing:
        raise GateRefused(f"evidence is missing step(s): {', '.join(sorted(missing))}")
    not_passed = sorted(
        name for name, step in steps.items() if step.get("required") and step.get("status") != "passed"
    )
    if not_passed:
        raise GateRefused(f"required step(s) not passed in evidence: {', '.join(not_passed)}")


def ensure_fast_forward_of_base(base, cwd=ROOT):
    sh(["git", "fetch", "origin", base], cwd=cwd)
    base_sha = sh(["git", "rev-parse", f"origin/{base}"], cwd=cwd)
    sha = head_sha(cwd=cwd)
    merge_base = sh(["git", "merge-base", base_sha, sha], cwd=cwd)
    if merge_base != base_sha:
        raise GateRefused(
            f"HEAD ({sha}) is not a fast-forward of origin/{base} ({base_sha}); "
            f"merge-base is {merge_base}, not origin/{base}"
        )
    return base_sha


def derive_topic(explicit, sha, cwd=ROOT):
    if explicit:
        return explicit
    branch = sh(["git", "rev-parse", "--abbrev-ref", "HEAD"], cwd=cwd, check=False)
    if branch and branch not in ("HEAD", "main"):
        slug = re.sub(r"[^a-zA-Z0-9._-]+", "-", branch).strip("-")
        if slug:
            return slug
    return sha[:12]


def push_ci_branch(sha, branch, cwd=ROOT):
    sh(["git", "push", "origin", f"{sha}:refs/heads/{branch}"], cwd=cwd)


def origin_repo(cwd=ROOT):
    """`owner/name` of the origin remote that land.py pushes to."""
    url = sh(["git", "remote", "get-url", "origin"], cwd=cwd)
    match = re.search(r"github\.com[:/]([^/]+/[^/]+?)(?:\.git)?/?$", url)
    if not match:
        raise GateRefused(f"origin {url!r} is not a GitHub repository")
    return match.group(1)


def find_ci_run(sha, timeout, interval, cwd=ROOT):
    # gh picks a repository from the remotes on its own (it prefers `upstream`),
    # so name origin explicitly: CI must be read where the ci/ branch was pushed.
    repo = origin_repo(cwd)
    deadline = time.monotonic() + timeout
    while True:
        out = sh(
            ["gh", "run", "list", "-R", repo, "--workflow", "ci.yml", "--commit", sha, "--json", "databaseId,status,conclusion", "--limit", "5"],
            cwd=cwd,
        )
        runs = json.loads(out) if out else []
        if runs:
            return runs[0]["databaseId"]
        if time.monotonic() >= deadline:
            raise GateRefused(f"no CI run for commit {sha} appeared on ci.yml in {repo} within {timeout}s")
        time.sleep(interval)


def watch_run(run_id, cwd=ROOT):
    completed = RUN(["gh", "run", "watch", "-R", origin_repo(cwd), str(run_id), "--exit-status"], cwd=cwd)
    if completed.returncode != 0:
        raise GateRefused(f"CI run {run_id} did not succeed (exit {completed.returncode})")


def fast_forward_base(sha, base, cwd=ROOT):
    sh(["git", "push", "origin", f"{sha}:refs/heads/{base}"], cwd=cwd)


def delete_ci_branch(branch, cwd=ROOT):
    sh(["git", "push", "origin", "--delete", branch], cwd=cwd, check=False)


def plan(topic_arg, base="main", cwd=ROOT, evidence_dir=EVIDENCE_DIR):
    """Validate every landing precondition and return (sha, base_sha, branch)."""
    ensure_clean_tree(cwd)
    sha = head_sha(cwd)
    evidence = load_evidence(sha, evidence_dir)
    validate_evidence(evidence, sha)
    base_sha = ensure_fast_forward_of_base(base, cwd)
    topic = derive_topic(topic_arg, sha, cwd)
    branch = f"ci/{topic}"
    return sha, base_sha, branch


def main(argv=None, cwd=ROOT, evidence_dir=EVIDENCE_DIR):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--topic", help="ci/<topic> branch name; default is the current branch or sha12")
    parser.add_argument("--base", default="main", help="branch to fast-forward onto landing (default: main)")
    parser.add_argument("--dry-run", action="store_true", help="print the plan without pushing or watching CI")
    parser.add_argument("--poll-timeout", type=int, default=1800, help="seconds to wait for the CI run to appear")
    parser.add_argument("--poll-interval", type=int, default=15, help="seconds between polls for the CI run")
    args = parser.parse_args(argv)

    try:
        sha, base_sha, branch = plan(args.topic, args.base, cwd=cwd, evidence_dir=evidence_dir)
    except GateRefused as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    print(f"HEAD:          {sha}")
    print(f"origin/{args.base}:   {base_sha}")
    print(f"push:          {sha} -> origin/{branch}")
    print("wait:          ci.yml 'CI required' success on that exact sha")
    print(f"fast-forward:  origin/{args.base} -> {sha}")
    print(f"cleanup:       delete origin/{branch}")

    if args.dry_run:
        print("\n--dry-run: no changes were made.")
        return 0

    push_ci_branch(sha, branch, cwd=cwd)
    try:
        run_id = find_ci_run(sha, args.poll_timeout, args.poll_interval, cwd=cwd)
        print(f"watching CI run {run_id} ...")
        watch_run(run_id, cwd=cwd)
    except GateRefused as error:
        print(f"error: {error}", file=sys.stderr)
        print(f"origin/{branch} was pushed but is not landed; inspect and delete it manually once resolved.", file=sys.stderr)
        return 1

    fast_forward_base(sha, args.base, cwd=cwd)
    delete_ci_branch(branch, cwd=cwd)
    print(f"\nlanded {sha} on {args.base}; deleted origin/{branch}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
