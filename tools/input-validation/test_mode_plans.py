from __future__ import annotations

import tempfile
import math
import unittest
from pathlib import Path

from input_validation import ValidationError
from mode_plans import KeyAction, PlayPlan, PointerMove, parse_play_plan


class ModePlanTests(unittest.TestCase):
    def parse(self, body: str, tap_duration_ms: int = 40) -> PlayPlan:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "map.osu"
            path.write_text(body, encoding="utf-8")
            return parse_play_plan(path, tap_duration_ms)

    @staticmethod
    def map(mode: int, objects: str, *, difficulty: str = "", timing: str = "") -> str:
        return (
            "osu file format v14\n"
            "[General]\n"
            f"Mode:{mode}\n"
            "[Difficulty]\n"
            f"{difficulty}"
            "[TimingPoints]\n"
            f"{timing}"
            "[HitObjects]\n"
            f"{objects}"
        )

    def test_mania_circles_holds_columns_and_tap_clipping(self) -> None:
        plan = self.parse(
            self.map(
                3,
                "0,192,100,1,0,0:0:0:0:\n"
                "511,192,100,128,0,250:0:0:0:0:\n"
                "0,192,130,1,0,0:0:0:0:\n",
                difficulty="CircleSize:4\n",
            )
        )
        self.assertEqual(plan.controls, ("C0", "C1", "C2", "C3"))
        self.assertEqual(
            plan.actions,
            (
                KeyAction(100, "C0", True),
                KeyAction(100, "C3", True),
                KeyAction(130, "C0", False),
                KeyAction(130, "C0", True),
                KeyAction(170, "C0", False),
                KeyAction(250, "C3", False),
            ),
        )

    def test_mania_rejects_simultaneous_and_overlapping_notes(self) -> None:
        simultaneous = self.map(3, "100,0,10,1,0\n120,0,10,1,0\n", difficulty="CircleSize:4\n")
        with self.assertRaisesRegex(ValidationError, "simultaneously"):
            self.parse(simultaneous)
        overlap = self.map(3, "100,0,10,128,0,100:0\n100,0,99,1,0\n", difficulty="CircleSize:4\n")
        with self.assertRaisesRegex(ValidationError, "overlaps"):
            self.parse(overlap)

    def test_taiko_hit_sound_sides_alternate_independently(self) -> None:
        plan = self.parse(
            self.map(1, "256,192,10,1,0\n256,192,20,1,4\n256,192,30,1,2\n256,192,40,1,8\n")
        )
        presses = [action for action in plan.actions if isinstance(action, KeyAction) and action.pressed]
        self.assertEqual(
            [action.control for action in presses],
            ["DON_LEFT", "DON_RIGHT", "KAT_LEFT", "KAT_RIGHT"],
        )

    def test_taiko_slider_and_spinner_generate_bounded_beat_rolls(self) -> None:
        plan = self.parse(
            self.map(
                1,
                "0,0,100,2,0,L|100:0,2,100\n256,192,500,8,0,750\n",
                difficulty="SliderMultiplier:1\nSliderTickRate:2\n",
                timing="0,100,4,2,0,100,1,0\n",
            ),
            tap_duration_ms=10,
        )
        presses = [action for action in plan.actions if isinstance(action, KeyAction) and action.pressed]
        self.assertEqual([action.timestamp_ms for action in presses], [100, 150, 200, 250, 500, 525, 550, 575, 600, 625, 650, 675, 700, 725])
        self.assertEqual([action.control for action in presses[:4]], ["DON_LEFT", "DON_RIGHT", "DON_LEFT", "DON_RIGHT"])

    def test_osu_circle_orders_move_before_press(self) -> None:
        plan = self.parse(self.map(0, "12,34,100,1,0\n"))
        self.assertEqual(
            plan.actions,
            (PointerMove(100, 12.0, 34.0), KeyAction(100, "PRIMARY", True), KeyAction(140, "PRIMARY", False)),
        )

    def test_osu_slider_holds_and_samples_repeated_path(self) -> None:
        plan = self.parse(
            self.map(
                0,
                "0,0,100,2,0,L|100:0,2,100\n",
                difficulty="SliderMultiplier:1\nSliderTickRate:2\n",
                timing="0,100,4,2,0,100,1,0\n",
            )
        )
        moves = [action for action in plan.actions if isinstance(action, PointerMove)]
        self.assertEqual((moves[0], moves[-1]), (PointerMove(100, 0.0, 0.0), PointerMove(300, 0.0, 0.0)))
        self.assertTrue(all(right.timestamp_ms - left.timestamp_ms <= 16 for left, right in zip(moves, moves[1:])))
        turn = next(move for move in moves if move.timestamp_ms == 200)
        self.assertEqual(turn, PointerMove(200, 100.0, 0.0))
        self.assertIn(KeyAction(100, "PRIMARY", True), plan.actions)
        self.assertIn(KeyAction(300, "PRIMARY", False), plan.actions)

    def test_osu_bezier_slider_uses_curved_arc_length_path(self) -> None:
        plan = self.parse(
            self.map(
                0,
                "100,100,0,2,0,B|200:200|300:100,1,229.5\n",
                difficulty="SliderMultiplier:2.390625\nSliderTickRate:1\n",
                timing="0,100,4,2,0,100,1,0\n",
            )
        )
        midpoint = next(
            action
            for action in plan.actions
            if isinstance(action, PointerMove) and action.timestamp_ms == 48
        )
        self.assertAlmostEqual(midpoint.x, 200.0, delta=2.0)
        self.assertAlmostEqual(midpoint.y, 150.0, delta=2.0)

    def test_osu_spinner_holds_and_emits_multi_rotation_circle(self) -> None:
        plan = self.parse(self.map(0, "256,192,100,8,0,1100\n"))
        moves = [action for action in plan.actions if isinstance(action, PointerMove)]
        self.assertGreater(len(moves), 60)
        self.assertLess(len(moves), 70)
        self.assertTrue(all(0 <= move.x <= 512 and 0 <= move.y <= 384 for move in moves))
        self.assertTrue(all(right.timestamp_ms - left.timestamp_ms <= 16 for left, right in zip(moves, moves[1:])))
        angles = [math.atan2(move.y - 192.0, move.x - 256.0) for move in moves]
        unwrapped = [angles[0]]
        for angle in angles[1:]:
            while angle < unwrapped[-1] - math.pi:
                angle += 2 * math.pi
            while angle > unwrapped[-1] + math.pi:
                angle -= 2 * math.pi
            unwrapped.append(angle)
        self.assertGreater(unwrapped[-1] - unwrapped[0], 11 * math.pi)
        self.assertIn(KeyAction(100, "PRIMARY", True), plan.actions)
        self.assertIn(KeyAction(1100, "PRIMARY", False), plan.actions)

    def test_catch_circles_and_sliders_move_horizontally_but_spinners_do_nothing(self) -> None:
        plan = self.parse(
            self.map(
                2,
                "20,30,100,1,0\n0,0,200,2,0,L|100:0,1,100\n256,192,400,8,0,500\n",
                difficulty="SliderMultiplier:1\nSliderTickRate:2\n",
                timing="0,100,4,2,0,100,1,0\n",
            )
        )
        self.assertEqual(plan.controls, ())
        moves = [action for action in plan.actions if isinstance(action, PointerMove)]
        self.assertEqual((moves[0], moves[1], moves[-1]), (PointerMove(100, 20.0, 384.0), PointerMove(200, 0.0, 384.0), PointerMove(300, 100.0, 384.0)))
        self.assertTrue(all(right.timestamp_ms - left.timestamp_ms <= 16 for left, right in zip(moves[1:], moves[2:])))

    def test_inherited_timing_changes_slider_duration(self) -> None:
        plan = self.parse(
            self.map(
                2,
                "0,0,100,2,0,L|100:0,1,100\n",
                difficulty="SliderMultiplier:1\nSliderTickRate:1\n",
                timing="0,100,4,2,0,100,1,0\n50,-50,4,2,0,100,0,0\n",
            )
        )
        self.assertEqual(plan.actions[-1].timestamp_ms, 150)

    def test_negative_timing_offset_is_valid(self) -> None:
        plan = self.parse(
            self.map(
                2,
                "0,0,0,2,0,L|100:0,1,100\n",
                difficulty="SliderMultiplier:1\nSliderTickRate:1\n",
                timing="-100,100,4,2,0,100,1,0\n",
            )
        )
        self.assertEqual(plan.actions[-1].timestamp_ms, 100)

    def test_actions_are_deterministically_sorted(self) -> None:
        source = self.map(0, "30,40,200,1,0\n10,20,100,1,0\n")
        first = self.parse(source)
        second = self.parse(source)
        self.assertEqual(first, second)
        self.assertEqual([action.timestamp_ms for action in first.actions], sorted(action.timestamp_ms for action in first.actions))

    def test_rejects_malformed_required_fields_and_objects(self) -> None:
        cases = (
            ("[General]\nMode:no\n", "Mode"),
            (self.map(4, ""), "unsupported Mode"),
            (self.map(3, "0,0,1,1,0\n", difficulty="CircleSize:4.5\n"), "CircleSize"),
            (self.map(0, "0,0,1,2,0,L|1:1,1,10\n", difficulty="SliderTickRate:1\n"), "SliderMultiplier"),
            (self.map(0, "513,0,1,1,0\n"), "invalid hit object"),
            (self.map(0, "0,0,-1,1,0\n"), "hit object time"),
            (self.map(0, "0,0,1,8,0,1\n"), "spinner must end"),
            (self.map(0, "0,0,1,128,0,10:0\n"), "unsupported hit object"),
            (self.map(0, "0,0,1,3,0\n"), "unsupported hit object"),
            (self.map(0, "0,0,10,8,0,20\n", timing="0,0,4,2,0,100,1,0\n"), "timing point"),
            (self.map(0, "", timing="0,100,bad,2,0,100,1,0\n"), "timing point meter"),
        )
        for source, message in cases:
            with self.subTest(message=message), self.assertRaisesRegex(ValidationError, message):
                self.parse(source)

    def test_rejects_oversized_sampling_and_action_times(self) -> None:
        oversized = self.map(
            0,
            "0,0,0,2,0,L|512:384,10000,1000000\n",
            difficulty="SliderMultiplier:0.01\nSliderTickRate:10000\n",
            timing="0,1000,4,2,0,100,1,0\n",
        )
        with self.assertRaisesRegex(ValidationError, "duration|samples"):
            self.parse(oversized)
        excessive_samples = self.map(
            0,
            "0,0,0,2,0,L|100:0,1,200000\n",
            difficulty="SliderMultiplier:1\nSliderTickRate:1\n",
            timing="0,100,4,2,0,100,1,0\n",
        )
        with self.assertRaisesRegex(ValidationError, "too many samples"):
            self.parse(excessive_samples)
        with self.assertRaisesRegex(ValidationError, "action time"):
            self.parse(self.map(0, "0,0,86400000,1,0\n"))

    def test_tap_duration_validation(self) -> None:
        source = self.map(0, "0,0,0,1,0\n")
        for value in (0, -1, True, 1.5):
            with self.subTest(value=value), self.assertRaisesRegex(ValidationError, "tap duration"):
                self.parse(source, value)  # type: ignore[arg-type]


if __name__ == "__main__":
    unittest.main()
