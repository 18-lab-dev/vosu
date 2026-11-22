"""Check whether a main push's exact SHA already passed "CI required" on a
ci/<topic> run.

`scripts/land.py` pushes a commit to `ci/<topic>`, waits for its "CI
required" check to pass, then fast-forwards `main` to that identical SHA.
Re-running the full lint/test suite on the resulting main push would just
repeat that already-recorded result, so `ci.yml` looks it up here and skips
redundant work when it finds it.
"""

import json
import os
import subprocess
import sys


def already_verified(repo, sha, run_id):
    result = subprocess.run(
        # `-f` fields default `gh api` to POST, which this list endpoint rejects.
        [
            "gh", "api", "--method", "GET", f"repos/{repo}/commits/{sha}/check-runs",
            "-f", "check_name=CI required", "-f", "per_page=100",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        # Hosted API hiccup (rate limit, outage, token scope): fail safe by
        # re-running the full gate rather than silently skipping it.
        print(f"CI-required lookup failed (exit {result.returncode}): {result.stderr.strip()}", file=sys.stderr)
        return False
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        print(f"CI-required lookup returned unparseable output: {exc}", file=sys.stderr)
        return False
    for run in payload.get("check_runs", []):
        if run.get("status") != "completed" or run.get("conclusion") != "success":
            continue
        # Exclude this very run: its own "CI required" job has not reported
        # yet, so it cannot be the prior verification we are looking for.
        if f"/actions/runs/{run_id}/" in run.get("html_url", ""):
            continue
        return True
    return False


def main():
    repo = os.environ["GITHUB_REPOSITORY"]
    sha = os.environ["GITHUB_SHA"]
    run_id = os.environ["GITHUB_RUN_ID"]
    verified = already_verified(repo, sha, run_id)
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"verified={str(verified).lower()}\n")
    print(f"Already verified via a prior ci/<topic> run: {verified}")


if __name__ == "__main__":
    main()
