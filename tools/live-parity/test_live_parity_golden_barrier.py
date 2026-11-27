"""Unit tests for parity_golden_barrier.py (barrier wait/observe, split out of
parity_golden_capture.py: docs/architecture/source-map.md's 1,000-line maintained-file
limit). `Session._wait_barrier`/`_observe` are exercised directly with a mocked
`_observe`, so these need neither the fake-daemon harness nor a real client."""
from __future__ import annotations

import json
from pathlib import Path
import sys
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import parity_golden_barrier as pgb
import parity_golden_capture as gc
from live_parity import Sample

READY = pgb.ClientFields("chart-checksum", "", "game-root")
BLANK = pgb.ClientFields("", "", "")


def observe(seen, view, clock, *, source=READY, vosu=READY) -> pgb.BarrierObservation:
    return pgb.BarrierObservation(seen, view, clock, source, vosu)


class BarrierTests(unittest.TestCase):
    def session(self, observations: list, *, timeout: float = 5) -> tuple[gc.Session, list]:
        client = mock.Mock()
        client.exited.return_value = False
        session = gc.Session({"golden": {}}, gc.SCENARIO_SETS["m4b-dry-run-v1"], Path("x"),
                             timeout=timeout, build=None, client=client, operator=lambda b, m: None)
        calls: list = []

        def next_observation(_source, _dump):
            calls.append(1)
            observation = observations[len(calls) - 1]
            return observation if isinstance(observation, pgb.BarrierObservation) else observe(*observation)
        session._observe = next_observation
        return session, calls

    def test_startup_requires_both_state_zero_spectra_and_bounds_the_wait(self) -> None:
        from dataclasses import replace
        data = {"state": {"number": 0}, "settings": {"bassDensity": 0.6}}
        self.assertTrue(pgb.main_menu_fft(data))
        self.assertTrue(pgb.main_menu_fft({**data, "settings": {"bassDensity": 1}}))
        for bad in (0, -0.1, True, "0.6", float("inf")):
            self.assertFalse(pgb.main_menu_fft({**data, "settings": {"bassDensity": bad}}))
        self.assertFalse(pgb.main_menu_fft({**data, "state": {"number": 5}}))
        view = observe({"source": "menu", "vosu": "menu"}, "same", None)
        source_ready = replace(view, main_menu_fft=(True, False))
        vosu_ready = replace(view, main_menu_fft=(False, True))
        both = replace(view, main_menu_fft=(True, True))
        session, calls = self.session([source_ready, vosu_ready, both])
        with mock.patch.object(pgb, "POLL_S", 0):
            session._wait_main_menu_fft(None, None)
        self.assertEqual(len(calls), 3)
        self.assertTrue(session.transcript["main_menu_fft"]["source"])
        self.assertTrue(session.transcript["main_menu_fft"]["vosu"])

        session, _ = self.session([source_ready])
        with mock.patch.object(pgb.time, "monotonic", side_effect=[0.0, 0.0, 30.0]), \
                self.assertRaisesRegex(pgb.CaptureError, "main_menu_fft_not_observed"):
            session._wait_main_menu_fft(None, None)
        self.assertNotIn("main_menu_fft", session.transcript)

    def test_barrier_needs_consecutive_identical_views(self) -> None:
        views = ["a", "b", "b", None, "c", "c", "c", "d"]
        session, calls = self.session([({"source": "menu", "vosu": "menu"}, v, None) for v in views])
        with mock.patch.object(pgb, "POLL_S", 0):
            session._wait_barrier("menu", None, None, [])
        self.assertEqual(len(calls), 7)
        self.assertEqual(session.transcript["barriers"][0]["stable_observations"], 3)
        self.assertTrue(session.transcript["barriers"][0]["identical_views"])

    def test_caseless_barrier_needs_only_the_reported_state(self) -> None:
        # Active play: the clock and the provider generation change every observation.
        views = ["a", None, "b", "c"]
        session, calls = self.session([({"source": "menu", "vosu": "menu"}, v, None) for v in views])
        with mock.patch.object(pgb, "POLL_S", 0):
            session._wait_barrier("menu", None, None, [], identical_views=False)
        self.assertEqual(len(calls), 3)
        self.assertFalse(session.transcript["barriers"][0]["identical_views"])

    def test_observes_client_fields_from_both_v2_payloads(self) -> None:
        session, _ = self.session([])
        source, dump = mock.Mock(port=1234), mock.Mock()
        blank = {"state": {"name": "menu"}, "beatmap": {"checksum": ""},
                 "directPath": {"beatmapFile": "."}, "folders": {"game": ""}}
        ready = {"state": {"name": "selectPlay"}, "beatmap": {"checksum": "chart-checksum"},
                 "folders": {"game": "game-root"}}
        dump.fetch.return_value = {"http": {"kind": "ready", "body_utf8": json.dumps(ready)}}
        with mock.patch.object(pgb, "HttpProbe") as probe, \
                mock.patch.object(pgb.volatile, "dump_view", return_value=("dump", [])):
            upstream = probe.return_value.__enter__.return_value
            upstream.fetch.return_value = Sample("source", "v2", 0, blank)
            observation = gc.Session._observe(session, source, dump)
            self.assertEqual(observation.seen, {"source": "menu", "vosu": "menu"})
            self.assertEqual((observation.source, observation.vosu), (BLANK, READY))
            self.assertFalse(session._holds("menu", observation))
            upstream.fetch.return_value = Sample("source", "v2", 0, ready)
            self.assertTrue(session._holds("menu", gc.Session._observe(session, source, dump)))

    def test_client_barriers_wait_for_matching_ready_sources(self) -> None:
        for index, barrier in enumerate(pgb.TRAVERSAL[:-1]):
            with self.subTest(barrier=barrier):
                seen = {"source": barrier, "vosu": barrier}
                ready = observe(seen, "same", None)
                session, calls = self.session([
                    ready, ready,
                    observe(seen, "same", None, source=BLANK),
                    observe(seen, "same", None, vosu=BLANK),
                    observe(seen, "same", None, source=pgb.ClientFields("wrong", "", "game-root")),
                    ready, ready, ready,
                ])
                with mock.patch.object(pgb, "POLL_S", 0):
                    session._wait_barrier(barrier, None, None, list(pgb.TRAVERSAL[:index]),
                                          identical_views=barrier != "play")
                self.assertEqual(len(calls), 8)
                self.assertEqual([b["barrier"] for b in session.transcript["barriers"]], [barrier])

    def test_matching_beatmap_path_can_substitute_for_missing_checksum(self) -> None:
        seen = {"source": "menu", "vosu": "menu"}
        source = pgb.ClientFields("", "map.osu", "game-root")
        vosu = pgb.ClientFields("chart-checksum", "map.osu", "game-root")
        session, _ = self.session([])
        self.assertTrue(session._holds("menu", observe(seen, "same", None, source=source, vosu=vosu)))
        no_folder = pgb.ClientFields("", "map.osu", "")
        self.assertFalse(session._holds("menu", observe(seen, "same", None, source=no_folder, vosu=vosu)))

    def test_no_client_keeps_requiring_exit_not_client_fields(self) -> None:
        observation = observe({"source": "no-client", "vosu": "no-client"}, "same", None,
                              source=BLANK, vosu=BLANK)
        session, calls = self.session([observation] * 3)
        self.assertFalse(session._holds("no-client", observation))
        session.client.exited.return_value = True
        with mock.patch.object(pgb, "POLL_S", 0):
            session._wait_barrier("no-client", None, None, list(pgb.TRAVERSAL[:-1]))
        self.assertEqual(len(calls), 3)

    def test_upstream_unready_fails_30_seconds_after_vosu_is_ready(self) -> None:
        seen = {"source": "menu", "vosu": "menu"}
        session, calls = self.session([
            observe(seen, "same", None, source=BLANK, vosu=BLANK),
            observe(seen, "same", None, source=BLANK, vosu=BLANK),
            *[observe(seen, "same", None, source=BLANK)] * 3,
        ])
        session.deadline = 3600
        # 100 s spent waiting for vosu do not count; upstream gets 30 s from then.
        with mock.patch.object(pgb.time, "monotonic", side_effect=[0, 99, 100, 100, 129, 129, 130, 130]), \
                mock.patch.object(pgb.time, "sleep"), \
                self.assertRaisesRegex(pgb.CaptureError,
                                       r"upstream_not_reading_client.*beatmap\.checksum.*folders\.game.*30 s"):
            session._wait_barrier("menu", None, None, [])
        self.assertEqual(len(calls), 5)
        self.assertEqual(session.transcript["barriers"], [])

    def test_upstream_reading_the_wrong_beatmap_also_fails_fast(self) -> None:
        wrong_map = pgb.ClientFields("wrong", "", "game-root")
        waiting = observe({"source": "menu", "vosu": "menu"}, "same", None, source=wrong_map)
        session, _ = self.session([waiting] * 2)
        session.deadline = 3600
        with mock.patch.object(pgb.time, "monotonic", side_effect=[0, 0, 30, 30]), \
                mock.patch.object(pgb.time, "sleep"), \
                self.assertRaisesRegex(pgb.CaptureError,
                                       r"upstream_not_reading_client.*beatmap\.checksum.*does not match vosu"):
            session._wait_barrier("menu", None, None, [])
        self.assertEqual(session.transcript["barriers"], [])


