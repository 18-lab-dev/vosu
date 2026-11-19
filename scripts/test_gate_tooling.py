"""Verify prepush.py/native-ci.ps1 cover every ci.yml check, and land.py's refusals."""

import importlib.util
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
CI_YML = ROOT / ".github" / "workflows" / "ci.yml"

# Jobs whose `run:` steps are expected to run on an engineer's machine or in
# the vosu-windows-pro VM via scripts/prepush.py / scripts/native-ci.ps1.
GATED_JOBS = ("changes", "validation", "dashboard", "windows-native")

# Commands that only make sense on a hosted runner (classification needing
# GitHub event env, package installation, disk/cache bookkeeping, SDK cache
# priming consumed implicitly by the xwin clippy step itself) are not gate
# steps and are not expected to appear in prepush.py or native-ci.ps1.
RUNNER_ONLY = (
    re.compile(r"^python scripts/ci-changes\.py$"),
    re.compile(r'^echo "week=.*"\s*>>'),
    re.compile(r"^sudo apt-get"),
    re.compile(r"^cargo xwin cache xwin$"),
    re.compile(r"^df -h"),
    # Runner provisioning for the dashboard job; locally prepush.py takes an
    # installed Playwright module through --playwright-module instead.
    re.compile(r"^npm install "),
    re.compile(r"^npx --yes playwright@\S+ install "),
)

# Machine-specific arguments compared as placeholders on both sides.
PLACEHOLDERS = (
    (re.compile(r"--binary \S+"), "--binary <binary>"),
    (re.compile(r"--playwright-module \S+"), "--playwright-module <playwright>"),
)


def normalize(command):
    # Compare argv, not shell quoting: ci.yml quotes globs that prepush.py passes unquoted.
    command = " ".join(shlex.split(command))
    for pattern, replacement in PLACEHOLDERS:
        command = pattern.sub(replacement, command)
    return command


# Commands that are a documented strict subset of a broader local/native gate
# command (e.g. `cargo test --workspace --all-features --locked` already runs
# every target `--lib --bins --tests --examples` would, plus doctests).
SUBSET_OF = {
    "cargo test --workspace --lib --bins --tests --examples --all-features --locked": "cargo test --workspace --all-features --locked",
}


def _load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _job_blocks(text):
    """Split a workflow file's top-level `jobs:` entries by 2-space-indented key."""
    job_line = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$")
    blocks = {}
    current = None
    buf = []
    in_jobs = False
    for line in text.splitlines():
        if line.rstrip() == "jobs:":
            in_jobs = True
            continue
        if not in_jobs:
            continue
        match = job_line.match(line)
        if match:
            if current is not None:
                blocks[current] = buf
            current = match.group(1)
            buf = []
            continue
        if current is not None:
            buf.append(line)
    if current is not None:
        blocks[current] = buf
    return blocks


def _extract_run_commands(lines):
    commands = []
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()
        if stripped == "run: |":
            indent = len(line) - len(line.lstrip(" "))
            i += 1
            while i < len(lines) and (lines[i].strip() == "" or len(lines[i]) - len(lines[i].lstrip(" ")) > indent):
                content = lines[i].strip()
                if content:
                    commands.append(content)
                i += 1
            continue
        match = re.match(r"^\s*run:\s*(.+)$", line)
        if match:
            commands.append(match.group(1).strip())
        i += 1
    return commands


def gated_commands(ci_yml_text):
    """All `run:` commands from GATED_JOBS, in document order."""
    blocks = _job_blocks(ci_yml_text)
    commands = []
    for job in GATED_JOBS:
        commands.extend(_extract_run_commands(blocks.get(job, [])))
    return commands


def uncovered_commands(ci_yml_text, covered):
    """Commands from GATED_JOBS that neither match `covered` nor are runner-only."""
    missing = []
    for command in gated_commands(ci_yml_text):
        if any(pattern.search(command) for pattern in RUNNER_ONLY):
            continue
        normalized = normalize(SUBSET_OF.get(command, command))
        if normalized in covered:
            continue
        missing.append(command)
    return missing


