"""Exercise measure-build.py's pure helpers (no cargo invocations)."""

import importlib.util
from pathlib import Path
import unittest


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


measure_build = _load("measure_build", "measure-build.py")


class TasksetPrefixTests(unittest.TestCase):
    def test_empty_cpu_list_disables_pinning(self):
        self.assertEqual(measure_build.taskset_prefix(""), [])

    def test_nonempty_cpu_list_builds_taskset_prefix(self):
        self.assertEqual(
            measure_build.taskset_prefix("0-7,16-23"),
            ["taskset", "--cpu-list", "0-7,16-23"],
        )


class MedianRangeTests(unittest.TestCase):
    def test_median_and_range_of_odd_sample_count(self):
        median, low, high = measure_build.median_range([3.0, 1.0, 2.0, 5.0, 4.0])
        self.assertEqual((median, low, high), (3.0, 1.0, 5.0))

    def test_single_sample(self):
        self.assertEqual(measure_build.median_range([2.5]), (2.5, 2.5, 2.5))


class ProbesTableTests(unittest.TestCase):
    def test_every_probe_has_a_unique_name_and_an_owning_crate(self):
        names = [probe[0] for probe in measure_build.PROBES]
        self.assertEqual(len(names), len(set(names)))
        for _name, relative_path, crate in measure_build.PROBES:
            self.assertTrue(relative_path)
            self.assertTrue(crate)


if __name__ == "__main__":
    unittest.main()
