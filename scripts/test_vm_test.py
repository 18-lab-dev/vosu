"""Exercise vm-test.py's pure parts: cargo argument building, the
PowerShell/schtasks command and script text it constructs, and its cleanup
plan. None of these touch the VM; the end-to-end behavior is proven
separately against the live VM (see AGENTS.md's Windows-sensitive bullet)."""

import base64
import importlib.util
import json
from pathlib import Path
import unittest


def _load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


vm_test = _load("vm_test", "vm-test.py")


class BuildCargoTestArgsTests(unittest.TestCase):
    def test_defaults_to_workspace(self):
        args = vm_test.build_cargo_test_args(None, None, False, [])
        self.assertEqual(args, ["test", "--locked", "--all-features", "--workspace"])

    def test_package_replaces_workspace(self):
        args = vm_test.build_cargo_test_args("vosu-sys", None, False, [])
        self.assertEqual(args, ["test", "--locked", "--all-features", "--package", "vosu-sys"])
        self.assertNotIn("--workspace", args)

    def test_lib_flag(self):
        args = vm_test.build_cargo_test_args("vosu-core", None, True, [])
        self.assertIn("--lib", args)
        self.assertEqual(args[-1], "--lib")

    def test_test_binary_selector(self):
        args = vm_test.build_cargo_test_args(None, "process_memory", False, [])
        self.assertIn("--test", args)
        self.assertEqual(args[args.index("--test") + 1], "process_memory")

    def test_extra_args_follow_double_dash(self):
        args = vm_test.build_cargo_test_args("vosu-core", None, True, ["config::tests::", "--nocapture"])
        self.assertEqual(args[-3:], ["--", "config::tests::", "--nocapture"])

    def test_no_extra_args_omits_double_dash(self):
        args = vm_test.build_cargo_test_args(None, None, False, [])
        self.assertNotIn("--", args)

    def test_combines_package_lib_test_and_extra(self):
        args = vm_test.build_cargo_test_args("vosu-sys", "windows_platform", True, ["--ignored"])
        self.assertEqual(
            args,
            [
                "test", "--locked", "--all-features",
                "--package", "vosu-sys",
                "--lib",
                "--test", "windows_platform",
                "--", "--ignored",
            ],
        )


class InvocationConfigTests(unittest.TestCase):
    def test_includes_all_fields(self):
        config = vm_test.build_invocation_config(
            "V:\\bt\\vmt-abc-123", "deadbeefcafef00d", ["test", "--workspace"], has_patch=True, jobs=8, timeout_seconds=1200
        )
        self.assertEqual(config["WorkRoot"], "V:\\bt\\vmt-abc-123")
        self.assertEqual(config["BundlePath"], "V:\\bt\\vmt-abc-123\\vosu.bundle")
        self.assertEqual(config["PatchPath"], "V:\\bt\\vmt-abc-123\\uncommitted.patch")
        self.assertEqual(config["ExpectedCommit"], "deadbeefcafef00d")
        self.assertEqual(config["CargoArgs"], ["test", "--workspace"])
        self.assertEqual(config["Jobs"], 8)
        self.assertEqual(config["TimeoutSeconds"], 1200)

    def test_empty_patch_path_when_no_uncommitted_changes(self):
        config = vm_test.build_invocation_config(
            "V:\\bt\\vmt-abc-123", "deadbeefcafef00d", ["test"], has_patch=False, jobs=8, timeout_seconds=1200
        )
        self.assertEqual(config["PatchPath"], "")

    def test_round_trips_through_json_with_long_or_quoted_filters(self):
        # The whole point of a config file over a /TR command line: no
        # length cap (schtasks caps /TR at 261 chars) and no quoting hazard
        # for filters containing spaces or quotes.
        extra_filter = 'a "quoted" filter with spaces ' * 20
        config = vm_test.build_invocation_config(
            "V:\\bt\\vmt-abc-123", "deadbeefcafef00d", ["test", "--", extra_filter], has_patch=False, jobs=8, timeout_seconds=1200
        )
        decoded = json.loads(json.dumps(config))
        self.assertEqual(decoded["CargoArgs"], ["test", "--", extra_filter])

    def test_plugin_host_fixtures_default_off_and_round_trip(self):
        config = vm_test.build_invocation_config(
            "V:\\bt\\vmt-abc-123", "deadbeefcafef00d", ["test"], has_patch=False, jobs=8, timeout_seconds=1200
        )
        self.assertIs(config["PluginHostFixtures"], False)
        config = vm_test.build_invocation_config(
            "V:\\bt\\vmt-abc-123", "deadbeefcafef00d", ["test"], has_patch=False, jobs=8, timeout_seconds=1200,
            plugin_host_fixtures=True,
        )
        self.assertIs(json.loads(json.dumps(config))["PluginHostFixtures"], True)

    def test_release_daemon_default_off_and_implies_the_fixtures(self):
        config = vm_test.build_invocation_config(
            "V:\\bt\\vmt-abc-123", "deadbeefcafef00d", ["test"], has_patch=False, jobs=8, timeout_seconds=1200
        )
        self.assertIs(config["ReleaseDaemon"], False)
        config = vm_test.build_invocation_config(
            "V:\\bt\\vmt-abc-123", "deadbeefcafef00d", ["test"], has_patch=False, jobs=8, timeout_seconds=1200,
            release_daemon=True,
        )
        decoded = json.loads(json.dumps(config))
        self.assertIs(decoded["ReleaseDaemon"], True)
        self.assertIs(decoded["PluginHostFixtures"], True)
        runner = Path(__file__).with_name("vm-test.ps1").read_text(encoding="utf-8")
        self.assertIn("-ReleaseDaemon:([bool]$config.ReleaseDaemon)", runner)


