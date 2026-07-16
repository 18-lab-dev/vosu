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
RUN_SPEC = importlib.util.spec_from_file_location("live_parity_run", HERE / "run.py")
assert RUN_SPEC and RUN_SPEC.loader
LIVE_RUN = importlib.util.module_from_spec(RUN_SPEC)
RUN_SPEC.loader.exec_module(LIVE_RUN)


def v2(daemon: str, timestamp: int, state: object, time_ms: int, *, paused: bool = False,
       static: str = "same", score: int = 0) -> Sample:
    return Sample(daemon, "v2", timestamp, {
        "state": state,
        "client": "lazer",
        "session": {"playTime": time_ms},
        "beatmap": {
            "checksum": "shared-map",
            "time": {"live": time_ms},
            "title": static,
        },
        "play": {"paused": paused, "score": score},
        "game": {"paused": paused},
    })
class BuildCliTests(unittest.TestCase):
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
        with tempfile.TemporaryDirectory() as tmp:
            capture = Path(tmp) / "capture.json"
            source = Path(str(capture.resolve()) + ".source-worktree")
            vosu = Path(str(capture.resolve()) + ".vosu-target")
            source.mkdir()
            vosu.mkdir()
            (source / "stale.js").write_bytes(b"stale")
            (vosu / "stale-binary").write_bytes(b"stale")
            actual_source, actual_vosu = LIVE_RUN._fresh_build_roots(str(capture))
            self.assertEqual((source, vosu), (actual_source, actual_vosu))
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
                     "PATH": os.environ.get("PATH", "")}):
                rustc = str(LIVE_RUN._resolved_trusted_command("rustc"))
                environment = LIVE_RUN._controlled_build_environment(
                    home, {"CARGO_TARGET_DIR": target, "RUSTC": rustc})
            self.assertEqual(str(home), environment["HOME"])
            self.assertEqual(str(target), environment["CARGO_TARGET_DIR"])
            self.assertNotIn("NODE_OPTIONS", environment)
            self.assertNotIn("RUSTFLAGS", environment)
            self.assertNotIn("RUSTUP_HOME", environment)
            self.assertEqual(rustc, environment["RUSTC"])
            cargo_version = LIVE_RUN._command_version(
                [str(LIVE_RUN._resolved_command("cargo")), "--version"], environment)
            rustc_version = LIVE_RUN._command_version(
                [str(LIVE_RUN._resolved_command("rustc")), "--version"], environment)
            self.assertIn("cargo ", cargo_version)
            self.assertIn("rustc ", rustc_version)

    def test_controlled_path_rejects_cross_tool_directory_shadowing(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            first = Path(tmp) / "first"
            second = Path(tmp) / "second"
            first.mkdir()
            second.mkdir()
            paths = {
                "git": first / "git",
                "node": second / "node",
                "pnpm": second / "pnpm",
                "cargo": second / "cargo",
                "rustc": second / "rustc",
            }
            for path in [*paths.values(), first / "node"]:
                path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
                path.chmod(0o755)
            with (mock.patch.object(
                    LIVE_RUN, "_resolved_trusted_command",
                    side_effect=lambda name: paths[name]),
                  mock.patch.object(os, "defpath", "")):
                with self.assertRaisesRegex(ValueError, "shadows attested node"):
                    LIVE_RUN._controlled_tool_path()

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

    def test_source_repository_ignores_inherited_global_git_filters(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            repository = root / "repository"
            home = root / "home"
            home.mkdir()
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
            redirected = LIVE_RUN.fetch(base, "source", "v1")
            generic = LIVE_RUN.fetch(base, "source", "v2")
            self.assertEqual("http_302", redirected.error)
            self.assertEqual("http_500", generic.error)
        finally:
            server.shutdown()
            thread.join()
            server.server_close()
