from __future__ import annotations

import json
import base64
import hashlib
import importlib.util
import math
from pathlib import Path
import subprocess
import os
import shutil
import socket
import threading
import time
import sys
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from live_parity import (
    PINNED_SOURCE_REVISION, PINNED_SOURCE_VERSION, Sample, align_active,
    barrier_of, compare_pair, dump_capture, monotonic_violations,
    validate_barriers,
)
from parity_domains import gamebase_path_family, infer_reference_capabilities
import parity_verify as PARITY_VERIFY
RUN_SPEC = importlib.util.spec_from_file_location("live_parity_run", HERE / "run.py")
assert RUN_SPEC and RUN_SPEC.loader
LIVE_RUN = importlib.util.module_from_spec(RUN_SPEC)
RUN_SPEC.loader.exec_module(LIVE_RUN)


def v2(daemon: str, timestamp: int, state: object, time_ms: int, *, paused: bool = False,
       static: str = "same", score: int = 0) -> Sample:
    return Sample(daemon, "v2", timestamp, {
        "state": state,
        "client": "lazer",
        "settings": {"client": {"version": "2026.921.0"}},
        "session": {"playTime": time_ms},
        "beatmap": {
            "checksum": "shared-map",
            "time": {"live": time_ms, "mp3Length": 100_000},
            "title": static,
        },
        "play": {"paused": paused, "score": score},
        "game": {"paused": paused},
    })
