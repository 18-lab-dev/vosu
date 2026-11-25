"""Exercise plugin-host-fixtures.py's pure parts: the exact cargo commands,
library selection from Cargo's JSON records, staging and its env output.
Building the real examples is the gate's own fixture step."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import unittest.mock


def _load():
    spec = importlib.util.spec_from_file_location(
        "plugin_host_fixtures", Path(__file__).with_name("plugin-host-fixtures.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


fixtures = _load()


def artifact(name, filenames, kind="example", crate_types=("cdylib",)):
    return json.dumps({
        "reason": "compiler-artifact",
        "target": {"name": name, "kind": [kind], "crate_types": list(crate_types)},
        "filenames": filenames,
    })


class BuildCommandTests(unittest.TestCase):
    def test_matches_the_frozen_acceptance_builds(self):
        commands = [fixtures.build_command(package, example, extra) for _, package, example, extra in fixtures.FIXTURES]
        self.assertEqual(commands, [
            ["cargo", "build", "--locked", "-p", "vosu-plugin-testkit", "--example", "testkit_process_plugin",
             "--message-format=json-render-diagnostics"],
            ["cargo", "build", "--locked", "-p", "vosu-osu", "--example", "osu_process_plugin",
             "--features", "test-harness", "--message-format=json-render-diagnostics"],
        ])

    def test_target_is_forwarded(self):
        command = fixtures.build_command("p", "e", (), "x86_64-pc-windows-msvc")
        self.assertEqual(command[command.index("--target") + 1], "x86_64-pc-windows-msvc")


class SelectLibraryTests(unittest.TestCase):
    def test_takes_the_reported_library_not_a_guessed_name(self):
        stdout = "\n".join([
            '{"reason":"build-script-executed"}',
            artifact("other", ["/t/debug/examples/libother.so"]),
            artifact("testkit_process_plugin", ["/t/debug/examples/libtestkit_process_plugin-9f3e.so"]),
            '{"reason":"build-finished","success":true}',
        ])
        self.assertEqual(
            fixtures.select_library(stdout, "testkit_process_plugin"),
            Path("/t/debug/examples/libtestkit_process_plugin-9f3e.so"),
        )

    def test_windows_import_library_and_pdb_are_not_libraries(self):
        stdout = artifact("osu_process_plugin", [
            "C:\\t\\examples\\osu_process_plugin.dll",
            "C:\\t\\examples\\osu_process_plugin.dll.lib",
            "C:\\t\\examples\\osu_process_plugin.pdb",
        ])
        self.assertEqual(
            fixtures.select_library(stdout, "osu_process_plugin"),
            Path("C:\\t\\examples\\osu_process_plugin.dll"),
        )

    def test_missing_or_non_cdylib_artifacts_are_errors(self):
        with self.assertRaises(fixtures.FixtureError):
            fixtures.select_library('{"reason":"build-finished","success":true}', "testkit_process_plugin")
        with self.assertRaises(fixtures.FixtureError):
            fixtures.select_library(
                artifact("testkit_process_plugin", ["/t/x"], crate_types=("bin",)), "testkit_process_plugin"
            )
        with self.assertRaises(fixtures.FixtureError):
            fixtures.select_library(
                artifact("testkit_process_plugin", ["/t/a.so", "/t/b.so"]), "testkit_process_plugin"
            )


class StageAndEnvTests(unittest.TestCase):
    def test_stage_copies_beside_the_artifact_and_keeps_the_name(self):
        with tempfile.TemporaryDirectory() as temporary:
            library = Path(temporary) / "examples" / "libtestkit_process_plugin.so"
            library.parent.mkdir()
            library.write_bytes(b"first")
            staged = fixtures.stage(library)
            self.assertEqual(staged, library.parent / "plugin-host-fixtures" / library.name)
            # A later build replacing the Cargo output leaves the stage alone.
            library.write_bytes(b"second")
            self.assertEqual(staged.read_bytes(), b"first")
            self.assertEqual(fixtures.sha256(staged), fixtures.sha256(staged))

    def test_env_file_lines_and_command_environment(self):
        paths = {"VOSU_PLUGIN_HOST_TESTKIT_LIBRARY": Path("/s/a.so"), "VOSU_PLUGIN_HOST_OSU_LIBRARY": Path("/s/b.so")}
        self.assertEqual(
            fixtures.env_lines(paths),
            ["VOSU_PLUGIN_HOST_TESTKIT_LIBRARY=/s/a.so", "VOSU_PLUGIN_HOST_OSU_LIBRARY=/s/b.so"],
        )
        with tempfile.TemporaryDirectory() as temporary:
            env_file = Path(temporary) / "env"
            env_file.write_text("EXISTING=1\n")
            with unittest.mock.patch.object(fixtures, "prepare", return_value=paths), \
                    unittest.mock.patch.object(fixtures.subprocess, "run") as run:
                run.return_value.returncode = 7
                code = fixtures.main(["--env-file", str(env_file), "--", "cargo", "test"])
            self.assertEqual(code, 7)
            self.assertEqual(env_file.read_text().splitlines()[0], "EXISTING=1")
            self.assertIn("VOSU_PLUGIN_HOST_OSU_LIBRARY=/s/b.so", env_file.read_text())
            self.assertEqual(run.call_args.args[0], ["cargo", "test"])
            self.assertEqual(run.call_args.kwargs["env"]["VOSU_PLUGIN_HOST_TESTKIT_LIBRARY"], "/s/a.so")

    def test_a_failed_build_fails_without_running_the_command(self):
        with unittest.mock.patch.object(fixtures, "prepare", side_effect=fixtures.FixtureError("boom")), \
                unittest.mock.patch.object(fixtures.subprocess, "run") as run:
            self.assertEqual(fixtures.main(["--", "cargo", "test"]), 2)
        run.assert_not_called()


class ReleaseDaemonTests(unittest.TestCase):
    def test_builds_the_packaged_release_feature_set_without_test_harness(self):
        linux = fixtures.daemon_build_command("x86_64-unknown-linux-gnu")
        self.assertEqual(linux, [
            "cargo", "build", "--locked", "--release", "-p", "vosu", "--bin", "vosu", "--no-default-features",
            "--features", "linux-process-events", "--target", "x86_64-unknown-linux-gnu",
            "--message-format=json-render-diagnostics",
        ])
        windows = fixtures.daemon_build_command("x86_64-pc-windows-msvc")
        self.assertNotIn("--features", windows)
        for command in (linux, windows):
            self.assertNotIn("test-harness", " ".join(command))

    def test_takes_the_reported_executable(self):
        record = json.dumps({
            "reason": "compiler-artifact",
            "target": {"name": "vosu", "kind": ["bin"], "crate_types": ["bin"]},
            "filenames": ["C:\\t\\release\\vosu.exe", "C:\\t\\release\\vosu.pdb"],
            "executable": "C:\\t\\release\\vosu.exe",
        })
        library = artifact("vosu", ["/t/release/libvosu.rlib"], kind="lib", crate_types=("lib",))
        self.assertEqual(fixtures.select_executable("\n".join([library, record]), "vosu"),
                         Path("C:\\t\\release\\vosu.exe"))
        with self.assertRaises(fixtures.FixtureError):
            fixtures.select_executable(library, "vosu")

    def test_the_daemon_is_opt_in(self):
        paths = {"VOSU_PLUGIN_HOST_TESTKIT_LIBRARY": Path("/s/a.so")}
        daemon = {"VOSU_PLUGIN_APPROVAL_DAEMON": Path("/s/vosu")}
        for argv, expected in [([], paths), (["--release-daemon"], {**paths, **daemon})]:
            with unittest.mock.patch.object(fixtures, "prepare", return_value=dict(paths)), \
                    unittest.mock.patch.object(fixtures, "prepare_daemon", return_value=daemon) as built, \
                    unittest.mock.patch.dict(fixtures.os.environ), \
                    unittest.mock.patch.object(fixtures.subprocess, "run") as run:
                fixtures.os.environ.pop("VOSU_PLUGIN_APPROVAL_DAEMON", None)
                run.return_value.returncode = 0
                self.assertEqual(fixtures.main([*argv, "--", "cargo", "test"]), 0)
            self.assertEqual(built.called, bool(argv))
            environment = run.call_args.kwargs["env"]
            self.assertEqual({name: environment.get(name) for name in [*paths, *daemon]},
                             {name: str(expected[name]) if name in expected else None for name in [*paths, *daemon]})


class PrimeImageTests(unittest.TestCase):
    def test_priming_is_windows_only(self):
        with unittest.mock.patch.object(fixtures.os, "name", "posix"):
            self.assertIsNone(fixtures.prime_image(Path("/nonexistent.so")))


if __name__ == "__main__":
    unittest.main()