class PluginHostFixtureSelectionTests(unittest.TestCase):
    def test_selections_that_can_run_the_root_native_target_prepare_fixtures(self):
        for package, test in [(None, None), (None, "plugin_host_native"), ("vosu", None), ("vosu", "plugin_host_native")]:
            self.assertTrue(vm_test.needs_plugin_host_fixtures(package, test, False), (package, test))

    def test_other_selections_do_not(self):
        for package, test, lib in [
            ("vosu-sys", None, True),
            (None, None, True),
            ("vosu-core", None, False),
            (None, "process_memory", False),
        ]:
            self.assertFalse(vm_test.needs_plugin_host_fixtures(package, test, lib), (package, test, lib))

    def test_runner_prepares_fixtures_before_the_test_step(self):
        runner = Path(__file__).with_name("vm-test.ps1").read_text(encoding="utf-8")
        prepare = runner.index("Invoke-VosuPluginHostFixtures")
        self.assertLess(prepare, runner.index("-Name 'cargo-test'"))
        self.assertIn("$config.PluginHostFixtures", runner)


class NamingTests(unittest.TestCase):
    def test_remote_work_root_is_under_bt_and_unique_per_token(self):
        a = vm_test.remote_work_root("abc123def456", "f00dcafe")
        b = vm_test.remote_work_root("abc123def456", "deadbeef")
        self.assertEqual(a, "V:\\bt\\vmt-abc123def456-f00dcafe")
        self.assertNotEqual(a, b)
        self.assertTrue(a.startswith("V:\\bt\\vmt-"))

    def test_task_name_is_unique_per_token(self):
        a = vm_test.task_name("abc123def456", "f00dcafe")
        b = vm_test.task_name("abc123def456", "deadbeef")
        self.assertEqual(a, "vmtest-abc123def456-f00dcafe")
        self.assertNotEqual(a, b)


class BuildRunCommandTests(unittest.TestCase):
    def test_references_the_staged_script_and_config(self):
        cmd = vm_test.build_run_command("V:\\bt\\vmt-abc-123")
        self.assertIn("-File V:\\bt\\vmt-abc-123\\vm-test.ps1", cmd)
        self.assertIn("-ConfigPath V:\\bt\\vmt-abc-123\\invocation.json", cmd)

    def test_result_has_no_embedded_quotes(self):
        cmd = vm_test.build_run_command("V:\\bt\\vmt-abc-123")
        self.assertNotIn('"', cmd)

    def test_result_fits_the_schtasks_tr_length_cap(self):
        # schtasks /Create rejects a /TR value over 261 characters; this is
        # exactly why the per-run parameters moved to invocation.json.
        cmd = vm_test.build_run_command(vm_test.remote_work_root("0" * 12, "0" * 8))
        self.assertLessEqual(len(cmd), 261)