def covered_commands():
    prepush = _load_module("prepush_gate_tooling_test", ROOT / "scripts" / "prepush.py")
    native_ci_text = (ROOT / "scripts" / "native-ci.ps1").read_text(encoding="utf-8")
    covered = set(prepush.COMMAND_MANIFEST.values())
    # native-ci.ps1's own "Covered CI commands" header lists its literal
    # cargo invocations; cross-check those against the manifest too so the
    # PowerShell file cannot silently drift from scripts/prepush.py.
    for match in re.finditer(r"^#\s{3}(cargo .+)$", native_ci_text, re.MULTILINE):
        covered.add(match.group(1).strip())
    return covered


class CiDriftTest(unittest.TestCase):
    def test_every_gated_ci_command_is_covered(self):
        text = CI_YML.read_text(encoding="utf-8")
        missing = uncovered_commands(text, covered_commands())
        self.assertEqual(
            missing,
            [],
            "ci.yml has run: command(s) not covered by scripts/prepush.py or "
            "scripts/native-ci.ps1 (update one of them, or add a RUNNER_ONLY/"
            "SUBSET_OF entry here with justification): " + "; ".join(missing),
        )

    def test_drift_test_catches_an_added_fake_step(self):
        text = CI_YML.read_text(encoding="utf-8")
        tampered = text.replace(
            "      - name: Check formatting before native setup\n        run: cargo fmt --all -- --check\n",
            "      - name: Check formatting before native setup\n        run: cargo fmt --all -- --check\n"
            "      - name: Fake uncovered step\n        run: cargo foo --not-a-real-check\n",
            1,
        )
        self.assertNotEqual(tampered, text, "fixture replacement did not match ci.yml; update the anchor line")
        missing = uncovered_commands(tampered, covered_commands())
        self.assertIn("cargo foo --not-a-real-check", missing)