class BuildCliTests(unittest.TestCase):
    def test_source_build_commands_match_generated_contract_and_reject_mutations(self) -> None:
        pnpm = r"C:\Program Files\nodejs\pnpm.CMD"
        generated = [
            [pnpm, "install", "--frozen-lockfile"],
            [pnpm, "--filter", "tosu", "run", "genver"],
            [pnpm, "--filter", "tosu", "run", "ts:compile"],
        ]
        self.assertTrue(PARITY_VERIFY._valid_source_build_commands(generated, pnpm))
        for mutation in (
                [[pnpm, "--dir", r"C:\source", *generated[0][1:]], *generated[1:]],
                [*generated, [pnpm, "exec", "malicious"]],
                [[pnpm, "install", "--no-frozen-lockfile"], *generated[1:]],
                [[r"C:\untrusted\pnpm.CMD", *generated[0][1:]], *generated[1:]],
        ):
            self.assertFalse(
                PARITY_VERIFY._valid_source_build_commands(mutation, pnpm))

    def test_msvc_path_extensions_preserve_controlled_tools_and_protected_roots(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            installation = root / "Visual Studio"
            compiler = installation / "compiler"
            cmake = installation / "CMake"
            writable = installation / "writable"
            outside = root / "Visual Studio-other"
            sdk = root / "Program Files" / "Windows Kits"
            windows = root / "Windows"
            for path in (compiler, cmake, writable, outside, sdk, windows):
                path.mkdir(parents=True, exist_ok=True)
            base_parts = [str(root / "rust"), str(root / "python")]
            base = os.pathsep.join(base_parts)
            msvc = {"installation": str(installation)}
            with mock.patch.object(LIVE_RUN, "_controlled_tool_path", return_value=base), \
                    mock.patch.object(LIVE_RUN, "_windows_command_is_writable",
                                      side_effect=lambda path, _: path == writable), \
                    mock.patch.dict(os.environ, {
                        "ProgramFiles(x86)": str(sdk.parent), "WINDIR": str(windows),
                    }):
                for entries in (
                    [str(compiler), *base_parts],
                    [str(compiler), *base_parts, str(cmake)],
                    [str(sdk), *base_parts, str(windows)],
                ):
                    self.assertTrue(LIVE_RUN._valid_windows_build_path(
                        os.pathsep.join(entries), msvc, ("cargo", "python")))
                for entries in (
                    [str(compiler), *reversed(base_parts)],
                    [str(compiler), *base_parts, str(outside)],
                    [str(compiler), *base_parts, str(writable)],
                    [str(compiler), *base_parts, *base_parts],
                    [str(compiler), base_parts[0], str(cmake), base_parts[1]],
                ):
                    self.assertFalse(LIVE_RUN._valid_windows_build_path(
                        os.pathsep.join(entries), msvc, ("cargo", "python")))

    def config(self) -> dict:
        root = HERE.parents[1]
        # Pinned upstream tosu checkout and package layout.
        source = root.parent / "tosu"
        entrypoint = source / "packages/tosu/dist/index.js"
        client = root / ("osu!.exe" if sys.platform == "win32" else "osu!")
        vosu = root / "target/release" / ("vosu.exe" if sys.platform == "win32" else "vosu")
        return {
            "source_directory": str(source),
            "client_executable": str(client),
            "client_args": [],
            "client_expected_sha256": "b" * 64,
            "client_expected_version": "2026.714.0",
            "os": "windows" if sys.platform == "win32" else "linux",
            "client": "lazer",
            "poll_rate_ms": 150,
            "daemons": {
                "source": {"base_url": "http://127.0.0.1:24050",
                           "command": ["node", str(entrypoint)]},
                "vosu": {"base_url": "http://127.0.0.1:24051", "command": [str(vosu)]},
            },
        }

    def native_config(self, executable: Path, digest: str = "a" * 64) -> dict:
        cfg = self.config()
        cfg.pop("source_directory")
        cfg["source_mode"] = "native-reference"
        cfg["source_reference"] = {
            "executable": str(executable),
            "sha256": digest,
            "version": PINNED_SOURCE_VERSION,
        }
        cfg["daemons"]["source"]["command"] = [str(executable)]
        return cfg

    def run_check(self, cfg: dict) -> subprocess.CompletedProcess[str]:
        cfg = json.loads(json.dumps(cfg))
        identity = {
            "source": {"revision": PINNED_SOURCE_REVISION},
            "vosu": {"sha256": "a" * 64},
            "client": {"executable": cfg.get("client_executable"),
                       "kind": cfg.get("client")},
        }
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "config.json"
            path.write_text(json.dumps(cfg), encoding="utf-8")
            try:
                with mock.patch.object(
                        LIVE_RUN, "validate_artifacts", return_value=identity):
                    value = LIVE_RUN.load_config(str(path))
                return subprocess.CompletedProcess([], 0, json.dumps({
                    "valid": True, "artifacts": value["_artifact_identity"],
                }), "")
            except ValueError as exc:
                return subprocess.CompletedProcess([], 2, "", f"vosu-live-parity: {exc}")
    def test_fresh_build_roots_remove_stale_source_and_vosu_outputs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch.dict(os.environ, {"LOCALAPPDATA": tmp}):
            capture = Path(tmp) / "capture.json"
            source, vosu = LIVE_RUN._fresh_build_roots(str(capture))
            source.mkdir(parents=True)
            vosu.mkdir()
            (source / "stale.js").write_bytes(b"stale")
            (vosu / "stale-binary").write_bytes(b"stale")
            LIVE_RUN._fresh_build_roots(str(capture))
            self.assertFalse(source.exists())
            self.assertFalse(vosu.exists())

    def test_windows_build_roots_use_local_app_data_not_unc_capture_parent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            capture = r"\\host.lan\Data\capture.json"
            with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                    mock.patch.dict(
                        LIVE_RUN.os.environ, {"LOCALAPPDATA": tmp}):
                source, vosu = LIVE_RUN._fresh_build_roots(capture)
                source.mkdir(parents=True)
                vosu.mkdir()
                (source / "stale.js").write_bytes(b"stale")
                actual_source, actual_vosu = LIVE_RUN._fresh_build_roots(capture)

            self.assertEqual((source, vosu), (actual_source, actual_vosu))
            self.assertEqual(Path(tmp) / "tp", source.parent.parent)
            self.assertFalse(source.exists())
            self.assertFalse(vosu.exists())
    def test_controlled_build_environment_isolates_user_configuration(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp) / "clean-home"
            target = Path(tmp) / "fresh-target"
            with mock.patch.dict(
                    os.environ,
                    {"NODE_OPTIONS": "--require=evil.js",
                     "RUSTFLAGS": "--cfg evil",
                     "RUSTUP_HOME": str(Path(tmp) / "untrusted-rustup"),
                     "PYTHONPATH": str(Path(tmp) / "untrusted-python")}):
                environment = LIVE_RUN._controlled_build_environment(
                    home, {"CARGO_TARGET_DIR": target}, required_tools=())
            # Exercise isolation in a real child without requiring the unrelated
            # reference builder's Node/pnpm or a protected Rust installation.
            observed = json.loads(LIVE_RUN._command_version(
                [sys.executable, "-c",
                 "import json, os; print(json.dumps(dict(os.environ)))"],
                environment))
            self.assertEqual(str(home), observed["HOME"])
            self.assertEqual(str(target), observed["CARGO_TARGET_DIR"])
            for key in ("NODE_OPTIONS", "RUSTFLAGS", "RUSTUP_HOME", "PYTHONPATH"):
                self.assertNotIn(key, observed)

    def test_controlled_path_rejects_cross_tool_directory_shadowing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            first = Path(tmp) / "first"
            second = Path(tmp) / "second"
            first.mkdir()
            second.mkdir()
            suffix = ".exe" if sys.platform == "win32" else ""
            paths = {
                name: directory / f"{name}{suffix}"
                for name, directory in (
                    ("git", first), ("node", second), ("pnpm", second),
                    ("cargo", second), ("rustc", second))
            }
            for path in paths.values():
                path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                path.chmod(0o755)
            with (mock.patch.object(
                    LIVE_RUN, "_resolved_trusted_command",
                    side_effect=lambda name: paths[name]),
                  mock.patch.object(os, "defpath", "")):
                controlled = LIVE_RUN._controlled_tool_path()
                self.assertEqual(
                    paths["node"],
                    Path(shutil.which("node", path=controlled)).resolve())
                shadow = first / paths["node"].name
                shadow.write_bytes(paths["node"].read_bytes())
                shadow.chmod(0o755)
                with self.assertRaises(ValueError):
                    LIVE_RUN._controlled_tool_path()
                if os.name == "posix":
                    untrusted = Path(tmp) / "untrusted"
                    untrusted.mkdir()
                    injected = untrusted / paths["node"].name
                    injected.write_bytes(paths["node"].read_bytes())
                    injected.chmod(0o755)
                    with mock.patch.dict(os.environ, {
                            "PATH": os.pathsep.join(map(str, (
                                untrusted, second, first))),
                    }):
                        controlled = LIVE_RUN._controlled_tool_path()
                    self.assertEqual(
                        paths["node"],
                        Path(shutil.which("node", path=controlled)).resolve())
                    self.assertEqual(
                        paths["cargo"],
                        Path(shutil.which("cargo", path=controlled)).resolve())
                    self.assertNotIn(str(untrusted), controlled.split(os.pathsep))

    def test_source_manifest_rejects_directory_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "dist"
            external = Path(tmp) / "external"
            root.mkdir()
            external.mkdir()
            (root / "index.js").write_bytes(b"source")
            (external / "addon.js").write_bytes(b"external")
            try:
                (root / "linked").symlink_to(external, target_is_directory=True)
            except OSError:
                self.skipTest("directory symlinks unavailable")
            with self.assertRaisesRegex(ValueError, "unsafe symlink"):
                LIVE_RUN._tree_manifest(root)



    def test_source_repository_rejects_untracked_hooks_and_git_locks(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repository = Path(tmp)
            subprocess.run(
                ["git", "init", str(repository)], check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            (repository / "pnpm-lock.yaml").write_text(
                "lockfileVersion: '9.0'\n", encoding="utf-8")
            subprocess.run(
                ["git", "-C", str(repository), "add", "pnpm-lock.yaml"],
                check=True)
            subprocess.run(
                ["git", "-C", str(repository), "-c", "user.name=Parity Test",
                 "-c", "user.email=parity@example.invalid", "commit", "-m", "fixture"],
                check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

            untracked = repository / "unexpected.js"
            untracked.write_text("untrusted", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "untracked changes"):
                LIVE_RUN._validate_source_repository(repository)
            untracked.unlink()

            hook = repository / ".git/hooks/post-checkout"
            hook.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "active Git hooks"):
                LIVE_RUN._validate_source_repository(repository)
            hook.unlink()

            lock = repository / ".git/index.lock"
            lock.write_bytes(b"")
            with self.assertRaisesRegex(ValueError, "Git lock files"):
                LIVE_RUN._validate_source_repository(repository)

    def test_source_repository_rejects_filters_and_info_attributes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repository = Path(tmp)
            subprocess.run(
                ["git", "init", str(repository)], check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            (repository / "pnpm-lock.yaml").write_text(
                "lockfileVersion: '9.0'\n", encoding="utf-8")
            subprocess.run(
                ["git", "-C", str(repository), "add", "pnpm-lock.yaml"],
                check=True)
            subprocess.run(
                ["git", "-C", str(repository), "-c", "user.name=Parity Test",
                 "-c", "user.email=parity@example.invalid", "commit", "-m", "fixture"],
                check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            subprocess.run(
                ["git", "-C", str(repository), "config", "filter.parity.clean",
                 "cat"], check=True)
            with self.assertRaisesRegex(ValueError, "filters/attributes"):
                LIVE_RUN._validate_source_repository(repository)
            subprocess.run(
                ["git", "-C", str(repository), "config", "--unset-all",
                 "filter.parity.clean"], check=True)
            attributes = repository / ".git/info/attributes"
            attributes.write_text("*.js filter=parity\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "info attributes"):
                LIVE_RUN._validate_source_repository(repository)


    def test_compare_rejects_repository_filter_before_status_can_execute_it(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repository = root / "repository"
            sentinel = root / "filter-ran"
            subprocess.run(
                ["git", "init", str(repository)], check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            (repository / ".gitattributes").write_text(
                "*.txt filter=parity\n", encoding="utf-8")
            (repository / "tracked.txt").write_text(
                "tracked content\n", encoding="utf-8")
            subprocess.run(
                ["git", "-C", str(repository), "add", "."], check=True)
            subprocess.run(
                ["git", "-C", str(repository), "-c", "user.name=Parity Test",
                 "-c", "user.email=parity@example.invalid", "commit", "-m", "fixture"],
                check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            command = f"sh -c 'echo ran >> {sentinel}'"
            subprocess.run(
                ["git", "-C", str(repository), "config",
                 "filter.parity.clean", command], check=True)
            subprocess.run(
                ["git", "-C", str(repository), "config",
                 "filter.parity.process", command], check=True)

            with self.assertRaisesRegex(ValueError, "filters/attributes"):
                LIVE_RUN.verify_capture_artifacts({
                    "artifacts": {"source": {
                        "mode": "node-build", "checkout": str(repository),
                    }},
                }, [])
            self.assertFalse(sentinel.exists())
    def test_source_repository_ignores_inherited_global_git_filters(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repository = root / "repository"
            home = root / "home"
            home.mkdir()
            subprocess.run(
                ["git", "init", str(repository)], check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            (repository / "pnpm-lock.yaml").write_bytes(
                b"lockfileVersion: '9.0'\n")
            subprocess.run(
                ["git", "-C", str(repository), "add", "pnpm-lock.yaml"],
                check=True)
            subprocess.run(
                ["git", "-C", str(repository), "-c", "user.name=Parity Test",
                 "-c", "user.email=parity@example.invalid", "commit", "-m", "fixture"],
                check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            subprocess.run(
                ["git", "config", "--file", str(home / ".gitconfig"),
                 "filter.inherited.clean", "cat"], check=True)
            head = subprocess.run(
                ["git", "-C", str(repository), "rev-parse", "HEAD"],
                check=True, text=True, stdout=subprocess.PIPE).stdout.strip()

            with mock.patch.object(LIVE_RUN, "PINNED_SOURCE_REVISION", head), \
                    mock.patch.dict(
                        os.environ,
                        {"HOME": str(home), "USERPROFILE": str(home)}):
                revision = LIVE_RUN._validate_source_repository(repository)
            self.assertEqual(head, revision)

    def test_compare_rejects_hidden_source_tampering_and_unpinned_head(self) -> None:
        for tamper in ("assume-unchanged", "unpinned-head"):
            with self.subTest(tamper=tamper), tempfile.TemporaryDirectory() as tmp:
                repository = Path(tmp)
                git = ["git", "-C", str(repository)]
                subprocess.run(["git", "init", str(repository)], check=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                (repository / "pnpm-lock.yaml").write_bytes(b"lockfileVersion: '9.0'\n")
                tracked = repository / "tracked.txt"
                tracked.write_bytes(b"trusted\n")
                subprocess.run([*git, "add", "."], check=True)
                commit = [*git, "-c", "user.name=Parity Test",
                          "-c", "user.email=parity@example.invalid", "commit"]
                subprocess.run([*commit, "-m", "fixture"], check=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                pinned = subprocess.check_output([*git, "rev-parse", "HEAD"],
                                                 text=True).strip()
                if tamper == "assume-unchanged":
                    subprocess.run([*git, "update-index", "--assume-unchanged",
                                    "tracked.txt"], check=True)
                    tracked.write_bytes(b"untrusted\n")
                    expected_error = "differs from Git blob"
                else:
                    subprocess.run([*commit, "--allow-empty", "-m", "unpinned"],
                                   check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    expected_error = "not the clean pinned revision"
                revision = subprocess.check_output([*git, "rev-parse", "HEAD"],
                                                   text=True).strip()
                with mock.patch.object(LIVE_RUN, "PINNED_SOURCE_REVISION", pinned):
                    with self.assertRaisesRegex(ValueError, expected_error):
                        LIVE_RUN.verify_capture_artifacts({
                            "artifacts": {"source": {
                                "mode": "node-build", "checkout": str(repository),
                                "revision": revision,
                            }},
                        }, [])


    def test_worktree_bytes_must_equal_pinned_git_blobs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repository = Path(tmp)
            subprocess.run(
                ["git", "init", str(repository)], check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            tracked = repository / "tracked.txt"
            tracked.write_bytes(b"trusted\n")
            subprocess.run(
                ["git", "-C", str(repository), "add", "tracked.txt"], check=True)
            subprocess.run(
                ["git", "-C", str(repository), "-c", "user.name=Parity Test",
                 "-c", "user.email=parity@example.invalid", "commit", "-m", "fixture"],
                check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            LIVE_RUN._verify_pinned_worktree(repository, "HEAD")
            tracked.write_bytes(b"filtered\n")
            with self.assertRaisesRegex(ValueError, "differs from Git blob"):
                LIVE_RUN._verify_pinned_worktree(repository, "HEAD")

    def test_windows_symlink_files_require_git_policy_and_exact_target(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            git_path = LIVE_RUN._resolved_trusted_command("git")
            repository = Path(tmp)
            git = ["git", "-C", str(repository)]
            subprocess.run(
                [*git, "init"], check=True,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            target = b"target.txt"
            blob = subprocess.run(
                [*git, "hash-object", "-w", "--stdin"], input=target,
                check=True, stdout=subprocess.PIPE).stdout.decode().strip()
            subprocess.run(
                [*git, "update-index", "--add", "--cacheinfo",
                 f"120000,{blob},link.txt"], check=True)
            tree = subprocess.run(
                [*git, "write-tree"], check=True,
                stdout=subprocess.PIPE).stdout.decode().strip()
            link = repository / "link.txt"
            link.write_bytes(target)
            subprocess.run(
                [*git, "config", "core.symlinks", "false"], check=True)
            with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                    mock.patch.object(
                        LIVE_RUN, "_resolved_trusted_command", return_value=git_path):
                LIVE_RUN._verify_pinned_worktree(repository, tree)
                link.write_bytes(b"untrusted.txt")
                with self.assertRaisesRegex(ValueError, "differs from Git blob"):
                    LIVE_RUN._verify_pinned_worktree(repository, tree)
                link.write_bytes(target)
                subprocess.run(
                    [*git, "config", "core.symlinks", "true"], check=True)
                with self.assertRaisesRegex(ValueError, "differs from Git blob"):
                    LIVE_RUN._verify_pinned_worktree(repository, tree)
            subprocess.run(
                [*git, "config", "core.symlinks", "false"], check=True)
            with mock.patch.object(LIVE_RUN.sys, "platform", "linux"), \
                    mock.patch.object(
                        LIVE_RUN, "_resolved_trusted_command", return_value=git_path):
                with self.assertRaisesRegex(ValueError, "differs from Git blob"):
                    LIVE_RUN._verify_pinned_worktree(repository, tree)

    def test_user_writable_path_tool_is_not_trusted(self) -> None:
        if os.name == "nt":
            self.skipTest("POSIX path protection assertion")
        with tempfile.TemporaryDirectory() as tmp:
            tool = Path(tmp) / "parity-tool"
            tool.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            tool.chmod(0o755)
            with mock.patch.dict(os.environ, {"PATH": tmp}):
                with self.assertRaisesRegex(ValueError, "protected.*path"):
                    LIVE_RUN._resolved_trusted_command("parity-tool")
    def test_port_collision_and_unsupported_linux_stable_are_rejected(self) -> None:
        cfg = self.config()
        if sys.platform != "win32":
            cfg["client"] = "stable"
        cfg["daemons"]["vosu"]["base_url"] = cfg["daemons"]["source"]["base_url"]
        result = self.run_check(cfg)
        self.assertEqual(2, result.returncode)
        if sys.platform != "win32":
            self.assertIn("unsupported matrix", result.stderr)
        self.assertIn("ports must be distinct", result.stderr)

    def test_tree_manifest_detects_changed_and_extra_dist_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "index.js").write_bytes(b"source")
            original = LIVE_RUN._tree_manifest(root)
            (root / "native.node").write_bytes(b"stale addon")
            self.assertNotEqual(original, LIVE_RUN._tree_manifest(root))

    def test_compare_writes_machine_report_and_exits_nonzero(self) -> None:
        samples = []
        states = [(0, False, 0), (2, False, 100), (2, True, 200), (7, False, 300)]
        for daemon, base in (("source", 1_000_000_000), ("vosu", 1_001_000_000)):
            for index, (state, paused, current) in enumerate(states):
                timestamp = base + index * 100_000_000
                samples.append(v2(daemon, timestamp, state, current, paused=paused))
                for schema in ("v1", "precise", "sc"):
                    payload = {"static": "same", "keys": {"k1": 0}}
                    if daemon == "vosu" and schema == "sc" and state == 0:
                        payload["static"] = "different"
                    samples.append(Sample(daemon, schema, timestamp + 1, payload))
            timestamp = base + 4 * 100_000_000
            for schema in ("v1", "v2", "precise", "sc"):
                samples.append(Sample(daemon, schema, timestamp,
                                      {"error": "osu is not ready/running"}, 500, "not_ready"))
        with tempfile.TemporaryDirectory() as tmp:
            capture = Path(tmp) / "capture.json"
            report = Path(tmp) / "report.json"
            dump_capture(str(capture), samples, {
                "poll_rate_ms": 150, "capture_interval_ms": 25,
                "client_process_exit": {"observed_monotonic_ns": 1_300_000_000},
            })
            with mock.patch.object(LIVE_RUN, "verify_capture_artifacts"):
                result = LIVE_RUN.compare(str(capture), str(report))
            self.assertEqual(1, result)
            value = json.loads(report.read_text(encoding="utf-8"))
            self.assertFalse(value["passed"])
            self.assertTrue(any(item["schema"] == "sc" and item["pointer"] == "/static"
                                for item in value["mismatches"]))
    def test_fetch_rejects_redirect_and_generic_500_as_readiness(self) -> None:
        from http.server import BaseHTTPRequestHandler, HTTPServer

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:
                if self.path == "/json":
                    self.send_response(302)
                    self.send_header("Location", "/json/v2")
                    self.end_headers()
                else:
                    self.send_response(500)
                    self.end_headers()
                    self.wfile.write(b'{"error":"other"}')

            def log_message(self, *_: object) -> None:
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            base = f"http://127.0.0.1:{server.server_port}"
            with LIVE_RUN.HttpProbe(base, "source", "v1") as probe:
                redirected = probe.fetch()
            with LIVE_RUN.HttpProbe(base, "source", "v2") as probe:
                generic = probe.fetch()
            self.assertEqual("http_302", redirected.error)
            self.assertEqual("http_500", generic.error)
        finally:
            server.shutdown()
            thread.join()
            server.server_close()