class BuildCreateAndRunScriptTests(unittest.TestCase):
    def test_creates_then_runs_the_named_task(self):
        script = vm_test.build_create_and_run_script("vmtest-abc-123", "powershell.exe -File x")
        self.assertIn('schtasks /Create /TN "vmtest-abc-123"', script)
        self.assertIn("/SC ONCE", script)
        self.assertIn("/F", script)
        self.assertIn('schtasks /Run /TN "vmtest-abc-123"', script)
        self.assertLess(script.index("/Create"), script.index("/Run"))

    def test_checks_lastexitcode_after_each_schtasks_call(self):
        script = vm_test.build_create_and_run_script("t", "cmd")
        self.assertEqual(script.count("$LASTEXITCODE"), 2)

    def test_embeds_the_exact_run_command(self):
        run_command = "powershell.exe -NoProfile -File V:\\bt\\x\\vm-test.ps1 -Jobs 8"
        script = vm_test.build_create_and_run_script("t", run_command)
        self.assertIn(run_command, script)

    def test_rejects_a_run_command_with_embedded_quotes(self):
        with self.assertRaises(AssertionError):
            vm_test.build_create_and_run_script("t", 'powershell.exe -File "has a quote"')


class BuildTailScriptTests(unittest.TestCase):
    def test_embeds_remote_paths_and_offsets(self):
        script = vm_test.build_tail_script("V:\\bt\\vmt-abc-123", 42, 7)
        self.assertIn("V:\\bt\\vmt-abc-123\\logs\\cargo-test.stdout.log", script)
        self.assertIn("V:\\bt\\vmt-abc-123\\logs\\cargo-test.stderr.log", script)
        self.assertIn("V:\\bt\\vmt-abc-123\\done.json", script)
        self.assertIn("Read-Tail 'V:\\bt\\vmt-abc-123\\logs\\cargo-test.stdout.log' 42", script)
        self.assertIn("Read-Tail 'V:\\bt\\vmt-abc-123\\logs\\cargo-test.stderr.log' 7", script)

    def test_zero_offsets_are_valid(self):
        script = vm_test.build_tail_script("V:\\bt\\vmt-x", 0, 0)
        self.assertIn("Read-Tail 'V:\\bt\\vmt-x\\logs\\cargo-test.stdout.log' 0", script)


class CleanupTests(unittest.TestCase):
    def test_cleanup_script_ends_task_before_deleting_it_before_removing_dir(self):
        script = vm_test.build_cleanup_script("vmtest-abc-123", "V:\\bt\\vmt-abc-123")
        end_index = script.index("/End")
        delete_index = script.index("/Delete")
        remove_index = script.index("Remove-Item")
        self.assertLess(end_index, delete_index)
        self.assertLess(delete_index, remove_index)
        self.assertIn('"vmtest-abc-123"', script)
        self.assertIn('"V:\\bt\\vmt-abc-123"', script)

    def test_cleanup_script_is_best_effort(self):
        script = vm_test.build_cleanup_script("t", "V:\\bt\\d")
        self.assertIn("-ErrorAction SilentlyContinue", script)
        self.assertIn("2>$null", script)

    def test_plan_cleanup_is_ordered_end_delete_remove(self):
        plan = vm_test.plan_cleanup("vmtest-abc-123", "V:\\bt\\vmt-abc-123")
        self.assertEqual(
            plan,
            [
                ("end_task", "vmtest-abc-123"),
                ("delete_task", "vmtest-abc-123"),
                ("remove_dir", "V:\\bt\\vmt-abc-123"),
            ],
        )


class ArgParsingTests(unittest.TestCase):
    def test_defaults(self):
        args = vm_test.parse_args([])
        self.assertIsNone(args.package)
        self.assertIsNone(args.test)
        self.assertFalse(args.lib)
        self.assertEqual(args.timeout, vm_test.DEFAULT_TIMEOUT_SECONDS)
        self.assertEqual(args.jobs, vm_test.DEFAULT_JOBS)
        self.assertEqual(args.extra, [])

    def test_filter_after_double_dash(self):
        args = vm_test.parse_args(["--package", "vosu-core", "--lib", "--", "config::tests::", "--nocapture"])
        self.assertEqual(args.package, "vosu-core")
        self.assertTrue(args.lib)
        self.assertEqual(args.extra, ["config::tests::", "--nocapture"])

    def test_timeout_and_jobs_overrides(self):
        args = vm_test.parse_args(["--timeout", "300", "--jobs", "4"])
        self.assertEqual(args.timeout, 300)
        self.assertEqual(args.jobs, 4)


class StreamDecodeTests(unittest.TestCase):
    def test_empty_input_is_empty_string(self):
        self.assertEqual(vm_test.stream_decode(""), "")

    def test_decodes_base64_utf8(self):
        encoded = base64.b64encode("hello\n".encode("utf-8")).decode("ascii")
        self.assertEqual(vm_test.stream_decode(encoded), "hello\n")


if __name__ == "__main__":
    unittest.main()
