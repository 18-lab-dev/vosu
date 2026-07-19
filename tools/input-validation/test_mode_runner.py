from __future__ import annotations

import unittest
from unittest import mock

import mode_runner
from input_validation import DryRunBackend, ValidationError
from mode_plans import KeyAction, PlayPlan, PointerMove
from mode_runner import (
    PlanBackends,
    create_plan_backends,
    jittered_plan_actions,
    key_mapping_for_plan,
    live_play_plan,
    parse_rectangle,
    play_plan,
)
from pointer_backends import DryRunPointerBackend, PlayfieldRect


class RecordingKeyboard:
    def __init__(self) -> None:
        self.actions: list[tuple[str, bool]] = []
        self.closed = False

    def emit(self, key: str, pressed: bool) -> None:
        self.actions.append((key, pressed))

    def close(self) -> None:
        self.closed = True


class RecordingPointer:
    def __init__(self) -> None:
        self.actions: list[tuple[float, float]] = []
        self.closed = False

    def move(self, x: float, y: float) -> None:
        self.actions.append((x, y))

    def close(self) -> None:
        self.closed = True


class ModeRunnerTests(unittest.TestCase):
    def test_mode_specific_default_key_order(self) -> None:
        self.assertEqual(
            key_mapping_for_plan(PlayPlan(0, ("PRIMARY",), ()), None),
            ("Z",),
        )
        self.assertEqual(
            key_mapping_for_plan(
                PlayPlan(1, ("DON_LEFT", "DON_RIGHT", "KAT_LEFT", "KAT_RIGHT"), ()),
                None,
            ),
            ("F", "J", "D", "K"),
        )
        self.assertEqual(key_mapping_for_plan(PlayPlan(2, (), ()), None), ())

    def test_pointer_only_catch_rejects_keys(self) -> None:
        with self.assertRaisesRegex(ValidationError, "pointer-only catch"):
            key_mapping_for_plan(PlayPlan(2, (), ()), "Z")

    def test_rectangle_parser_requires_four_integers(self) -> None:
        self.assertEqual(parse_rectangle("-10,20,513,385", "--playfield"), PlayfieldRect(-10, 20, 513, 385))
        with self.assertRaisesRegex(ValidationError, "x,y,width,height"):
            parse_rectangle("0,0,100", "--desktop")
        with self.assertRaisesRegex(ValidationError, "invalid --desktop"):
            parse_rectangle("0,0,no,100", "--desktop")

    def test_dry_run_pointer_plan_needs_no_geometry_and_prints_combined_order(self) -> None:
        plan = PlayPlan(
            0,
            ("PRIMARY",),
            (
                PointerMove(100, 256.0, 192.0),
                KeyAction(100, "PRIMARY", True),
                KeyAction(140, "PRIMARY", False),
            ),
        )
        backends = create_plan_backends(plan, "dry-run", ("Z",), None, None, None)
        self.assertIsInstance(backends.keyboard, DryRunBackend)
        self.assertIsInstance(backends.pointer, DryRunPointerBackend)
        output: list[str] = []

        play_plan(plan, ("Z",), backends, 0.0, output=output.append)

        self.assertEqual(
            output,
            [
                "    100 ms  POINTER 256.000,192.000",
                "    100 ms  PRIMARY Z down",
                "    140 ms  PRIMARY Z up",
            ],
        )

    def test_live_plan_dispatches_pointer_keys_and_start_then_cleans_up(self) -> None:
        plan = PlayPlan(
            0,
            ("PRIMARY",),
            (
                PointerMove(100, 10.0, 20.0),
                KeyAction(100, "PRIMARY", True),
                KeyAction(140, "PRIMARY", False),
            ),
        )
        keyboard = RecordingKeyboard()
        pointer = RecordingPointer()

        def run_timeline(scheduled, emit, base_url, start, start_timeout):
            self.assertEqual(base_url, "http://127.0.0.1:24050")
            self.assertEqual(start_timeout, 12.0)
            self.assertEqual([timestamp for timestamp, _ in scheduled], [100.0, 100.0, 140.0])
            assert start is not None
            start()
            for _, action in scheduled:
                emit(action)

        with mock.patch.object(mode_runner, "run_live_timeline", side_effect=run_timeline):
            live_play_plan(
                plan,
                ("Z",),
                PlanBackends(keyboard, pointer),
                "http://127.0.0.1:24050",
                "ENTER",
                12.0,
                sleep=lambda _: None,
            )

        self.assertEqual(
            keyboard.actions,
            [("ENTER", True), ("ENTER", False), ("Z", True), ("Z", False)],
        )
        self.assertEqual(pointer.actions, [(10.0, 20.0)])
        self.assertTrue(keyboard.closed)
        self.assertTrue(pointer.closed)

    def test_mania_jitter_is_paired_deterministic_and_other_modes_reject_it(self) -> None:
        plan = PlayPlan(
            3,
            ("C0",),
            (KeyAction(100, "C0", True), KeyAction(140, "C0", False)),
        )
        first = jittered_plan_actions(plan, 5.0, 7)
        second = jittered_plan_actions(plan, 5.0, 7)
        self.assertEqual(first, second)
        self.assertAlmostEqual(first[1][0] - first[0][0], 40.0)
        with self.assertRaisesRegex(ValidationError, "only for osu!mania"):
            jittered_plan_actions(PlayPlan(0, ("PRIMARY",), ()), 1.0, 0)


if __name__ == "__main__":
    unittest.main()
