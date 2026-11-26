"""Unit tests for parity_golden_barrier.py (barrier wait/observe, split out of
parity_golden_capture.py: docs/architecture/source-map.md's 1,000-line maintained-file
limit). `Session._wait_barrier`/`_observe` are exercised directly with a mocked
`_observe`, so these need neither the fake-daemon harness nor a real client."""
from __future__ import annotations

from pathlib import Path
import sys
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import parity_golden_barrier as pgb
import parity_golden_capture as gc


class BarrierTests(unittest.TestCase):
    def session(self, observations: list, *, timeout: float = 5) -> tuple[gc.Session, list]:
        client = mock.Mock()
        client.exited.return_value = False
        session = gc.Session({"golden": {}}, gc.SCENARIO_SETS["m4b-dry-run-v1"], Path("x"),
                             timeout=timeout, build=None, client=client, operator=lambda b, m: None)
        calls: list = []

        def observe(_source, _dump):
            calls.append(1)
            return observations[len(calls) - 1]
        session._observe = observe
        return session, calls

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


class PlayBarrierPregamePauseTests(unittest.TestCase):
    """GOLDEN_CONTRACT.md §6 / owner amendment 2026-11-26: the play barrier tolerates
    only an upstream `paused` observed before its clock has ever advanced."""

    def session(self, observations: list, *, timeout: float = 5) -> tuple[gc.Session, list]:
        client = mock.Mock()
        client.exited.return_value = False
        session = gc.Session({"golden": {}}, gc.SCENARIO_SETS["m4b-dry-run-v1"], Path("x"),
                             timeout=timeout, build=None, client=client, operator=lambda b, m: None)
        calls: list = []

        def observe(_source, _dump):
            calls.append(1)
            return observations[len(calls) - 1]
        session._observe = observe
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
