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
class ConfigCliTests(unittest.TestCase):
    @unittest.skipIf(sys.platform == "win32", "Linux build environment contract")
    def test_linux_build_environment_does_not_require_cmd(self) -> None:
        environment = {"PATH": "/usr/bin"}
        actual, msvc = LIVE_RUN._windows_msvc_environment(
            environment, Path("/tmp/unused"), None)
        self.assertIs(environment, actual)
        self.assertIsNone(msvc)

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
    def test_config_schema_exposes_artifacts_and_matrix(self) -> None:
        result = subprocess.run([sys.executable, str(HERE / "run.py"), "config-schema"],
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.assertEqual(0, result.returncode, result.stderr)
        schema = json.loads(result.stdout)
        self.assertNotIn("source_directory", schema["required"])
        self.assertIn("client_executable", schema["required"])
        self.assertIn("client_expected_sha256", schema["required"])
        self.assertNotIn("client_pid", schema["properties"])
        self.assertEqual(
            ["launch", "external"],
            schema["properties"]["client_launch_mode"]["enum"],
        )
        self.assertEqual(3, len(schema["oneOf"]))
        self.assertEqual(
            ["mania-hold"], schema["properties"]["scenario"]["enum"])

    def test_mania_hold_scenario_is_optional_and_strict(self) -> None:
        general = self.run_check(self.config())
        self.assertEqual(0, general.returncode, general.stderr)

        mania = self.config()
        mania["scenario"] = "mania-hold"
        valid = self.run_check(mania)
        self.assertEqual(0, valid.returncode, valid.stderr)

        invalid = self.config()
        invalid["scenario"] = "mania"
        rejected = self.run_check(invalid)
        self.assertEqual(2, rejected.returncode)
        self.assertIn(
            "scenario must be mania-hold when provided", rejected.stderr)

    def test_cli_guidance_names_mania_hold_capture_requirements(self) -> None:
        result = subprocess.run(
            [sys.executable, str(HERE / "run.py"), "--help"],
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("mania-hold", result.stdout)
        self.assertIn("hold notes and judged tails", result.stdout)

    def test_config_schema_exposes_native_reference_branch(self) -> None:
        schema = LIVE_RUN.config_schema()
        self.assertEqual(
            ["node-build", "native-reference"],
            schema["properties"]["source_mode"]["enum"],
        )
        reference = schema["properties"]["source_reference"]
        self.assertEqual(
            ["executable", "sha256", "version"], reference["required"])
        self.assertEqual(
            "^[0-9a-f]{64}$", reference["properties"]["sha256"]["pattern"])
        self.assertEqual(
            PINNED_SOURCE_VERSION, reference["properties"]["version"]["const"])
        self.assertFalse(reference["additionalProperties"])

        source_branches = schema["allOf"][0]["oneOf"]
        node = next(
            branch for branch in source_branches
            if branch["properties"]["source_mode"].get("enum") == ["node-build"])
        native = next(
            branch for branch in source_branches
            if branch["properties"]["source_mode"].get("const")
            == "native-reference")
        self.assertIn("source_directory", node["required"])
        self.assertEqual(["source_reference"], node["not"]["required"])
        self.assertIn("source_reference", native["required"])
        self.assertEqual(["source_directory"], native["not"]["required"])

    def test_valid_artifacts_are_derived(self) -> None:
        result = self.run_check(self.config())
        self.assertEqual(0, result.returncode, result.stderr)
        value = json.loads(result.stdout)
        self.assertEqual(PINNED_SOURCE_REVISION, value["artifacts"]["source"]["revision"])
        self.assertEqual("lazer", value["artifacts"]["client"]["kind"])
        self.assertEqual(64, len(value["artifacts"]["vosu"]["sha256"]))

    def test_validate_artifacts_accepts_exact_native_reference(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            # Native source fixtures model the pinned upstream tosu release.
            source = Path(tmp) / (
                "upstream-tosu-source.exe"
                if sys.platform == "win32" else "upstream-tosu-source")
            client = Path(tmp) / (
                "osu!.exe" if sys.platform == "win32" else "osu!")
            magic = b"MZ\0\0" if sys.platform == "win32" else b"\x7fELF"
            source.write_bytes(
                magic + f"native upstream tosu {PINNED_SOURCE_VERSION} release".encode())
            source.chmod(0o755)
            client.write_bytes(b"client fixture")
            source_hash = LIVE_RUN.sha256_file(source)
            cfg = self.native_config(source, source_hash)
            cfg["client_executable"] = str(client)
            cfg["client_expected_sha256"] = LIVE_RUN.sha256_file(client)
            with (mock.patch.object(
                    LIVE_RUN, "_derive_client_version",
                    return_value=(cfg["client_expected_version"], None)),
                  mock.patch.object(
                      LIVE_RUN, "_resolved_trusted_command",
                      side_effect=lambda value: (
                          source if value == str(source)
                          else LIVE_RUN._resolved_command(value)))):
                identity = LIVE_RUN.validate_artifacts(cfg)

        self.assertEqual("native-reference", identity["source"]["mode"])
        self.assertEqual(str(source.resolve()), identity["source"]["executable"])
        self.assertEqual(source_hash, identity["source"]["sha256"])
        self.assertEqual(PINNED_SOURCE_VERSION, identity["source"]["version"])
        self.assertEqual([str(source.resolve())], identity["source"]["command"])

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux release identity")
    def test_compressed_native_reference_requires_exact_published_digest(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            # Model a compressed upstream pkg release without a literal version.
            source = Path(tmp) / "upstream-tosu-source"
            source.write_bytes(b"\x7fELFcompressed upstream payload")
            source.chmod(0o755)
            published_hash = LIVE_RUN.sha256_file(source)
            client = Path(tmp) / "osu!"
            client.write_bytes(b"client fixture")
            cfg = self.native_config(source, published_hash)
            cfg["client_executable"] = str(client)
            cfg["client_expected_sha256"] = LIVE_RUN.sha256_file(client)
            with (mock.patch.object(
                    LIVE_RUN, "_PINNED_LINUX_REFERENCE",
                    (PINNED_SOURCE_VERSION, published_hash)),
                  mock.patch.object(
                    LIVE_RUN, "_derive_client_version",
                    return_value=(cfg["client_expected_version"], None)),
                  mock.patch.object(
                    LIVE_RUN, "_resolved_trusted_command",
                    side_effect=lambda value: (
                        source if value == str(source)
                        else LIVE_RUN._resolved_command(value)))):
                identity = LIVE_RUN.validate_artifacts(cfg)
                self.assertEqual(
                    published_hash, identity["source"]["sha256"])

                # A caller-provided digest does not attest a different release.
                source.write_bytes(b"\x7fELFdifferent compressed upstream payload")
                cfg["source_reference"]["sha256"] = LIVE_RUN.sha256_file(source)
                with self.assertRaisesRegex(ValueError, "native release binary"):
                    LIVE_RUN.validate_artifacts(cfg)

                # Even an allowlisted digest cannot override the ELF requirement.
                source.write_bytes(b"not an ELF compressed payload")
                invalid_hash = LIVE_RUN.sha256_file(source)
                cfg["source_reference"]["sha256"] = invalid_hash
                with mock.patch.object(
                        LIVE_RUN, "_PINNED_LINUX_REFERENCE",
                        (PINNED_SOURCE_VERSION, invalid_hash)):
                    with self.assertRaisesRegex(ValueError, "native release binary"):
                        LIVE_RUN.validate_artifacts(cfg)

    def test_validate_artifacts_rejects_native_reference_identity_mismatch(
            self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            # Native source fixtures model the pinned upstream tosu release.
            source = Path(tmp) / (
                "upstream-tosu-source.exe"
                if sys.platform == "win32" else "upstream-tosu-source")
            client = Path(tmp) / (
                "osu!.exe" if sys.platform == "win32" else "osu!")
            magic = b"MZ\0\0" if sys.platform == "win32" else b"\x7fELF"
            source.write_bytes(
                magic + f"native upstream tosu {PINNED_SOURCE_VERSION} release".encode())
            source.chmod(0o755)
            client.write_bytes(b"client fixture")
            source_hash = LIVE_RUN.sha256_file(source)

            for mismatch, expected_error in (
                    ("hash", "digest"),
                    ("version", "version"),
                    ("command", "command")):
                with self.subTest(mismatch=mismatch):
                    cfg = self.native_config(source, source_hash)
                    cfg["client_executable"] = str(client)
                    cfg["client_expected_sha256"] = LIVE_RUN.sha256_file(client)
                    if mismatch == "hash":
                        cfg["source_reference"]["sha256"] = "0" * 64
                    elif mismatch == "version":
                        cfg["source_reference"]["version"] = "4.24.0"
                    else:
                        cfg["daemons"]["source"]["command"].append("--unexpected")
                    with (mock.patch.object(
                            LIVE_RUN, "_derive_client_version",
                            return_value=(cfg["client_expected_version"], None)),
                          mock.patch.object(
                              LIVE_RUN, "_resolved_trusted_command",
                              side_effect=lambda value: (
                                  source if value == str(source)
                                  else LIVE_RUN._resolved_command(value)))):
                        with self.assertRaisesRegex(ValueError, expected_error):
                            LIVE_RUN.validate_artifacts(cfg)

    def test_external_client_launch_mode_is_accepted(self) -> None:
        cfg = self.config()
        cfg["client_launch_mode"] = "external"
        result = self.run_check(cfg)
        self.assertEqual(0, result.returncode, result.stderr)

    def test_invalid_client_launch_mode_is_rejected(self) -> None:
        cfg = self.config()
        cfg["client_launch_mode"] = "detached"
        result = self.run_check(cfg)
        self.assertEqual(2, result.returncode)
        self.assertIn("client_launch_mode must be launch or external", result.stderr)

    def test_unknown_configuration_field_is_rejected(self) -> None:
        cfg = self.config()
        cfg["untrusted_identity"] = "spoof"
        result = self.run_check(cfg)
        self.assertEqual(2, result.returncode)
        self.assertIn("unknown configuration fields", result.stderr)

    def test_native_reference_loader_rejects_missing_reference(self) -> None:
        cfg = self.config()
        cfg.pop("source_directory")
        cfg["source_mode"] = "native-reference"
        result = self.run_check(cfg)
        self.assertEqual(2, result.returncode)
        self.assertIn(
            "source_reference is required in native-reference mode",
            result.stderr,
        )

    def test_native_reference_loader_rejects_unknown_fields(self) -> None:
        executable = Path(tempfile.gettempdir()).resolve() / "upstream-tosu-native"
        cfg = self.native_config(executable)
        cfg["source_reference"]["revision"] = PINNED_SOURCE_REVISION
        result = self.run_check(cfg)
        self.assertEqual(2, result.returncode)
        self.assertIn("source_reference has unknown fields", result.stderr)

    def test_native_reference_loader_rejects_malformed_reference(self) -> None:
        executable = Path(tempfile.gettempdir()).resolve() / "upstream-tosu-native"
        cases = {
            "not-an-object": [],
            "missing-executable": {
                "sha256": "a" * 64, "version": PINNED_SOURCE_VERSION},
            "missing-sha256": {
                "executable": str(executable), "version": PINNED_SOURCE_VERSION},
            "missing-version": {
                "executable": str(executable), "sha256": "a" * 64},
            "uppercase-digest": {
                "executable": str(executable),
                "sha256": "A" * 64,
                "version": PINNED_SOURCE_VERSION,
            },
            "wrong-version": {
                "executable": str(executable),
                "sha256": "a" * 64,
                "version": "4.24.0",
            },
        }
        for case, reference in cases.items():
            with self.subTest(case=case):
                cfg = self.native_config(executable)
                cfg["source_reference"] = reference
                result = self.run_check(cfg)
                self.assertEqual(2, result.returncode)
                self.assertIn("source_reference", result.stderr)
                if case == "wrong-version":
                    self.assertIn(
                        f"source_reference.version must be exactly "
                        f"{PINNED_SOURCE_VERSION}",
                        result.stderr,
                    )

    def test_native_reference_loader_rejects_source_directory(self) -> None:
        executable = Path(tempfile.gettempdir()).resolve() / "upstream-tosu-native"
        cfg = self.native_config(executable)
        cfg["source_directory"] = str(Path(tempfile.gettempdir()).resolve())
        result = self.run_check(cfg)
        self.assertEqual(2, result.returncode)
        self.assertIn(
            "source_directory is forbidden in native-reference mode",
            result.stderr,
        )

    def test_loader_rejects_unknown_source_mode(self) -> None:
        cfg = self.config()
        cfg["source_mode"] = "download"
        result = self.run_check(cfg)
        self.assertEqual(2, result.returncode)
        self.assertIn(
            "source_mode must be node-build or native-reference",
            result.stderr,
        )

    def test_node_build_default_still_requires_source_directory(self) -> None:
        cfg = self.config()
        cfg.pop("source_directory")
        result = self.run_check(cfg)
        self.assertEqual(2, result.returncode)
        self.assertIn(
            "source_directory is required in node-build mode",
            result.stderr,
        )

    def test_noncanonical_source_command_shape_is_rejected(self) -> None:
        cfg = self.config()
        cfg["daemons"]["source"]["command"].append("--unexpected")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "config.json"
            path.write_text(json.dumps(cfg), encoding="utf-8")
            with mock.patch.object(
                    LIVE_RUN, "validate_artifacts",
                    side_effect=ValueError(
                        "source daemon command must be exactly Node plus pinned dist/index.js")):
                with self.assertRaisesRegex(ValueError, "exactly Node"):
                    LIVE_RUN.load_config(str(path))

    def test_code_loading_environment_override_is_rejected(self) -> None:
        cfg = self.config()
        cfg["daemons"]["source"]["env"] = {"NODE_OPTIONS": "--require=evil.js"}
        result = self.run_check(cfg)
        self.assertEqual(2, result.returncode)
        self.assertIn("code-loading overrides", result.stderr)

    def test_scrubbed_environment_drops_inherited_override(self) -> None:
        with mock.patch.dict(os.environ, {"NODE_OPTIONS": "--require=evil.js",
                                          "PATH": os.environ.get("PATH", "")}):
            environment = LIVE_RUN._scrubbed_environment()
        self.assertNotIn("NODE_OPTIONS", environment)

    def test_scrubbed_environment_preserves_gui_session_transport(self) -> None:
        with mock.patch.dict(
                os.environ,
                {"WAYLAND_DISPLAY": "wayland-test",
                 "XDG_RUNTIME_DIR": "/run/user/test",
                 "LANG": "en_US.UTF-8", "LC_TIME": "en_US.UTF-8"},
                clear=True):
            environment = LIVE_RUN._scrubbed_environment()
        self.assertEqual("wayland-test", environment["WAYLAND_DISPLAY"])
        self.assertEqual("/run/user/test", environment["XDG_RUNTIME_DIR"])
        self.assertEqual("en_US.UTF-8", environment["LANG"])
        self.assertEqual("en_US.UTF-8", environment["LC_TIME"])

    def test_loader_and_compiler_wrapper_overrides_are_rejected(self) -> None:
        for key in (
                "LD_PRELOAD", "BASH_ENV", "PERL5OPT", "JAVA_TOOL_OPTIONS",
                "SCCACHE_DIR", "CC", "RUSTC_WRAPPER", "GIT_CONFIG_COUNT"):
            with self.subTest(key=key):
                with self.assertRaisesRegex(ValueError, "override is forbidden"):
                    LIVE_RUN._scrubbed_environment({key: "attacker-controlled"})

    def test_spoofed_client_label_is_rejected(self) -> None:
        cfg = self.config()
        cfg["client_build_label"] = "trusted-looking-spoof"
        result = self.run_check(cfg)
        self.assertEqual(2, result.returncode)
        self.assertIn("unknown configuration fields", result.stderr)
