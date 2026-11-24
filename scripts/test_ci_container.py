"""Verify scripts/ci-container/run_tests.py stays matched to ci.yml's
`validation` job and fails closed when podman or cargo-nextest is missing."""

import importlib.util
from pathlib import Path
import re
import subprocess
import unittest
import unittest.mock

ROOT = Path(__file__).resolve().parent.parent
CI_YML = ROOT / ".github" / "workflows" / "ci.yml"
CONTAINER_DIR = ROOT / "scripts" / "ci-container"


def _load_run_tests():
    spec = importlib.util.spec_from_file_location(
        "ci_container_run_tests_test", CONTAINER_DIR / "run_tests.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


run_tests = _load_run_tests()


class NextestCommandParityTest(unittest.TestCase):
    def test_nextest_cmd_matches_ci_yml_validation_step(self):
        """ci.yml's "Test workspace..." run: line must stay the literal
        command run_tests.NEXTEST_CMD replays inside the container."""
        text = CI_YML.read_text(encoding="utf-8")
        match = re.search(
            r"Test workspace.*?\n\s*run:\s*(cargo nextest run[^\n]*)", text, re.DOTALL
        )
        self.assertIsNotNone(match, "ci.yml's validation nextest step was not found")
        self.assertEqual(match.group(1).strip(), " ".join(run_tests.NEXTEST_CMD))


class ImageTagTest(unittest.TestCase):
    def test_tag_changes_when_containerfile_content_changes(self):
        original = run_tests.CONTAINERFILE.read_bytes()
        tag_before = run_tests.image_tag()
        try:
            run_tests.CONTAINERFILE.write_bytes(original + b"\n# touched for test\n")
            tag_after = run_tests.image_tag()
        finally:
            run_tests.CONTAINERFILE.write_bytes(original)
        self.assertNotEqual(tag_before, tag_after)

    def test_tag_is_stable_for_unchanged_content(self):
        self.assertEqual(run_tests.image_tag(), run_tests.image_tag())


class UnavailableToolingTest(unittest.TestCase):
    def test_run_refuses_without_podman(self):
        with unittest.mock.patch.object(run_tests.shutil, "which", return_value=None):
            with self.assertRaisesRegex(RuntimeError, "podman"):
                run_tests.run()

    def test_run_refuses_without_cargo_nextest(self):
        def fake_which(name):
            return None if name == "cargo-nextest" else "/usr/bin/podman"

        with unittest.mock.patch.object(run_tests.shutil, "which", side_effect=fake_which):
            with self.assertRaisesRegex(RuntimeError, "cargo-nextest"):
                run_tests.run()

    def test_ensure_image_skips_build_when_already_tagged(self):
        with unittest.mock.patch.object(
            run_tests.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)
        ) as mock_run:
            wall, log = run_tests.ensure_image("podman", "localhost/vosu-ci-linux:deadbeef")
        self.assertEqual(wall, 0.0)
        self.assertEqual(log, "")
        mock_run.assert_called_once()
        self.assertEqual(mock_run.call_args.args[0][:2], ["podman", "image"])


if __name__ == "__main__":
    unittest.main()
