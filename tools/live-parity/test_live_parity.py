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
    PINNED_SOURCE_REVISION, PINNED_SOURCE_VERSION, PINNED_VOSU_VERSION,
    SOURCE_BUILD_LABEL, VOSU_BUILD_LABEL, Sample, align_active, barrier_of,
    compare_pair, dump_capture, monotonic_violations, validate_barriers,
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
class VersionIdentityTests(unittest.TestCase):
    def test_vosu_metadata_error_names_the_local_version(self) -> None:
        cfg = {
            "source_mode": "node-build",
            "daemons": {"source": {}, "vosu": {}},
        }
        with (
            mock.patch.object(
                LIVE_RUN.tomllib,
                "loads",
                return_value={"package": {"version": PINNED_SOURCE_VERSION}},
            ),
            self.assertRaisesRegex(
                ValueError,
                rf"vosu package metadata version is not {PINNED_VOSU_VERSION}",
            ),
        ):
            LIVE_RUN.validate_artifacts(cfg)



class AlignmentTests(unittest.TestCase):
    def test_same_state_ordered_unique_vosu_samples(self) -> None:
        source = [v2("source", 1, 2, 1000), v2("source", 2, 2, 1190)]
        vosu = [v2("vosu", 20, 2, 1200), v2("vosu", 10, 2, 990), v2("vosu", 5, 0, 1000)]
        pairs, missing = align_active(source, vosu, 150)
        self.assertEqual([], missing)
        self.assertEqual([(1000, 990), (1190, 1200)],
                         [(p[0].payload["session"]["playTime"], p[1].payload["session"]["playTime"]) for p in pairs])

    def test_alignment_uses_beatmap_clock_when_session_clock_disagrees(self) -> None:
        source = v2("source", 1, 2, 1_000)
        vosu = v2("vosu", 2, 2, 1_100)
        source.payload["session"]["playTime"] = 900_000
        vosu.payload["session"]["playTime"] = -900_000
        pairs, missing = align_active([source], [vosu], 150)
        self.assertEqual([(source, vosu)], pairs)
        self.assertFalse(missing)

    def test_window_is_maximum_of_twice_poll_and_300(self) -> None:
        pairs, missing = align_active([v2("source", 1, 2, 0)], [v2("vosu", 2, 2, 301)], 100)
        self.assertFalse(pairs)
        self.assertEqual(1, len(missing))
        pairs, missing = align_active([v2("source", 1, 2, 0)], [v2("vosu", 2, 2, 399)], 200)
        self.assertEqual(1, len(pairs))
        self.assertFalse(missing)

    def test_timestamp_alignment_preserves_later_matches(self) -> None:
        source = [v2("source", 100, 2, 0), v2("source", 110, 2, 0)]
        vosu = [v2("vosu", 0, 2, 0), v2("vosu", 105, 2, 0)]

        pairs, missing = LIVE_RUN._align_timestamp(source, vosu, 100)

        self.assertEqual([(100, 0), (110, 105)], [
            (left.monotonic_ns, right.monotonic_ns)
            for left, right in pairs
        ])
        self.assertFalse(missing)

    def test_clock_alignment_preserves_matches_at_pause_plateau(self) -> None:
        source = [v2("source", i, 2, clock)
                  for i, clock in enumerate((9798, 9803, 9803))]
        vosu = [v2("vosu", i, 2, clock)
                for i, clock in enumerate((9719, 9803, 9803))]

        pairs, missing = align_active(source, vosu, 150)

        self.assertEqual(list(zip(source, vosu)), pairs)
        self.assertFalse(missing)

    def test_clock_alignment_minimizes_skew_without_losing_coverage(self) -> None:
        source = [v2("source", i, 2, clock)
                  for i, clock in enumerate((1000, 1150))]
        vosu = [v2("vosu", i, 2, clock)
                for i, clock in enumerate((750, 900, 1005, 1155))]

        pairs, missing = align_active(source, vosu, 150)

        self.assertEqual(list(zip(source, vosu[2:])), pairs)
        self.assertFalse(missing)

    def test_clock_plateau_uses_nearest_receipts_after_maximizing_coverage(self) -> None:
        source = [v2("source", t, 2, 1000) for t in (100, 200)]
        vosu = [v2("vosu", t, 2, 1000) for t in (0, 90, 190)]

        pairs, missing = align_active(source, vosu, 150)

        self.assertEqual(list(zip(source, vosu[1:])), pairs)
        self.assertFalse(missing)


