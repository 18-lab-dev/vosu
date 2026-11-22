"""Exercise ci-check-verified.py's already_verified lookup and its fail-safe
behavior on a hosted API hiccup."""

import importlib.util
import json
from pathlib import Path
import unittest
import unittest.mock


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ci_check_verified = _load("ci_check_verified", "ci-check-verified.py")


class FakeCompleted:
    def __init__(self, returncode=0, stdout="", stderr=""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


def _check_runs(*runs):
    return json.dumps({"check_runs": list(runs)})


class AlreadyVerifiedTests(unittest.TestCase):
    def test_finds_passing_check_from_a_different_run(self):
        stdout = _check_runs(
            {"status": "completed", "conclusion": "success", "html_url": "https://github.com/o/r/actions/runs/111/job/1"}
        )
        with unittest.mock.patch.object(ci_check_verified.subprocess, "run", return_value=FakeCompleted(stdout=stdout)):
            self.assertTrue(ci_check_verified.already_verified("o/r", "deadbeef", "222"))

    def test_ignores_its_own_run(self):
        stdout = _check_runs(
            {"status": "completed", "conclusion": "success", "html_url": "https://github.com/o/r/actions/runs/222/job/1"}
        )
        with unittest.mock.patch.object(ci_check_verified.subprocess, "run", return_value=FakeCompleted(stdout=stdout)):
            self.assertFalse(ci_check_verified.already_verified("o/r", "deadbeef", "222"))

    def test_ignores_incomplete_or_failed_runs(self):
        stdout = _check_runs(
            {"status": "in_progress", "conclusion": None, "html_url": "https://github.com/o/r/actions/runs/111/job/1"},
            {"status": "completed", "conclusion": "failure", "html_url": "https://github.com/o/r/actions/runs/112/job/1"},
        )
        with unittest.mock.patch.object(ci_check_verified.subprocess, "run", return_value=FakeCompleted(stdout=stdout)):
            self.assertFalse(ci_check_verified.already_verified("o/r", "deadbeef", "222"))

    def test_no_check_runs_is_not_verified(self):
        with unittest.mock.patch.object(
            ci_check_verified.subprocess, "run", return_value=FakeCompleted(stdout=_check_runs())
        ):
            self.assertFalse(ci_check_verified.already_verified("o/r", "deadbeef", "222"))

    def test_api_failure_fails_safe_to_not_verified(self):
        with unittest.mock.patch.object(
            ci_check_verified.subprocess, "run", return_value=FakeCompleted(returncode=1, stderr="rate limited")
        ):
            self.assertFalse(ci_check_verified.already_verified("o/r", "deadbeef", "222"))

    def test_unparseable_output_fails_safe_to_not_verified(self):
        with unittest.mock.patch.object(
            ci_check_verified.subprocess, "run", return_value=FakeCompleted(stdout="not json")
        ):
            self.assertFalse(ci_check_verified.already_verified("o/r", "deadbeef", "222"))


if __name__ == "__main__":
    unittest.main()