class FakeCompleted:
    def __init__(self, returncode=0, stdout="", stderr=""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class LandRefusalTest(unittest.TestCase):
    def setUp(self):
        self.land = _load_module("land_gate_tooling_test", ROOT / "scripts" / "land.py")
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        self._git("init", "-q", "-b", "main")
        self._git("config", "user.email", "test@example.com")
        self._git("config", "user.name", "Test")
        (self.repo / ".gitignore").write_text("/target/\n", encoding="utf-8")
        (self.repo / "README.md").write_text("hello\n", encoding="utf-8")
        self._git("add", "README.md", ".gitignore")
        self._git("commit", "-q", "-m", "init")
        self.sha = self._git("rev-parse", "HEAD")
        # Simulate origin/main pointing at the same commit via a bare remote.
        self.remote = Path(self.tmp.name + "-remote")
        subprocess.run(["git", "init", "-q", "--bare", str(self.remote)], check=True)
        self._git("remote", "add", "origin", str(self.remote))
        self._git("push", "-q", "origin", "main")
        self.evidence_dir = self.repo / "target" / "prepush"
        self.evidence_dir.mkdir(parents=True)

    def tearDown(self):
        self.tmp.cleanup()
        subprocess.run(["rm", "-rf", str(self.remote)], check=False)

    def _git(self, *args):
        completed = subprocess.run(["git", *args], cwd=self.repo, capture_output=True, text=True)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        return completed.stdout.strip()

    def _write_evidence(self, sha, steps, file_sha=None):
        full_steps = []
        for name in self.land.EXPECTED_STEPS:
            full_steps.append(steps.get(name, {"name": name, "required": True, "status": "passed"}))
        (self.evidence_dir / f"{file_sha or sha}.json").write_text(
            json.dumps({"sha": sha, "steps": full_steps}), encoding="utf-8"
        )

    def test_stale_evidence_sha_is_refused(self):
        self._write_evidence("deadbeef" * 5, {}, file_sha=self.sha)
        with self.assertRaisesRegex(self.land.GateRefused, "does not match HEAD"):
            self.land.plan(None, cwd=self.repo, evidence_dir=self.evidence_dir)

    def test_skipped_required_step_is_refused(self):
        self._write_evidence(self.sha, {"fmt": {"name": "fmt", "required": True, "status": "skipped"}})
        with self.assertRaisesRegex(self.land.GateRefused, "not passed in evidence"):
            self.land.plan(None, cwd=self.repo, evidence_dir=self.evidence_dir)

    def test_missing_step_is_refused(self):
        full_steps = [
            {"name": name, "required": True, "status": "passed"}
            for name in sorted(self.land.EXPECTED_STEPS)
            if name != "fmt"
        ]
        (self.evidence_dir / f"{self.sha}.json").write_text(
            json.dumps({"sha": self.sha, "steps": full_steps}), encoding="utf-8"
        )
        with self.assertRaisesRegex(self.land.GateRefused, "missing step"):
            self.land.plan(None, cwd=self.repo, evidence_dir=self.evidence_dir)

    def test_not_applicable_optional_step_is_accepted(self):
        self._write_evidence(self.sha, {"actionlint": {"name": "actionlint", "required": False, "status": "not_applicable"}})
        sha, main_sha, branch = self.land.plan(None, cwd=self.repo, evidence_dir=self.evidence_dir)
        self.assertEqual(sha, self.sha)
        self.assertEqual(main_sha, self.sha)
        self.assertEqual(branch, "ci/" + self.sha[:12])

    def test_non_fast_forward_of_main_is_refused(self):
        self._write_evidence(self.sha, {})
        # Advance origin/main past HEAD from a second clone so this repo's
        # HEAD is no longer a fast-forward ancestor.
        other = self.repo.parent / (self.repo.name + "-other")
        subprocess.run(["git", "clone", "-q", str(self.remote), str(other)], check=True)
        subprocess.run(["git", "checkout", "-q", "-B", "main", "origin/main"], cwd=other, check=True)
        (other / "two.txt").write_text("two\n", encoding="utf-8")
        subprocess.run(["git", "add", "two.txt"], cwd=other, check=True)
        subprocess.run(["git", "-c", "user.email=t@e.com", "-c", "user.name=T", "commit", "-q", "-m", "two"], cwd=other, check=True)
        subprocess.run(["git", "push", "-q", "origin", "main"], cwd=other, check=True)
        with self.assertRaisesRegex(self.land.GateRefused, "not a fast-forward"):
            self.land.plan(None, cwd=self.repo, evidence_dir=self.evidence_dir)

    def test_dry_run_does_not_push_or_call_gh(self):
        self._write_evidence(self.sha, {})
        calls = []
        original = self.land.RUN

        def fake_run(cmd, *args, **kwargs):
            calls.append(cmd)
            return original(cmd, *args, **kwargs)

        self.land.RUN = fake_run
        try:
            code = self.land.main(["--dry-run"], cwd=self.repo, evidence_dir=self.evidence_dir)
        finally:
            self.land.RUN = original
        self.assertEqual(code, 0)
        for cmd in calls:
            args_text = " ".join(cmd) if isinstance(cmd, list) else str(cmd)
            self.assertNotIn(" push ", " " + args_text + " ")
            self.assertNotRegex(args_text, r"\bgh\b")

    def _fake_gh(self, gh_result):
        calls = []
        original = self.land.RUN

        def fake_run(cmd, *args, **kwargs):
            if cmd[0] == "gh":
                calls.append(cmd)
                return gh_result
            return original(cmd, *args, **kwargs)

        self.land.RUN = fake_run
        self.addCleanup(setattr, self.land, "RUN", original)
        return calls

    def test_ci_run_is_read_from_origin_even_with_an_upstream_remote(self):
        self._git("remote", "set-url", "origin", "https://github.com/acme/real.git")
        self._git("remote", "add", "upstream", "https://github.com/someone/fork.git")
        calls = self._fake_gh(FakeCompleted(stdout='[{"databaseId": 7, "status": "queued", "conclusion": ""}]'))
        self.assertEqual(self.land.find_ci_run(self.sha, timeout=0, interval=0, cwd=self.repo), 7)
        self.assertIn(["-R", "acme/real"], [cmd[3:5] for cmd in calls])

    def test_gh_failure_is_refused_immediately_instead_of_polling(self):
        self._git("remote", "set-url", "origin", "https://github.com/acme/real.git")
        calls = self._fake_gh(FakeCompleted(returncode=1, stderr="HTTP 404: Not Found"))
        with self.assertRaisesRegex(self.land.GateRefused, "404"):
            self.land.find_ci_run(self.sha, timeout=3600, interval=3600, cwd=self.repo)
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()
