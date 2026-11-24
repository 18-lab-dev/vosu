"""Exercise benchmark statistics, reservation failure, and interleaving without root."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest import mock


spec = importlib.util.spec_from_file_location("bench_compare", Path(__file__).with_name("bench-compare.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class StatisticsTests(unittest.TestCase):
    def test_median_of_medians_and_spread(self):
        result = bench.summarize([100, 102, 101, 99, 400])
        self.assertEqual(result["median_ns"], 101)
        self.assertEqual(result["spread_ns"], 301)
        self.assertLessEqual(result["bootstrap_95_ci_ns"][0], 101)
        self.assertGreaterEqual(result["bootstrap_95_ci_ns"][1], 101)

    def test_round_noise_larger_than_delta_is_inconclusive(self):
        rounds = {"old": [{"case": n} for n in [100, 110, 90]],
                  "new": [{"case": n} for n in [104, 114, 94]]}
        result = bench.report(rounds)["case"]["versus_first"]["new"]
        self.assertTrue(result["inconclusive"])
        self.assertTrue(result["spread_exceeds_delta"])
        self.assertEqual(result["delta_percent"], 4)

    def test_clear_difference_is_conclusive(self):
        rounds = {"old": [{"case": 100}] * 5, "new": [{"case": 120}] * 5}
        self.assertFalse(bench.report(rounds)["case"]["versus_first"]["new"]["inconclusive"])

    def test_missing_case_is_not_silently_compared(self):
        with self.assertRaisesRegex(ValueError, "IDs differ"):
            bench.report({"a": [{"x": 1}], "b": [{"y": 1}]})


class ReservationTests(unittest.TestCase):
    def test_cpu_ranges_and_real_sibling_topology(self):
        self.assertEqual(bench.cpu_set("0-2,7,23\n"), {0, 1, 2, 7, 23})
        with tempfile.TemporaryDirectory() as directory:
            topology = Path(directory)
            path = topology / "cpu7/topology/thread_siblings_list"
            path.parent.mkdir(parents=True)
            path.write_text("7,23\n")
            self.assertEqual(bench.physical_core(7, topology), {7, 23})

    def test_invalid_partition_fails_closed_and_releases_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "cgroup.subtree_control").write_text("cpuset cpu")
            (root / "cpuset.cpus.effective").write_text("0-31")
            (root / "cpuset.mems.effective").write_text("0")
            original_mkdir = Path.mkdir
            removed = []

            def mkdir(path):
                original_mkdir(path)
                # Fake kernel refuses to assign the exclusive CPUs.
                (path / "cpuset.cpus.effective").write_text("")

            def rmdir(path):
                removed.append(path)
                shutil.rmtree(path)

            with mock.patch.object(bench, "physical_core", return_value={7, 23}), \
                    mock.patch.object(Path, "mkdir", mkdir), \
                    mock.patch.object(Path, "rmdir", rmdir):
                with self.assertRaisesRegex(ValueError, "reservation failed"):
                    with bench.reserve_core(7, root):
                        self.fail("invalid partition must never run a benchmark")
            self.assertEqual(len(removed), 1)
            self.assertFalse(removed[0].exists())


class RunnerTests(unittest.TestCase):
    def test_interleaves_revisions_and_reads_fresh_criterion_homes(self):
        with tempfile.TemporaryDirectory() as directory:
            args = argparse.Namespace(output=Path(directory), rounds=3, filter="case",
                                      warmup=1, measurement=2, samples=10,
                                      timeout=15, cpu=7)
            calls = []

            def run(command, **kwargs):
                home = Path(kwargs["env"]["CRITERION_HOME"])
                calls.append((command[0], home, kwargs["timeout"]))
                estimate = home / "case/new/estimates.json"
                estimate.parent.mkdir(parents=True)
                estimate.write_text(json.dumps({"median": {"point_estimate": 42}}))

            with mock.patch.object(bench.subprocess, "run", side_effect=run):
                rounds = bench.run_rounds([("a", Path("/a")), ("b", Path("/b"))],
                                          args, Path("/reserved"), None)
            self.assertEqual([call[0] for call in calls], ["/a", "/b"] * 3)
            self.assertEqual(len({call[1] for call in calls}), 6)
            self.assertTrue(all(call[2] == 15 for call in calls))
            self.assertEqual(rounds["a"], [{"case": 42}] * 3)

    def test_empty_or_invalid_estimates_fail(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            with self.assertRaisesRegex(ValueError, "no Criterion estimates"):
                bench.load_medians(home)
            estimate = home / "case/new/estimates.json"
            estimate.parent.mkdir(parents=True)
            estimate.write_text('{"median":{"point_estimate":-1}}')
            with self.assertRaisesRegex(ValueError, "invalid median"):
                bench.load_medians(home)

    def test_child_enters_partition_pins_and_drops_privileges_in_order(self):
        with mock.patch.object(Path, "write_text") as write, \
                mock.patch.object(os, "sched_setaffinity") as affinity, \
                mock.patch.object(os, "setgroups") as groups, \
                mock.patch.object(os, "setgid") as gid, \
                mock.patch.object(os, "setuid") as uid:
            manager = mock.Mock()
            for name, call in [("enter", write), ("pin", affinity), ("groups", groups),
                               ("gid", gid), ("uid", uid)]:
                manager.attach_mock(call, name)
            bench.child_setup(Path("/reserved"), 7, (1000, 1001))()
            self.assertEqual([call[0] for call in manager.mock_calls],
                             ["enter", "pin", "groups", "gid", "uid"])
            affinity.assert_called_once_with(0, {7})
            uid.assert_called_once_with(1000)

    def test_nonexclusive_child_pins_only_the_requested_sibling(self):
        with mock.patch.object(Path, "write_text") as write, \
                mock.patch.object(os, "sched_setaffinity") as affinity, \
                mock.patch.object(os, "setgroups") as groups:
            bench.child_setup(None, 7, None)()
            affinity.assert_called_once_with(0, {7})
            write.assert_not_called()
            groups.assert_not_called()

    def test_no_reserve_cli_runs_unprivileged_and_marks_evidence_nonexclusive(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "evidence"
            argv = ["bench-compare.py", "--binary", f"old={bench.sys.executable}",
                    "--binary", f"new={bench.sys.executable}", "--filter", "case",
                    "--cpu", "7", "--rounds", "2", "--no-reserve",
                    "--output", str(output)]
            rounds = {"old": [{"case": 100}] * 2, "new": [{"case": 110}] * 2}
            with mock.patch.object(bench.sys, "argv", argv), \
                    mock.patch.object(os, "geteuid", return_value=1000), \
                    mock.patch.object(os, "chown") as chown, \
                    mock.patch.object(bench, "physical_core", return_value={7, 23}), \
                    mock.patch.object(bench, "reserve_core") as reserve, \
                    mock.patch.object(bench, "run_rounds", return_value=rounds) as run, \
                    mock.patch("builtins.print"):
                self.assertEqual(bench.main(), 0)
            reserve.assert_not_called()
            chown.assert_not_called()
            self.assertEqual(run.call_args.args[2:], (None, None))
            evidence = json.loads((output / "summary.json").read_text())
            self.assertFalse(evidence["exclusive"])
            self.assertEqual(evidence["physical_core"], [7, 23])
            self.assertEqual(evidence["reserved_core"], [])


if __name__ == "__main__":
    unittest.main()
