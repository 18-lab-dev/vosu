"""Exercise vm-stress.py's pure parts: cargo argument building for the
build-once/rerun-N-times shape, the invocation config, naming, and the
scheduled-task command text. None of these touch the VM; the end-to-end
behavior is proven separately against the live VM (see AGENTS.md's
Windows-sensitive bullet)."""

import importlib.util
import json
from pathlib import Path
import unittest


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


vm_stress = _load("vm_stress", "vm-stress.py")


class BuildCargoArgsBuildTests(unittest.TestCase):
    def test_appends_no_run(self):
        args = vm_stress.build_cargo_args_build("vosu-core", None, True)
        self.assertEqual(
            args,
            ["test", "--locked", "--all-features", "--package", "vosu-core", "--lib", "--no-run"],
        )

    def test_no_run_never_carries_a_filter(self):
        args = vm_stress.build_cargo_args_build(None, None, False)
        self.assertNotIn("--", args)
        self.assertEqual(args[-1], "--no-run")


class BuildTestArgsTests(unittest.TestCase):
    def test_filter_is_first_without_cargo_flags(self):
        self.assertEqual(vm_stress.build_test_args("plugin::", []), ["plugin::"])

    def test_extra_args_follow_the_filter(self):
        args = vm_stress.build_test_args("plugin::", ["--nocapture"])
        self.assertEqual(args, ["plugin::", "--nocapture"])

    def test_never_appends_no_run(self):
        args = vm_stress.build_test_args("plugin::", [])
        self.assertNotIn("--no-run", args)


class DirectExecutionTests(unittest.TestCase):
    def test_iteration_loop_cannot_reinvoke_cargo(self):
        runner = Path(__file__).with_name("vm-stress.ps1").read_text()
        loop = runner.split("for ($i = 1;", 1)[1].split("$perTestOut =", 1)[0]
        self.assertIn("-CargoExe $binary.executable", loop)
        self.assertNotIn("-CargoExe $cargo ", loop)
        self.assertIn("Split-Path -Parent $binary.manifest_path", loop)
        self.assertIn("'--message-format=json'", runner)
        self.assertIn("$artifact.profile.test", runner)


class InvocationConfigTests(unittest.TestCase):
    def test_includes_all_fields(self):
        config = vm_stress.build_invocation_config(
            "V:\\bt\\vms-abc-123", "deadbeefcafef00d",
            ["test", "--no-run"], ["plugin::"],
            has_patch=True, iterations=30, jobs=8,
            build_timeout_seconds=1800, iter_timeout_seconds=180,
        )
        self.assertEqual(config["WorkRoot"], "V:\\bt\\vms-abc-123")
        self.assertEqual(config["BundlePath"], "V:\\bt\\vms-abc-123\\vosu.bundle")
        self.assertEqual(config["PatchPath"], "V:\\bt\\vms-abc-123\\uncommitted.patch")
        self.assertEqual(config["ExpectedCommit"], "deadbeefcafef00d")
        self.assertEqual(config["CargoArgsBuild"], ["test", "--no-run"])
        self.assertEqual(config["TestArgs"], ["plugin::"])
        self.assertEqual(config["Iterations"], 30)
        self.assertEqual(config["Jobs"], 8)
        self.assertEqual(config["BuildTimeoutSeconds"], 1800)
        self.assertEqual(config["IterTimeoutSeconds"], 180)

    def test_empty_patch_path_when_no_uncommitted_changes(self):
        config = vm_stress.build_invocation_config(
            "V:\\bt\\vms-abc-123", "deadbeefcafef00d", ["test"], ["test"],
            has_patch=False, iterations=1, jobs=8,
            build_timeout_seconds=1800, iter_timeout_seconds=180,
        )
        self.assertEqual(config["PatchPath"], "")

    def test_round_trips_through_json(self):
        config = vm_stress.build_invocation_config(
            "V:\\bt\\vms-abc-123", "deadbeefcafef00d", ["test"], ["a::b::"],
            has_patch=False, iterations=30, jobs=8,
            build_timeout_seconds=1800, iter_timeout_seconds=180,
        )
        decoded = json.loads(json.dumps(config))
        self.assertEqual(decoded["TestArgs"], ["a::b::"])


