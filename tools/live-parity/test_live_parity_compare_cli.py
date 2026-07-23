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
from live_parity import (PINNED_SOURCE_REVISION, Sample, align_active, barrier_of,
                         compare_pair, dump_capture, monotonic_violations, validate_barriers)
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
class CompareCliTests(unittest.TestCase):
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

    def complete_samples(self, *, vosu_ws_static: str = "same",
                         vosu_second_ns: int = 1_150_000_000) -> list[Sample]:
        samples: list[Sample] = []
        phases = [
            ("menu", 100_000_000, 0),
            ("play", 1_000_000_000, 100),
            ("play", 1_150_000_000, 200),
            ("play", 1_300_000_000, 300),
            ("play", 1_450_000_000, 400),
            ("play", 1_600_000_000, 500),
            ("play", 1_750_000_000, 600),
            ("play", 1_900_000_000, 700),
            ("paused", 2_000_000_000, 700),
            ("result", 3_000_000_000, 700),
        ]
        hits = {
            "0": 0, "50": 0, "100": 0, "300": 1, "geki": 0, "katu": 0,
            "sliderBreaks": 0, "sliderEndHits": 0, "smallTickHits": 0,
            "largeTickHits": 0,
        }
        for daemon in ("source", "vosu"):
            for transport in ("http", "websocket"):
                for phase, timestamp, value in phases:
                    if (daemon == "vosu" and transport == "websocket"
                            and timestamp == 1_150_000_000):
                        timestamp = vosu_second_ns
                    paused = phase == "paused"
                    state = ("menu" if phase == "menu" else
                             "result" if phase == "result" else "play")
                    static = vosu_ws_static if (
                        daemon == "vosu" and transport == "websocket") else "same"
                    payloads = {
                        "v1": {
                            "menu": {"bm": {"time": {"current": value}}},
                            "gameplay": {"paused": paused, "score": value, "hits": hits},
                            "state": state, "static": static,
                        },
                        "v2": {
                            "state": state, "client": "lazer",
                            "session": {"playTime": value},
                            "beatmap": {
                                "checksum": "shared-map",
                                "time": {
                                    "live": value, "firstObject": 0,
                                    "lastObject": 1,
                                },
                            },
                            "play": {
                                "score": value,
                                "hits": hits,
                                "playerName": (
                                    "player"
                                    if phase in {"play", "paused"}
                                    else ""
                                ),
                            },
                            "game": {"paused": paused}, "static": static,
                            "resultsScreen": {
                                "name": "player" if phase == "result" else ""
                            },
                        },
                        "sc": {
                            "state": state, "paused": paused, "static": static,
                            "time": value, "score": value, "geki": 0, "c300": 1,
                            "katsu": 0, "c100": 0, "c50": 0, "miss": 0,
                            "sliderBreaks": 0,
                        },
                        "precise": {
                            "state": state, "paused": paused, "static": static,
                            "keys": {
                                key: {"isPressed": False, "count": value}
                                for key in ("k1", "k2", "m1", "m2")
                            },
                        },
                    }
                    for schema, payload in payloads.items():
                        samples.append(Sample(
                            daemon, schema, timestamp, payload, transport=transport))
            for schema in ("v1", "v2", "precise", "sc"):
                samples.append(Sample(
                    daemon, schema, 4_100_000_000,
                    {"error": "osu is not ready/running"}, 500, "not_ready"))
        return samples

    def run_compare_samples(
            self, samples: list[Sample], *, scenario: str | None = None) -> dict:
        with tempfile.TemporaryDirectory() as tmp:
            capture = Path(tmp) / "capture.json"
            report = Path(tmp) / "report.json"
            metadata = {
                "poll_rate_ms": 150, "capture_interval_ms": 25,
                "client_process_exit": {"observed_monotonic_ns": 4_000_000_000},
            }
            if scenario is not None:
                metadata["scenario"] = scenario
            dump_capture(str(capture), samples, metadata)
            with mock.patch.object(LIVE_RUN, "verify_capture_artifacts"):
                LIVE_RUN.compare(str(capture), str(report))
            return json.loads(report.read_text(encoding="utf-8"))

    def mania_hold_samples(
            self, *, release: bool = True,
            partition_matches: bool = True) -> list[Sample]:
        samples = self.complete_samples()
        release_errors = [2] if release else []
        all_errors = [1, *release_errors] if partition_matches else [1]
        for sample in samples:
            if (sample.schema == "v2"
                    and isinstance(sample.payload, dict)
                    and sample.payload.get("state") == "play"):
                sample.payload["play"]["mode"] = {
                    "number": 3, "name": "mania"}
                sample.payload["play"]["hitErrorArray"] = {
                    "all": list(all_errors),
                    "press": [1],
                    "release": list(release_errors),
                }
        return samples

    def test_general_capture_does_not_enable_mania_hold_contract(self) -> None:
        report = self.run_compare_samples(self.complete_samples())
        self.assertTrue(report["passed"])
        self.assertEqual([], report["vosu_contract_failures"])

    def test_valid_mania_hold_capture_passes_contract(self) -> None:
        report = self.run_compare_samples(
            self.mania_hold_samples(), scenario="mania-hold")
        self.assertTrue(report["passed"])
        self.assertEqual([], report["vosu_contract_failures"])
        self.assertEqual("mania-hold", report["metadata"]["scenario"])

    def test_mania_hold_capture_requires_release_event(self) -> None:
        report = self.run_compare_samples(
            self.mania_hold_samples(release=False), scenario="mania-hold")
        self.assertFalse(report["passed"])
        self.assertEqual("failed", report["status"])
        self.assertTrue(any(
            item["reason"] == "missing_mania_release_event"
            for item in report["vosu_contract_failures"]))
        self.assertEqual(
            len(report["vosu_contract_failures"]),
            report["summary"]["vosu_contract_failures"],
        )

    def test_mania_hold_capture_requires_hit_error_partition(self) -> None:
        report = self.run_compare_samples(
            self.mania_hold_samples(partition_matches=False),
            scenario="mania-hold",
        )
        self.assertFalse(report["passed"])
        failures = report["vosu_contract_failures"]
        self.assertTrue(any(
            item["reason"] == "hit_error_partition_mismatch"
            and item["all"] == 1
            and item["press"] == 1
            and item["release"] == 1
            for item in failures))
        self.assertEqual("failed", report["summary"]["status"])

    def test_mania_hold_malformed_mode_and_arrays_are_contract_failures(
            self) -> None:
        cases = (
            (
                "mode",
                lambda play: play.__setitem__(
                    "mode", {"number": "3", "name": "mania"}),
                "malformed_mania_mode",
                "/play/mode/number",
            ),
            (
                "arrays",
                lambda play: play.__setitem__(
                    "hitErrorArray", {"all": [], "press": []}),
                "missing_or_malformed_hit_error_partition",
                "/play/hitErrorArray/release",
            ),
        )
        for name, corrupt, expected_reason, expected_pointer in cases:
            with self.subTest(case=name):
                samples = self.mania_hold_samples()
                for sample in samples:
                    if (sample.schema == "v2"
                            and isinstance(sample.payload, dict)
                            and sample.payload.get("state") == "play"):
                        corrupt(sample.payload["play"])
                report = self.run_compare_samples(
                    samples, scenario="mania-hold")
                self.assertFalse(report["passed"])
                self.assertEqual([], report["parity_mismatches"])
                failures = report["vosu_contract_failures"]
                self.assertTrue(any(
                    item["reason"] == expected_reason
                    and (
                        item.get("pointer") == expected_pointer
                        or expected_pointer in item.get("pointers", [])
                    )
                    for item in failures))
    def test_active_transport_error_cannot_be_hidden_by_adjacent_samples(self) -> None:
        samples = []
        replaced = False
        for sample in self.complete_samples():
            if (not replaced and sample.daemon == "vosu" and sample.schema == "sc"
                    and sample.transport == "http"
                    and sample.monotonic_ns == 1_300_000_000):
                samples.append(Sample(
                    sample.daemon, sample.schema, sample.monotonic_ns,
                    None, 0, "ConnectionError", sample.transport))
                replaced = True
            else:
                samples.append(sample)
        report = self.run_compare_samples(samples)
        self.assertTrue(any(
            item["comparator"] in {"transport-error", "http-status"}
            for item in report["mismatches"]))

    def test_stateless_streams_use_nearest_v2_context(self) -> None:
        samples = self.complete_samples()
        for sample in samples:
            if sample.schema not in {"sc", "precise"} \
                    or not isinstance(sample.payload, dict):
                continue
            sample.payload.pop("state", None)
            sample.payload.pop("paused", None)
            if sample.daemon == "vosu" and 1_000_000_000 <= \
                    sample.monotonic_ns < 2_000_000_000:
                sample.payload["static"] = "active-difference"
            if sample.daemon == "vosu" and sample.monotonic_ns == 2_000_000_000:
                sample.payload["static"] = "paused-difference"

        report = self.run_compare_samples(samples)

        self.assertFalse(any(
            schema in missing and "active-coverage" in missing
            for missing in report["missing_streams"]
            for schema in ("/sc/", "/precise/")
        ))
        self.assertTrue(any(
            item["schema"] in {"sc", "precise"}
            and item["pointer"] == "/static"
            and item["vosu"] == "paused-difference"
            for item in report["mismatches"]
        ))

    def test_http_and_websocket_payloads_are_compared_independently(self) -> None:
        report = self.run_compare_samples(
            self.complete_samples(vosu_ws_static="websocket-only-divergence"))
        self.assertTrue(any(
            item["pointer"] == "/static" and item["vosu"] == "websocket-only-divergence"
            for item in report["mismatches"]))

    def test_v1_alignment_does_not_cross_observation_order(self) -> None:
        samples = self.complete_samples()
        for index, sample in enumerate(samples):
            if (sample.schema != "v1" or sample.transport != "websocket"
                    or sample.payload.get("state") != "play"
                    or sample.payload["gameplay"]["paused"]):
                continue
            value = sample.payload["menu"]["bm"]["time"]["current"]
            sample.payload["static"] = f"time-{value}"
            timestamp = sample.monotonic_ns
            if sample.daemon == "vosu":
                if timestamp == 1_150_000_000:
                    timestamp = 1_300_000_000
                elif timestamp == 1_300_000_000:
                    timestamp = 1_150_000_000
            samples[index] = Sample(
                sample.daemon, sample.schema, timestamp, sample.payload,
                sample.status, sample.error, sample.transport)

        report = self.run_compare_samples(samples)
        self.assertTrue(any(
            item["pointer"] == "/static" and item["schema"] == "v1"
            for item in report["mismatches"]))

    def test_websocket_stalls_are_classified_by_daemon(self) -> None:
        for stalled, category in (
                ("source", "reference_failures"),
                ("vosu", "vosu_contract_failures")):
            with self.subTest(stalled=stalled):
                samples = [
                    sample for sample in self.complete_samples()
                    if not (
                        sample.daemon == stalled
                        and sample.transport == "websocket"
                        and 1_300_000_000 <= sample.monotonic_ns
                        < 1_900_000_000)
                ]
                report = self.run_compare_samples(samples)
                failures = (
                    report["coverage"]["reference_failures"]
                    if category == "reference_failures"
                    else report["vosu_contract_failures"])
                self.assertTrue(any(
                    item.get("reason") == "absolute_ws_stall"
                    and item.get("daemon") == stalled
                    for item in failures))

    def test_http_completion_gaps_are_metrics_not_cadence_failures(self) -> None:
        samples = [
            Sample(
                sample.daemon, sample.schema,
                sample.monotonic_ns + 500_000_000
                if (sample.daemon == "vosu" and sample.transport == "http"
                    and sample.monotonic_ns == 1_300_000_000)
                else sample.monotonic_ns,
                sample.payload, sample.status, sample.error, sample.transport)
            for sample in self.complete_samples()
        ]
        report = self.run_compare_samples(samples)
        self.assertTrue(report["metrics"]["http_probe_gaps"])
        self.assertFalse(any(
            item.get("reason") == "absolute_ws_stall"
            for item in report["vosu_contract_failures"]))
        self.assertFalse(any(
            item.get("comparator") in {"cadence-gap", "max-silence"}
            for item in report["mismatches"]))

    def test_dense_vosu_frames_are_metrics_but_unpaired_source_fails(self) -> None:
        samples = self.complete_samples()
        dense = [
            Sample(
                "vosu", sample.schema, sample.monotonic_ns + 1,
                json.loads(json.dumps(sample.payload)),
                sample.status, sample.error, sample.transport)
            for sample in samples
            if (sample.daemon == "vosu" and sample.transport == "websocket"
                and sample.monotonic_ns == 1_450_000_000)
        ]
        dense_report = self.run_compare_samples([*samples, *dense])
        self.assertTrue(dense_report["metrics"]["dense_vosu_frames"])
        self.assertFalse(any(
            "active-vosu-" in item for item in dense_report["missing_streams"]))

        sparse_vosu = [
            sample for sample in samples
            if not (sample.daemon == "vosu" and sample.schema == "v2"
                    and sample.transport == "websocket"
                    and sample.monotonic_ns == 1_450_000_000)
        ]
        unpaired_report = self.run_compare_samples(sparse_vosu)
        self.assertTrue(any(
            "websocket/v2/active-source-" in item
            for item in unpaired_report["missing_streams"]))
    def test_unresolved_gamebase_report_is_never_a_full_pass(self) -> None:
        samples = self.complete_samples()
        for sample in samples:
            if (sample.daemon == "source" and sample.schema == "v2"
                    and isinstance(sample.payload, dict)
                    and "beatmap" in sample.payload):
                sample.payload["beatmap"]["time"]["lastObject"] = 0
                sample.payload["folders"] = {"songs": ""}
                sample.payload["performance"] = {"graph": {"series": []}}
        report = self.run_compare_samples(samples)
        self.assertEqual(
            "unresolved",
            report["coverage"]["reference_capabilities"]["gamebase"]["status"])
        self.assertFalse(report["passed"])
        self.assertEqual("inconclusive", report["status"])
        self.assertTrue(report["coverage"]["unobservable_reference"])


    def test_missing_paused_websocket_frame_blocks_comparison(self) -> None:
        samples = [
            sample for sample in self.complete_samples()
            if not (sample.daemon == "vosu" and sample.transport == "websocket"
                    and sample.monotonic_ns == 2_000_000_000)
        ]
        report = self.run_compare_samples(samples)
        self.assertTrue(any("/paused/vosu" in item
                            for item in report["missing_streams"]))

    def test_identical_error_samples_cannot_satisfy_a_barrier(self) -> None:
        samples = [
            Sample(
                sample.daemon, sample.schema, sample.monotonic_ns,
                {"error": "temporary"}, 500, "http_500", sample.transport)
            if (sample.schema == "sc" and sample.transport == "http"
                and sample.monotonic_ns == 2_000_000_000)
            else sample
            for sample in self.complete_samples()
        ]
        report = self.run_compare_samples(samples)
        self.assertIn("http/sc/paused/source", report["missing_streams"])
        self.assertIn("http/sc/paused/vosu", report["missing_streams"])

    def test_scalar_success_samples_cannot_satisfy_a_barrier(self) -> None:
        samples = [
            Sample(
                sample.daemon, sample.schema, sample.monotonic_ns,
                7, 200, None, sample.transport)
            if (sample.schema == "sc" and sample.transport == "http"
                and sample.monotonic_ns == 2_000_000_000)
            else sample
            for sample in self.complete_samples()
        ]
        report = self.run_compare_samples(samples)
        self.assertIn("http/sc/paused/source", report["missing_streams"])
        self.assertIn("http/sc/paused/vosu", report["missing_streams"])

    def test_identical_active_errors_cannot_satisfy_stream_parity(self) -> None:
        samples = [
            Sample(
                sample.daemon, sample.schema, sample.monotonic_ns,
                {"error": "temporary"}, 500, "http_500", sample.transport)
            if (sample.schema == "sc" and sample.transport == "http"
                and sample.monotonic_ns == 1_300_000_000)
            else sample
            for sample in self.complete_samples()
        ]
        report = self.run_compare_samples(samples)
        self.assertIn("http/sc/active-invalid-source-2", report["missing_streams"])
        self.assertIn("http/sc/active-invalid-vosu-2", report["missing_streams"])

    def test_scalar_active_samples_are_explicitly_invalid(self) -> None:
        samples = [
            Sample(
                sample.daemon, sample.schema, sample.monotonic_ns,
                ["not", "state"], 200, None, sample.transport)
            if (sample.schema == "sc" and sample.transport == "http"
                and sample.monotonic_ns == 1_300_000_000)
            else sample
            for sample in self.complete_samples()
        ]
        report = self.run_compare_samples(samples)
        self.assertIn("http/sc/active-invalid-source-2", report["missing_streams"])
        self.assertIn("http/sc/active-invalid-vosu-2", report["missing_streams"])

    def test_barrier_pairing_excludes_pre_promotion_transients(self) -> None:
        samples: list[Sample] = []
        for sample in self.complete_samples():
            if (
                    sample.daemon == "vosu"
                    and sample.schema == "v2"
                    and sample.transport == "http"
                    and sample.monotonic_ns == 100_000_000):
                samples.append(Sample(
                    sample.daemon,
                    sample.schema,
                    95_000_000,
                    {
                        **sample.payload,
                        "beatmap": {},
                        "static": "transient",
                    },
                    sample.status,
                    sample.error,
                    sample.transport,
                ))
                samples.append(Sample(
                    sample.daemon,
                    sample.schema,
                    110_000_000,
                    sample.payload,
                    sample.status,
                    sample.error,
                    sample.transport,
                ))
            else:
                samples.append(sample)

        report = self.run_compare_samples(samples)

        self.assertFalse(any(
            item["schema"] == "v2"
            and item["pointer"] == "/static"
            and item["vosu"] == "transient"
            for item in report["mismatches"]
        ))

    def test_barriers_require_cross_daemon_timestamp_bound(self) -> None:
        samples = [
            Sample(
                sample.daemon, sample.schema,
                sample.monotonic_ns + 800_000_000
                if (sample.daemon == "vosu"
                    and sample.monotonic_ns == 3_000_000_000)
                else sample.monotonic_ns,
                sample.payload, sample.status, sample.error, sample.transport)
            for sample in self.complete_samples()
        ]
        report = self.run_compare_samples(samples)
        self.assertIn(
            "http/v2/result/cross-daemon-bound", report["missing_streams"])

    def test_barrier_pairing_jointly_selects_bounded_observations(self) -> None:
        samples: list[Sample] = []
        source_result: Sample | None = None
        for sample in self.complete_samples():
            if (
                    sample.schema == "v1"
                    and sample.transport == "http"
                    and sample.monotonic_ns == 3_000_000_000):
                timestamp = (
                    2_710_000_000
                    if sample.daemon == "source"
                    else 3_290_000_000
                )
                shifted = Sample(
                    sample.daemon, sample.schema, timestamp, sample.payload,
                    sample.status, sample.error, sample.transport)
                samples.append(shifted)
                if sample.daemon == "source":
                    source_result = shifted
            else:
                samples.append(sample)
        assert source_result is not None
        samples.append(Sample(
            source_result.daemon,
            source_result.schema,
            3_200_000_000,
            source_result.payload,
            source_result.status,
            source_result.error,
            source_result.transport,
        ))

        report = self.run_compare_samples(samples)

        self.assertNotIn(
            "http/v1/result/cross-daemon-bound", report["missing_streams"])

    def test_process_exit_requires_cross_daemon_timestamp_bound(self) -> None:
        samples = [
            Sample(
                sample.daemon, sample.schema,
                sample.monotonic_ns + 1_000_000_000
                if (sample.daemon == "vosu" and sample.status == 500)
                else sample.monotonic_ns,
                sample.payload, sample.status, sample.error, sample.transport)
            for sample in self.complete_samples()
        ]
        report = self.run_compare_samples(samples)
        self.assertIn(
            "http/sc/fresh-process-exit/cross-daemon-bound",
            report["missing_streams"])

    def test_capture_rejects_nondeterministic_interval_before_building(self) -> None:
        with mock.patch.object(LIVE_RUN, "build_release_artifacts") as build:
            with self.assertRaisesRegex(ValueError, "exactly 25"):
                LIVE_RUN.capture({}, "unused.json", 10, 50)
            build.assert_not_called()

    def test_post_exit_envelope_must_be_fresh_for_every_schema(self) -> None:
        samples = self.complete_samples()
        samples = [sample for sample in samples
                   if not (sample.daemon == "vosu" and sample.schema == "sc"
                           and sample.status == 500)]
        samples.append(Sample(
            "vosu", "sc", 3_900_000_000,
            {"error": "osu is not ready/running"}, 500, "not_ready"))
        report = self.run_compare_samples(samples)
        self.assertIn(
            "http/sc/fresh-process-exit/vosu", report["missing_streams"])