class BarrierTests(unittest.TestCase):
    def test_ordered_barriers_and_process_exit(self) -> None:
        samples = [
            v2("source", 1, 0, 0), v2("source", 2, 2, 10),
            v2("source", 3, 2, 20, paused=True), v2("source", 4, 7, 30),
            Sample("source", "v2", 5, {"error": "osu is not ready/running"}, 500, "not_ready"),
        ]
        self.assertEqual([], validate_barriers(samples))
        self.assertEqual("paused", barrier_of(samples[2]))
        self.assertEqual("process-exit", barrier_of(samples[4]))

    def test_song_select_satisfies_menu_barrier(self) -> None:
        sample = v2("source", 1, {"number": 5, "name": "selectPlay"}, 0)
        self.assertEqual("menu", barrier_of(sample))

        v1_sample = Sample("source", "v1", 2, {"menu": {"state": 5}}, 0, None)
        self.assertEqual("menu", barrier_of(v1_sample))

    def test_capture_requires_sustained_shared_pause(self) -> None:
        play_ns = 10_000_000_000
        both_paused = {"source": "paused", "vosu": "paused"}
        self.assertFalse(LIVE_RUN._capture_barrier_ready(
            "paused", play_ns + 999_999_999, {"play": play_ns}, both_paused))
        self.assertFalse(LIVE_RUN._capture_barrier_ready(
            "paused", play_ns + 1_000_000_000, {"play": play_ns},
            {"source": "paused", "vosu": "play"}))
        self.assertTrue(LIVE_RUN._capture_barrier_ready(
            "paused", play_ns + 1_000_000_000, {"play": play_ns}, both_paused))
        self.assertTrue(LIVE_RUN._capture_barrier_ready(
            "result", play_ns, {"play": play_ns}, {}))

    def test_play_requires_both_daemons_past_first_object_and_aligned(self) -> None:
        source = v2("source", 1, 2, 180_000)
        vosu = v2("vosu", 2, 2, 2_100)
        for sample in (source, vosu):
            sample.payload["beatmap"]["time"]["firstObject"] = 2_000
            sample.payload["play"]["playerName"] = "player"
            sample.payload["session"]["playTime"] = 0
        current = {"source": source, "vosu": vosu}
        self.assertFalse(LIVE_RUN._shared_play_ready(current, 150))
        source.payload["beatmap"]["time"]["live"] = 2_350
        self.assertTrue(LIVE_RUN._shared_play_ready(current, 150))
        vosu.payload["beatmap"]["time"]["live"] = 1_999
        self.assertFalse(LIVE_RUN._shared_play_ready(current, 150))

    def test_play_requires_two_seconds_of_shared_stability(self) -> None:
        source = v2("source", 1_000_000_000, 2, 100)
        vosu = v2("vosu", 1_000_000_010, 2, 100)
        current = {"source": source, "vosu": vosu}
        for sample in current.values():
            sample.payload["beatmap"]["time"]["firstObject"] = 0
            sample.payload["play"]["playerName"] = "player"
        stable, ready_since = LIVE_RUN._shared_play_stable(
            current, 150, None)
        self.assertFalse(stable)
        current = {
            "source": v2("source", 3_100_000_000, 2, 200),
            "vosu": v2("vosu", 3_100_000_010, 2, 200),
        }
        for sample in current.values():
            sample.payload["beatmap"]["time"]["firstObject"] = 0
            sample.payload["play"]["playerName"] = "player"
        stable, _ = LIVE_RUN._shared_play_stable(
            current, 150, ready_since)
        self.assertTrue(stable)

    def test_play_rejects_loading_with_an_aligned_preview_clock(self) -> None:
        source = v2("source", 1, 2, 44_741)
        vosu = v2("vosu", 2, 2, 44_750)
        current = {"source": source, "vosu": vosu}
        for sample in current.values():
            sample.payload["beatmap"]["time"]["firstObject"] = 3_000
            sample.payload["play"]["playerName"] = ""
        self.assertEqual(
            (False, None), LIVE_RUN._shared_play_stable(current, 150, 0))
        source.payload["play"]["playerName"] = "Guest"
        self.assertFalse(LIVE_RUN._shared_play_ready(current, 150))
        vosu.payload["play"]["playerName"] = "Guest"
        self.assertTrue(LIVE_RUN._shared_play_ready(current, 150))

    def test_comparison_context_starts_at_synchronized_play(self) -> None:
        source_stale = v2("source", 3, 2, 180_000)
        vosu_first = v2("vosu", 4, 2, 2_100)
        source_ready = v2("source", 5, 2, 2_350)
        vosu_ready = v2("vosu", 6, 2, 2_200)
        source_after = v2("source", 7, 2, 2_400)
        for sample in (
                source_stale, vosu_first, source_ready, vosu_ready, source_after):
            sample.payload["beatmap"]["time"]["firstObject"] = 2_000
            sample.payload["session"]["playTime"] = 0
        for sample in (source_ready, vosu_ready, source_after):
            sample.payload["play"]["playerName"] = "player"
        contexts = LIVE_RUN._ordered_barrier_contexts({
            "source": [
                v2("source", 1, 0, 0), source_stale, source_ready, source_after],
            "vosu": [v2("vosu", 2, 0, 0), vosu_first, vosu_ready],
        }, 150)
        self.assertIs(source_ready, contexts["source"]["play"])
        self.assertIs(vosu_ready, contexts["vosu"]["play"])

    def test_menu_and_result_contexts_wait_for_both_daemons(self) -> None:
        source_menu_first = v2("source", 1, 0, 0)
        source_menu_shared = v2("source", 90, 0, 0)
        vosu_menu_shared = v2("vosu", 100, 0, 0)
        source_play = v2("source", 1_000_000_000, 2, 100)
        vosu_play = v2("vosu", 1_000_000_010, 2, 100)
        for sample in (source_play, vosu_play):
            sample.payload["play"]["playerName"] = "player"
            sample.payload["beatmap"]["time"]["firstObject"] = 0
        source_paused = v2(
            "source", 2_100_000_000, 2, 500, paused=True)
        vosu_paused = v2(
            "vosu", 2_100_000_010, 2, 500, paused=True)
        source_result_first = v2("source", 3_000_000_000, 7, 500)
        source_result_shared = v2("source", 3_500_000_000, 7, 500)
        vosu_result_shared = v2("vosu", 3_500_000_010, 7, 500)
        source_result_shared.payload["resultsScreen"] = {"name": "player"}
        vosu_result_shared.payload["resultsScreen"] = {"name": "player"}

        contexts = LIVE_RUN._ordered_barrier_contexts({
            "source": [
                source_menu_first,
                source_menu_shared,
                source_play,
                source_paused,
                source_result_first,
                source_result_shared,
            ],
            "vosu": [
                vosu_menu_shared,
                vosu_play,
                vosu_paused,
                vosu_result_shared,
            ],
        }, 150)

        self.assertIs(source_menu_shared, contexts["source"]["menu"])
        self.assertIs(vosu_menu_shared, contexts["vosu"]["menu"])
        self.assertIs(source_result_shared, contexts["source"]["result"])
        self.assertIs(vosu_result_shared, contexts["vosu"]["result"])

    def test_persisted_promotions_select_first_matching_samples(self) -> None:
        source_initial = v2("source", 10, 0, 0)
        vosu_initial = v2("vosu", 20, 0, 0)
        source_initial.payload["beatmap"]["checksum"] = "source-map"
        vosu_initial.payload["beatmap"]["checksum"] = "vosu-map"
        source_later = v2("source", 100, 0, 0)
        vosu_later = v2("vosu", 110, 0, 0)
        source_play_early = v2("source", 120, 2, 0)
        vosu_play_early = v2("vosu", 130, 2, 0)
        source_play = v2("source", 200, 2, 0)
        vosu_play = v2("vosu", 210, 2, 0)

        contexts = LIVE_RUN._ordered_barrier_contexts(
            {
                "source": [
                    source_initial, source_later, source_play_early, source_play],
                "vosu": [vosu_initial, vosu_later, vosu_play_early, vosu_play],
            },
            150,
            {
                "source": {"menu": 10, "play": 200},
                "vosu": {"menu": 20, "play": 210},
            },
        )

        self.assertIs(source_initial, contexts["source"]["menu"])
        self.assertIs(vosu_initial, contexts["vosu"]["menu"])
        self.assertIs(source_play, contexts["source"]["play"])
        self.assertIs(vosu_play, contexts["vosu"]["play"])

    def test_persisted_process_exit_promotion_uses_exit_samples(self) -> None:
        source_exit = Sample(
            "source", "v2", 3_000_000_000,
            {"error": "osu is not ready/running"}, 500, "not_ready")
        vosu_exit = Sample(
            "vosu", "v2", 3_010_000_000,
            {"error": "osu is not ready/running"}, 500, "not_ready")
        contexts = LIVE_RUN._ordered_barrier_contexts(
            {
                "source": [
                    v2("source", 10, 0, 0),
                    v2("source", 100_000_000, 2, 100),
                    v2("source", 1_200_000_000, 2, 100, paused=True),
                    v2("source", 2_000_000_000, 7, 100),
                    source_exit,
                ],
                "vosu": [
                    v2("vosu", 20, 0, 0),
                    v2("vosu", 110_000_000, 2, 100),
                    v2("vosu", 1_210_000_000, 2, 100, paused=True),
                    v2("vosu", 2_010_000_000, 7, 100),
                    vosu_exit,
                ],
            },
            150,
            {
                "source": {
                    "menu": 10, "play": 100_000_000,
                    "paused": 1_200_000_000, "result": 2_000_000_000,
                    "process-exit": 3_000_000_000,
                },
                "vosu": {
                    "menu": 20, "play": 110_000_000,
                    "paused": 1_210_000_000, "result": 2_010_000_000,
                    "process-exit": 3_010_000_000,
                },
            },
        )

        self.assertIs(source_exit, contexts["source"]["process-exit"])
        self.assertIs(vosu_exit, contexts["vosu"]["process-exit"])

    def test_semantic_readiness_requires_shared_map_and_client_identity(self) -> None:
        source = v2("source", 1, 0, 0)
        vosu = v2("vosu", 2, 0, 0)
        self.assertTrue(LIVE_RUN._shared_menu_ready(
            {"source": source, "vosu": vosu}, 150))
        vosu.payload["client"] = ""
        self.assertFalse(LIVE_RUN._shared_menu_ready(
            {"source": source, "vosu": vosu}, 150))
        vosu.payload["client"] = "lazer"
        vosu.payload["beatmap"]["checksum"] = "other-map"
        self.assertFalse(LIVE_RUN._shared_menu_ready(
            {"source": source, "vosu": vosu}, 150))

        source.payload["state"] = 7
        vosu.payload["state"] = 7
        source.payload["resultsScreen"] = {"name": "player"}
        vosu.payload["resultsScreen"] = {"name": "player"}
        self.assertFalse(LIVE_RUN._shared_result_ready(
            {"source": source, "vosu": vosu}))

    def test_menu_waits_for_selected_map_audio_timing_on_both_sides(self) -> None:
        current = {"source": v2("source", 1, 0, 0), "vosu": v2("vosu", 2, 0, 0)}
        for sample in current.values():
            timing = sample.payload["beatmap"]["time"]
            del timing["mp3Length"]
            self.assertFalse(LIVE_RUN._shared_menu_ready(current, 150))
            for pending in (None, True, -1, float("nan"), float("inf")):
                timing["mp3Length"] = pending
                self.assertFalse(LIVE_RUN._shared_menu_ready(current, 150))
            timing["mp3Length"] = 100_000
            self.assertTrue(LIVE_RUN._shared_menu_ready(current, 150))

    def test_menu_waits_for_initialized_client_settings_on_both_sides(self) -> None:
        source = v2("source", 1, 0, 0)
        vosu = v2("vosu", 2, 0, 0)
        # A client can report menu before either daemon has read its settings.
        for sample in (source, vosu):
            sample.payload["beatmap"] = {}
        current = {"source": source, "vosu": vosu}
        self.assertTrue(LIVE_RUN._shared_menu_ready(current, 150))
        for sample in (source, vosu):
            for version in ("", " ", None):
                sample.payload["settings"]["client"]["version"] = version
                self.assertFalse(LIVE_RUN._shared_menu_ready(current, 150))
            sample.payload["settings"]["client"]["version"] = "2026.921.0"

    def test_menu_rejects_stale_cross_daemon_observation(self) -> None:
        current = {
            "source": v2("source", 1_000_000_000, 0, 0),
            "vosu": v2("vosu", 1_300_000_001, 0, 0),
        }
        self.assertFalse(LIVE_RUN._shared_menu_ready(current, 150))
        current["source"] = v2("source", 1_000_000_001, 0, 0)
        self.assertTrue(LIVE_RUN._shared_menu_ready(current, 150))


    def test_out_of_order_does_not_satisfy_gate(self) -> None:
        samples = [v2("source", 1, 2, 10), v2("source", 2, 0, 0), v2("source", 3, 7, 30)]
        self.assertEqual(["play", "paused", "result", "process-exit"], validate_barriers(samples))