class NamingTests(unittest.TestCase):
    def test_remote_work_root_is_under_bt_and_distinct_from_vm_test(self):
        a = vm_stress.remote_work_root("abc123def456", "f00dcafe")
        self.assertEqual(a, "V:\\bt\\vms-abc123def456-f00dcafe")
        self.assertTrue(a.startswith("V:\\bt\\vms-"))
        self.assertFalse(a.startswith("V:\\bt\\vmt-"))

    def test_task_name_is_unique_per_token(self):
        a = vm_stress.task_name("abc123def456", "f00dcafe")
        b = vm_stress.task_name("abc123def456", "deadbeef")
        self.assertEqual(a, "vmstress-abc123def456-f00dcafe")
        self.assertNotEqual(a, b)


class BuildRunCommandTests(unittest.TestCase):
    def test_references_the_staged_script_and_config(self):
        cmd = vm_stress.build_run_command("V:\\bt\\vms-abc-123")
        self.assertIn("-File V:\\bt\\vms-abc-123\\vm-stress.ps1", cmd)
        self.assertIn("-ConfigPath V:\\bt\\vms-abc-123\\invocation.json", cmd)

    def test_result_has_no_embedded_quotes(self):
        cmd = vm_stress.build_run_command("V:\\bt\\vms-abc-123")
        self.assertNotIn('"', cmd)

    def test_result_fits_the_schtasks_tr_length_cap(self):
        cmd = vm_stress.build_run_command(vm_stress.remote_work_root("0" * 12, "0" * 8))
        self.assertLessEqual(len(cmd), 261)


class BuildProgressScriptTests(unittest.TestCase):
    def test_embeds_progress_and_done_paths(self):
        script = vm_stress.build_progress_script("V:\\bt\\vms-abc-123")
        self.assertIn("V:\\bt\\vms-abc-123\\progress.json", script)
        self.assertIn("V:\\bt\\vms-abc-123\\done.json", script)


class ArgParsingTests(unittest.TestCase):
    def test_defaults(self):
        args = vm_stress.parse_args(["--", "plugin::"])
        self.assertIsNone(args.package)
        self.assertIsNone(args.test)
        self.assertFalse(args.lib)
        self.assertEqual(args.iterations, vm_stress.DEFAULT_ITERATIONS)
        self.assertEqual(args.jobs, vm_stress.DEFAULT_JOBS)
        self.assertEqual(args.build_timeout, vm_stress.DEFAULT_BUILD_TIMEOUT_SECONDS)
        self.assertEqual(args.iter_timeout, vm_stress.DEFAULT_ITER_TIMEOUT_SECONDS)
        self.assertEqual(args.extra, ["plugin::"])
        self.assertFalse(args.keep_remote)

    def test_overrides(self):
        args = vm_stress.parse_args(
            ["--package", "vosu-core", "--lib", "--iterations", "50", "--jobs", "4",
             "--build-timeout", "600", "--iter-timeout", "60", "--", "plugin::", "--nocapture"]
        )
        self.assertEqual(args.package, "vosu-core")
        self.assertTrue(args.lib)
        self.assertEqual(args.iterations, 50)
        self.assertEqual(args.jobs, 4)
        self.assertEqual(args.build_timeout, 600)
        self.assertEqual(args.iter_timeout, 60)
        self.assertEqual(args.extra, ["plugin::", "--nocapture"])

    def test_keep_remote_flag(self):
        args = vm_stress.parse_args(["--keep-remote", "--", "plugin::"])
        self.assertTrue(args.keep_remote)


if __name__ == "__main__":
    unittest.main()