class PlayBarrierPregamePauseTests(unittest.TestCase):
    """GOLDEN_CONTRACT.md §6 / owner amendment 2026-11-26: the play barrier tolerates
    only an upstream `paused` observed before its clock has ever advanced."""

    def session(self, observations: list, *, timeout: float = 5) -> tuple[gc.Session, list]:
        client = mock.Mock()
        client.exited.return_value = False
        session = gc.Session({"golden": {}}, gc.SCENARIO_SETS["m4b-dry-run-v1"], Path("x"),
                             timeout=timeout, build=None, client=client, operator=lambda b, m: None)
        calls: list = []

        def next_observation(_source, _dump):
            calls.append(1)
            return observe(*observations[len(calls) - 1])
        session._observe = next_observation
        return session, calls

    def test_tolerates_upstream_paused_before_gameplay(self) -> None:
        # PlayerLoader reports `play` with a clock that has not moved yet, which
        # upstream's own rule (`game.paused = playTime == previousPlayTime`) reports as
        # `paused`; the barrier tolerates that and still reaches `play`.
        observations = [
            ({"source": "paused", "vosu": "play"}, None, 0.0),
            ({"source": "play", "vosu": "play"}, None, 100.0),
            ({"source": "play", "vosu": "play"}, None, 100.0),
            ({"source": "play", "vosu": "play"}, None, 100.0),
        ]
        session, calls = self.session(observations)
        with mock.patch.object(pgb, "POLL_S", 0):
            session._wait_barrier("play", None, None, ["menu"], identical_views=False)
        self.assertEqual(len(calls), 4)
        self.assertEqual(session.transcript["barriers"][0]["pregame_pauses_tolerated"], 1)

    def test_aborts_on_paused_once_gameplay_started(self) -> None:
        # "Gameplay started" is defined from the observed clock: once it has advanced
        # even once, a later upstream `paused` is a real out-of-order observation again.
        observations = [
            ({"source": "paused", "vosu": "play"}, None, 0.0),
            ({"source": "play", "vosu": "play"}, None, 100.0),
            ({"source": "paused", "vosu": "paused"}, None, 100.0),
        ]
        session, _calls = self.session(observations)
        with mock.patch.object(pgb, "POLL_S", 0), \
                self.assertRaisesRegex(pgb.CaptureError,
                                        "barrier out of order: source reached paused"):
            session._wait_barrier("play", None, None, ["menu"], identical_views=False)

    def test_paused_barrier_after_gameplay_is_recorded_normally(self) -> None:
        # Once `play` held after the tolerated pre-gameplay pause, a later upstream
        # `paused` is the real `paused` barrier transition: recorded normally, with no
        # tolerance of its own (the pregame tolerance only ever applies to `play`).
        observations = [
            ({"source": "paused", "vosu": "play"}, None, 0.0),
            ({"source": "play", "vosu": "play"}, None, 100.0),
            ({"source": "play", "vosu": "play"}, None, 100.0),
            ({"source": "play", "vosu": "play"}, None, 100.0),
            ({"source": "paused", "vosu": "paused"}, None, 100.0),
            ({"source": "paused", "vosu": "paused"}, None, 100.0),
            ({"source": "paused", "vosu": "paused"}, None, 100.0),
        ]
        session, calls = self.session(observations)
        reached: list[str] = ["menu"]
        with mock.patch.object(pgb, "POLL_S", 0):
            session._wait_barrier("play", None, None, reached, identical_views=False)
            session._wait_barrier("paused", None, None, reached, identical_views=False)
        self.assertEqual(len(calls), 7)
        self.assertEqual(reached, ["menu", "play", "paused"])
        barriers = {b["barrier"]: b for b in session.transcript["barriers"]}
        self.assertEqual(barriers["play"]["pregame_pauses_tolerated"], 1)
        self.assertEqual(barriers["paused"]["pregame_pauses_tolerated"], 0)

    def test_vosu_side_paused_while_waiting_for_play_still_aborts(self) -> None:
        # The pre-gameplay tolerance is upstream-only.
        session, _calls = self.session([({"source": "play", "vosu": "paused"}, None, None)])
        with mock.patch.object(pgb, "POLL_S", 0), \
                self.assertRaisesRegex(pgb.CaptureError,
                                        "barrier out of order: vosu reached paused"):
            session._wait_barrier("play", None, None, ["menu"], identical_views=False)

    def test_skipping_past_paused_while_waiting_for_play_still_aborts(self) -> None:
        session, _calls = self.session([({"source": "result", "vosu": "result"}, None, None)])
        with mock.patch.object(pgb, "POLL_S", 0), \
                self.assertRaisesRegex(pgb.CaptureError,
                                        "barrier out of order: source reached result"):
            session._wait_barrier("play", None, None, ["menu"], identical_views=False)

    def test_paused_seen_while_waiting_for_an_earlier_barrier_still_aborts(self) -> None:
        # The tolerance only ever applies while waiting for `play`.
        session, _calls = self.session([({"source": "paused", "vosu": "paused"}, None, None)])
        with mock.patch.object(pgb, "POLL_S", 0), \
                self.assertRaisesRegex(pgb.CaptureError,
                                        "barrier out of order: source reached paused while "
                                        "waiting for menu"):
            session._wait_barrier("menu", None, None, [], identical_views=False)


if __name__ == "__main__":
    unittest.main()