class ComparatorTests(unittest.TestCase):
    def test_active_volatile_values_allow_difference_but_shape_is_exact(self) -> None:
        left = v2("source", 1, 2, 1000, score=10)
        right = v2("vosu", 2, 2, 1200, score=20)
        self.assertEqual([], compare_pair(left, right, 300))
        del right.payload["play"]["score"]
        mismatches = compare_pair(left, right, 300)
        self.assertEqual("keys", mismatches[0].comparator)
        self.assertEqual("/play", mismatches[0].pointer)

    def test_documented_vosu_extensions_preserve_shared_field_comparison(self) -> None:
        cases = (
            ("v1", {"gameplay": {"hits": {"hitErrorArray": [2]}}},
             {"gameplay": {"hits": {"hitErrorArray": [2],
                                   "pressHitErrors": [2], "releaseHitErrors": []}}}),
            ("v1", {"tourney": {"ipcClients": [{"gameplay": {"hits": {
                "hitErrorArray": [2]}}}]}},
             {"tourney": {"ipcClients": [{"gameplay": {"hits": {
                 "hitErrorArray": [2], "pressHitErrors": [2],
                 "releaseHitErrors": []}}}]}}),
            ("sc", {"hitErrors": [2]},
             {"hitErrors": [2], "pressHitErrors": [2], "releaseHitErrors": []}),
            ("v2", {"play": {"score": 0}},
             {"play": {"score": 0}, "keys": {"k1": 1, "all": []}}),
            ("v2", {"keys": {"k1": 1}, "play": {"score": 0}},
             {"keys": {"k1": 1, "all": []},
              "play": {"score": 0, "pressHitErrors": [], "releaseHitErrors": []}}),
            ("v2", {"tourney": {"clients": [{"play": {"score": 0}}]}},
             {"tourney": {"clients": [{"play": {
                 "score": 0, "pressHitErrors": [], "releaseHitErrors": []}}]}}),
            ("precise", {"keys": {"k1": 1}, "hitErrors": [2]},
             {"keys": {"k1": 1, "all": []}, "hitErrors": [2],
              "pressHitErrors": [2], "releaseHitErrors": []}),
            ("precise", {"tourney": [{"keys": {"k1": 1}, "hitErrors": [2]}]},
             {"tourney": [{"keys": {"k1": 1, "all": []}, "hitErrors": [2],
                          "pressHitErrors": [2], "releaseHitErrors": []}]}),
        )
        for schema, source_payload, vosu_payload in cases:
            with self.subTest(schema=schema, payload=source_payload):
                source = Sample("source", schema, 1, source_payload)
                vosu = Sample("vosu", schema, 2, vosu_payload)
                for active in (False, True):
                    self.assertEqual([], compare_pair(
                        source, vosu, 300, active_override=active))
                # Missing legacy fields must not be hidden by the extension policy.
                source_payload["required"] = 1
                self.assertEqual(["keys"], [
                    item.comparator for item in compare_pair(source, vosu, 300)])

        source = Sample("source", "precise", 1, {"pressHitErrors": [1]})
        vosu = Sample("vosu", "precise", 2, {"pressHitErrors": [2]})
        self.assertEqual([("/pressHitErrors/0", "exact")], [
            (item.pointer, item.comparator)
            for item in compare_pair(source, vosu, 300, active_override=False)])

    def test_extension_allowance_is_exact_and_one_sided(self) -> None:
        cases = (
            ("precise", {"unexpected": []}),
            ("precise", {"play": {"pressHitErrors": []}}),
            ("precise", {"keys": {"allExtra": []}}),
            ("v2", {"pressHitErrors": []}),
            ("v2", {"resultsScreen": {"releaseHitErrors": []}}),
            ("v1", {"keys": {"all": []}}),
            ("sc", {"play": {"pressHitErrors": []}}),
        )
        for schema, payload in cases:
            with self.subTest(schema=schema, payload=payload):
                self.assertTrue(compare_pair(
                    Sample("source", schema, 1, {}),
                    Sample("vosu", schema, 2, payload), 300))
        self.assertTrue(compare_pair(
            Sample("source", "precise", 1, {"pressHitErrors": []}),
            Sample("vosu", "precise", 2, {}), 300))

    def test_static_active_value_mismatch_is_detected(self) -> None:
        left = v2("source", 1, 2, 1000, static="A")
        right = v2("vosu", 2, 2, 1000, static="B")
        mismatch = compare_pair(left, right, 300)
        self.assertEqual([("/beatmap/title", "exact")], [(m.pointer, m.comparator) for m in mismatch])
    def test_gamebase_unresolved_excludes_only_registered_paths(self) -> None:
        sentinel = {
            "beatmap": {
                "checksum": "shared-map",
                "time": {"firstObject": 0, "lastObject": 0},
            },
            "folders": {"songs": ""},
            "performance": {"graph": {"series": []}},
        }
        capability = infer_reference_capabilities([
            Sample("source", "v2", 1, sentinel),
            Sample("source", "v2", 2, sentinel),
        ])
        self.assertEqual("unresolved", capability["gamebase"]["status"])

        left = Sample("source", "v2", 3, {
            "beatmap": {"stats": {"maxCombo": 0}},
            "resultsScreen": {"name": "player"},
        })
        right = Sample("vosu", "v2", 4, {
            "beatmap": {"stats": {"maxCombo": 9732}},
            "resultsScreen": {"name": "different"},
        })
        coverage: list[dict] = []
        mismatches = compare_pair(
            left, right, 300,
            unobservable=lambda schema, parts: gamebase_path_family(
                schema, parts),
            coverage=coverage,
        )
        self.assertEqual(
            [("/resultsScreen/name", "exact")],
            [(item.pointer, item.comparator) for item in mismatches])
        self.assertEqual(
            {"/beatmap/stats"},
            {item["path_family"] for item in coverage})

    def test_gamebase_ready_keeps_registered_paths_strict(self) -> None:
        ready = {
            "beatmap": {
                "checksum": "shared-map",
                "time": {"firstObject": 100, "lastObject": 200},
            },
            "folders": {"songs": "/songs"},
            "performance": {"graph": {"series": [{"data": [1]}]}},
        }
        capability = infer_reference_capabilities([
            Sample("source", "v2", 1, ready)])
        self.assertEqual("ready", capability["gamebase"]["status"])
        left = Sample("source", "v2", 1, {
            "beatmap": {"stats": {"maxCombo": 1}}})
        right = Sample("vosu", "v2", 2, {
            "beatmap": {"stats": {"maxCombo": 2}}})
        self.assertEqual(
            [("/beatmap/stats/maxCombo", "exact")],
            [(item.pointer, item.comparator)
             for item in compare_pair(left, right, 300)])


    def test_graph_values_allow_only_one_serialization_ulp(self) -> None:
        value = 18.111197644476704
        adjacent = math.nextafter(value, -math.inf)
        cases = {
            "v1-strains": (
                "v1",
                {"menu": {"pp": {"strains": [value]}}},
                {"menu": {"pp": {"strains": [adjacent]}}},
            ),
            "v1-series": (
                "v1",
                {"menu": {"pp": {"strainsAll": {"series": [{"data": [value]}]}}}},
                {"menu": {"pp": {"strainsAll": {"series": [{"data": [adjacent]}]}}}},
            ),
            "v2-series": (
                "v2",
                {"performance": {"graph": {"series": [{"data": [value]}]}}},
                {"performance": {"graph": {"series": [{"data": [adjacent]}]}}},
            ),
            "sc-map-strains": (
                "sc",
                {"mapStrains": {"12000": value}},
                {"mapStrains": {"12000": adjacent}},
            ),
        }
        for name, (schema, left_payload, right_payload) in cases.items():
            with self.subTest(name=name):
                left = Sample("source", schema, 1, left_payload)
                right = Sample("vosu", schema, 2, right_payload)
                self.assertEqual([], compare_pair(left, right, 300))

        below = math.nextafter(1.0, -math.inf)
        two_below = math.nextafter(below, -math.inf)
        boundary_left = Sample(
            "source", "v2", 1,
            {"performance": {"graph": {"series": [{"data": [1.0]}]}}})
        boundary_right = Sample(
            "vosu", "v2", 2,
            {"performance": {"graph": {"series": [{"data": [two_below]}]}}})
        self.assertTrue(compare_pair(boundary_left, boundary_right, 300))

        left = Sample("source", "v2", 1, cases["v2-series"][1])
        far = Sample(
            "vosu", "v2", 2,
            {"performance": {"graph": {"series": [{"data": [value + 1e-9]}]}}})
        mismatch = compare_pair(left, far, 300)
        self.assertEqual(
            [("/performance/graph/series/0/data/0", "exact")],
            [(item.pointer, item.comparator) for item in mismatch])

    def test_paused_snapshot_is_fully_exact(self) -> None:
        left = v2("source", 1, 2, 1000, paused=True, score=10)
        right = v2("vosu", 2, 2, 1000, paused=True, score=11)
        mismatch = compare_pair(left, right, 300)
        self.assertIn(("/play/score", "exact"), [(m.pointer, m.comparator) for m in mismatch])

    def test_active_dynamic_sequences_skip_length_and_content_only(self) -> None:
        cases = {
            "precise": ({"hitErrors": [1]}, {"hitErrors": [2, 3]}),
            "sc": ({"hitErrors": [1]}, {"hitErrors": [2, 3]}),
            "v1": (
                {"gameplay": {"hits": {"hitErrorArray": [1]}}},
                {"gameplay": {"hits": {"hitErrorArray": [2, 3]}}},
            ),
            "v2": (
                {"play": {"hitErrorArray": [1]}},
                {"play": {"hitErrorArray": [2, 3]}},
            ),
        }
        for schema, (left_payload, right_payload) in cases.items():
            with self.subTest(schema=schema):
                left = Sample("source", schema, 1, left_payload)
                right = Sample("vosu", schema, 2, right_payload)
                self.assertEqual(
                    [], compare_pair(left, right, 300, active_override=True))
                self.assertTrue(
                    compare_pair(left, right, 300, active_override=False))
    def test_source_hit_error_lists_compare_against_vosu_all_only(self) -> None:
        def payload(schema: str, hit_errors: object) -> dict:
            if schema == "v1":
                return {"gameplay": {"hits": {"hitErrorArray": hit_errors}}}
            if schema == "v2":
                return {"play": {"hitErrorArray": hit_errors}}
            return {"hitErrors": hit_errors}

        vosu_errors = {
            "all": [2, 3],
            "press": ["ignored"],
            "release": {"ignored": True},
        }
        for schema in ("precise", "sc", "v1", "v2"):
            with self.subTest(schema=schema):
                active_source = Sample(
                    "source", schema, 1, payload(schema, [1]))
                vosu = Sample(
                    "vosu", schema, 2, payload(schema, vosu_errors))
                self.assertEqual(
                    [],
                    compare_pair(
                        active_source, vosu, 300, active_override=True),
                )

                exact_source = Sample(
                    "source", schema, 1, payload(schema, [2, 3]))
                self.assertEqual(
                    [],
                    compare_pair(
                        exact_source, vosu, 300, active_override=False),
                )

                short_source = Sample(
                    "source", schema, 1, payload(schema, [2]))
                mismatch = compare_pair(
                    short_source, vosu, 300, active_override=False)
                self.assertEqual("length", mismatch[0].comparator)

    def test_hit_error_normalization_is_restricted_to_exact_paths(self) -> None:
        left = Sample("source", "v2", 1, {"decoy": [2, 3]})
        right = Sample("vosu", "v2", 2, {"decoy": {"all": [2, 3]}})
        mismatch = compare_pair(left, right, 300, active_override=False)
        self.assertEqual(
            [("/decoy", "type")],
            [(item.pointer, item.comparator) for item in mismatch],
        )

    def test_result_gameplay_is_volatile_but_result_screen_is_exact(self) -> None:
        left = Sample(
            "source", "v2", 1,
            {
                "play": {"score": 0, "hitErrorArray": [1, 2]},
                "resultsScreen": {"name": "source"},
            },
        )
        right = Sample(
            "vosu", "v2", 2,
            {
                "play": {"score": 1_000_000, "hitErrorArray": []},
                "resultsScreen": {"name": "vosu"},
            },
        )
        mismatches = compare_pair(
            left, right, 300, active_override=True,
            unobservable=lambda schema, parts: gamebase_path_family(
                schema, parts),
        )
        self.assertEqual(
            [("/resultsScreen/name", "exact")],
            [(item.pointer, item.comparator) for item in mismatches],
        )


    def test_bass_is_always_volatile_and_live_analysis_is_active_only(self) -> None:
        cases = {
            "v1": (
                {"menu": {"bm": {"stats": {"SR": 4.0}},
                          "mainMenu": {"bassDensity": 0.1}}},
                {"menu": {"bm": {"stats": {"SR": 4.1}},
                          "mainMenu": {"bassDensity": 0.2}}},
                "/menu/bm/stats/SR",
            ),
            "v2": (
                {"beatmap": {"stats": {"stars": {"live": 4.0}}},
                 "settings": {"bassDensity": 0.1}},
                {"beatmap": {"stats": {"stars": {"live": 4.1}}},
                 "settings": {"bassDensity": 0.2}},
                "/beatmap/stats/stars/live",
            ),
        }
        for schema, (left_payload, right_payload, expected_pointer) in cases.items():
            with self.subTest(schema=schema):
                left = Sample("source", schema, 1, left_payload)
                right = Sample("vosu", schema, 2, right_payload)
                self.assertEqual(
                    [], compare_pair(left, right, 300, active_override=True))
                self.assertEqual(
                    [expected_pointer],
                    [item.pointer for item in compare_pair(
                        left, right, 300, active_override=False)],
                )

    def test_phase_sensitive_fields_are_always_volatile(self) -> None:
        cases = (
            (
                "v1",
                {
                    "menu": {"bm": {"time": {"current": 100}}},
                    "gameplay": {"paused": False},
                },
                {
                    "menu": {"bm": {"time": {"current": 200}}},
                    "gameplay": {"paused": True},
                },
            ),
            (
                "v2",
                {
                    "session": {"playTime": 100},
                    "beatmap": {"time": {"live": 100}},
                    "game": {"paused": False},
                },
                {
                    "session": {"playTime": 200},
                    "beatmap": {"time": {"live": 200}},
                    "game": {"paused": True},
                },
            ),
            (
                "sc",
                {"mapPosition": 100, "time": 100, "timeLeft": 900},
                {"mapPosition": 200, "time": 200, "timeLeft": 800},
            ),
        )
        for schema, left_payload, right_payload in cases:
            with self.subTest(schema=schema):
                left = Sample("source", schema, 1, left_payload)
                right = Sample("vosu", schema, 2, right_payload)
                self.assertEqual(
                    [], compare_pair(left, right, 300, active_override=False))

                right_payload = dict(right_payload)
                first = next(iter(right_payload))
                right_payload[first] = "wrong type"
                wrong_type = Sample("vosu", schema, 2, right_payload)
                self.assertTrue(any(
                    mismatch.comparator == "type"
                    for mismatch in compare_pair(
                        left, wrong_type, 300, active_override=False)
                ))

    def test_types_and_omissions_are_detected_inside_volatile_root(self) -> None:
        left = Sample("source", "precise", 1, {"keys": {"k1": 1}})
        right = Sample("vosu", "precise", 2, {"keys": {"k1": "1", "k2": 0}})
        # Precise is only volatile while playing; at barriers/default it remains exact.
        mismatch = compare_pair(left, right, 300)
        got = {(m.pointer, m.comparator) for m in mismatch}
        self.assertIn(("/keys", "keys"), got)
        self.assertIn(("/keys/k1", "type"), got)

    def test_monotonic_score_regression_is_detected(self) -> None:
        samples = [v2("source", 1, 2, 10, score=10), v2("source", 2, 2, 20, score=9)]
        mismatch = monotonic_violations(samples)
        self.assertIn("/play/score", [m.pointer for m in mismatch])
    def test_confirmed_retry_segment_resets_monotonic_baseline(self) -> None:
        samples = [
            v2("source", 1, 2, 79_538, score=10),
            v2("source", 2, 2, 0, score=0),
            v2("source", 3, 2, 100, score=1),
        ]
        mismatches = monotonic_violations(
            samples, segment_of=lambda sample: int(sample.monotonic_ns >= 2))
        self.assertFalse(any(
            item.comparator == "monotonic" for item in mismatches))


    def test_sc_time_and_all_hit_regressions_are_path_specific(self) -> None:
        names = (
            "time", "score", "geki", "c300", "katsu", "c100", "c50", "miss",
            "sliderBreaks",
        )
        first = {name: 10 for name in names}
        second = {name: 9 for name in names}
        first["unrelated"] = {"time": 10, "c300": 10, "miss": 10}
        second["unrelated"] = {"time": 1, "c300": 1, "miss": 1}
        samples = [Sample("source", "sc", 1, first), Sample("source", "sc", 2, second)]
        mismatches = monotonic_violations(samples, lambda _: True)
        self.assertEqual({f"/{name}" for name in names}, {m.pointer for m in mismatches})

    def test_every_sc_monotonic_leaf_requires_two_numeric_observations(self) -> None:
        payload = {name: 1 for name in (
            "time", "score", "geki", "c300", "katsu", "c100", "c50", "miss",
            "sliderBreaks",
        )}
        mismatch = monotonic_violations(
            [Sample("source", "sc", 1, payload, transport="websocket")],
            lambda _: True)
        self.assertEqual(9, len([item for item in mismatch
                                if item.comparator == "coverage"]))

    def test_precise_key_counts_are_monotonic_and_require_two_samples(self) -> None:
        keys = {
            key: {"isPressed": False, "count": 2}
            for key in ("k1", "k2", "m1", "m2")
        }
        one = monotonic_violations(
            [Sample("source", "precise", 1, {"keys": keys})],
            lambda _: True)
        self.assertEqual(
            {f"/keys/{key}/count" for key in keys},
            {item.pointer for item in one if item.comparator == "coverage"})
        regressed = {
            key: {"isPressed": False, "count": 1}
            for key in ("k1", "k2", "m1", "m2")
        }
        two = monotonic_violations([
            Sample("source", "precise", 1, {"keys": keys}),
            Sample("source", "precise", 2, {"keys": regressed}),
        ], lambda _: True)
        self.assertEqual(
            {f"/keys/{key}/count" for key in keys},
            {item.pointer for item in two if item.comparator == "monotonic"})

    def test_nonfinite_numeric_values_fail_monotonic_coverage(self) -> None:
        samples = [
            Sample("source", "sc", 1, {"score": float("nan")}),
            Sample("source", "sc", 2, {"score": float("inf")}),
        ]
        mismatches = monotonic_violations(samples, lambda _: True)
        self.assertIn(
            ("/score", "invalid-numeric"),
            {(item.pointer, item.comparator) for item in mismatches})
        self.assertIn(
            ("/score", "coverage"),
            {(item.pointer, item.comparator) for item in mismatches})


def load_tests(
        loader: unittest.TestLoader,
        tests: unittest.TestSuite,
        pattern: str | None) -> unittest.TestSuite:
    if __name__ != "__main__":
        return tests
    modules = (
        "test_live_parity_websocket",
        "test_live_parity_compare_cli",
        "test_live_parity_config_cli",
        "test_live_parity_build_cli",
        "test_live_parity_process",
    )
    suite = loader.suiteClass([tests])
    for module_name in modules:
        suite.addTests(loader.loadTestsFromModule(__import__(module_name)))
    return suite


if __name__ == "__main__":
    unittest.main()
