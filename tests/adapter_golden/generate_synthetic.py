#!/usr/bin/env python3
"""Regenerate the synthetic `adapter_golden` mechanics fixture.

`tests/fixtures/golden-synthetic/` gives the comparator nonzero coverage
before live captures exist (GOLDEN_CONTRACT.md §9). Every record in it is
vosu-generated, so it is written with `provenance="synthetic"`, lives
outside `tests/fixtures/upstream/`, and is never pilot or full evidence.

Steps:
  1. run the ignored Rust exporter `synthetic::export_synthetic_capture`,
     which records R0 replays of R0's corpus source as case sandwiches for
     `windows-stable` and a lazer-shaped `linux-lazer`;
  2. wrap each platform in a `vosu-golden-capture-v1` capture whose identity
     fields say "synthetic";
  3. write the candidate with G0's writer, `golden(..., provenance="synthetic")`;
  4. replace the committed tree with that candidate.

Run from any directory, after `source scripts/dev-env.sh` if wanted:

    python tests/adapter_golden/generate_synthetic.py
"""
from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "tools/live-parity"))

from live_parity import PINNED_SOURCE_REVISION, PINNED_VOSU_VERSION  # noqa: E402
import parity_golden  # noqa: E402

TARGET = REPO / "tests/fixtures/golden-synthetic"
# A synthetic private root per platform; nothing echoes it, so nothing is
# rewritten.
PLATFORMS = {
    "windows-stable": ("stable", {"slot": "songs", "private": "C:\\Synthetic\\osu!\\Songs\\"}),
    "linux-lazer": ("lazer", {"slot": "files", "private": "/synthetic/osu/files/"}),
}
SCENARIO = "synthetic-mechanics"
CLIENT_VERSION = "synthetic"
# Never a real daemon: these only keep the attested identities distinct.
SOURCE_SHA = hashlib.sha256(b"synthetic: no upstream daemon was run").hexdigest()
VOSU_SHA = hashlib.sha256(b"synthetic: R0 replay of the corpus source").hexdigest()
CLIENT_SHA = hashlib.sha256(b"synthetic: no client was run").hexdigest()


def export(directory: Path) -> dict:
    out = directory / "export.json"
    env = dict(os.environ, VOSU_GOLDEN_SYNTHETIC_EXPORT=str(out))
    subprocess.run(
        ["cargo", "test", "--locked", "--features", "test-harness", "--test", "adapter_golden",
         "--", "--ignored", "--exact", "synthetic::export_synthetic_capture"],
        cwd=REPO, env=env, check=True)
    return json.loads(out.read_text(encoding="utf-8"))


def capture(exported: dict, commit: str) -> dict:
    platform = exported["platform"]
    variant, root = PLATFORMS[platform]
    return {
        "format": "vosu-golden-capture-v1",
        "source_revision": PINNED_SOURCE_REVISION,
        "platform": platform,
        "scenario_set": SCENARIO,
        "authorization": {
            "reference": "synthetic-mechanics-no-capture", "platform": platform,
            "source_revision": PINNED_SOURCE_REVISION, "vosu_commit": commit,
            "client_version": CLIENT_VERSION, "scenario_set": SCENARIO,
        },
        "attestation": {
            "source": {"revision": PINNED_SOURCE_REVISION, "kind": "source", "sha256": SOURCE_SHA},
            "vosu": {"commit": commit, "version": PINNED_VOSU_VERSION, "profile": "debug",
                     "features": ["test-harness"], "target": "synthetic-replay",
                     "sha256": VOSU_SHA},
            "client": {"variant": variant, "version": CLIENT_VERSION, "sha256": CLIENT_SHA},
        },
        "roots": [root],
        "assets": exported["assets"],
        "expected_cases": [case["id"] for case in exported["cases"]],
        "cases": exported["cases"],
    }


def main() -> int:
    commit = subprocess.run(["git", "-C", str(REPO), "rev-parse", "HEAD"], check=True,
                            capture_output=True, text=True).stdout.strip()
    with tempfile.TemporaryDirectory() as tmp:
        directory = Path(tmp)
        paths = []
        for exported in export(directory)["platforms"]:
            path = directory / f"capture-{exported['platform']}.json"
            path.write_text(json.dumps(capture(exported, commit)), encoding="utf-8")
            paths.append(str(path))
        candidate = directory / "candidate"
        with contextlib.redirect_stdout(io.StringIO()):
            parity_golden.golden(paths, str(candidate), provenance="synthetic")
        if TARGET.exists():
            shutil.rmtree(TARGET)
        shutil.copytree(candidate, TARGET)
    print(f"wrote {TARGET.relative_to(REPO)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
