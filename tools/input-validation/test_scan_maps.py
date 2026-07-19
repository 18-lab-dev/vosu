from __future__ import annotations

import io
import math
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

from input_validation import (
    DryRunBackend,
    ManiaEvent,
    ValidationError,
    ManiaReplay,
    autoplay,
    parse_key_mapping,
    parse_mania_replay,
    scan_maps,
)


class ScanMapsTests(unittest.TestCase):
    def test_filters_mode_counts_bits_and_sorts_ratios(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            songs = Path(temporary)
            self.write_map(songs / "normal.osu", 3, [1, 1, 128])
            self.write_map(songs / "nested" / "infinite.osu", 3, [128, 128])
            self.write_map(songs / "other-mode.osu", 0, [128] * 10)
            self.write_map(songs / "mixed-bit.osu", 3, [129, 1])

            result = scan_maps(songs, min_ln_ratio=0.5, limit=10)

            self.assertEqual([Path(item["path"]).name for item in result], ["infinite.osu", "mixed-bit.osu", "normal.osu"])
            self.assertTrue(math.isinf(result[0]["lnNormalRatio"]))
            self.assertEqual((result[1]["longNotes"], result[1]["normalNotes"], result[1]["lnNormalRatio"]), (1, 2, 0.5))

    def test_limit_is_applied_after_deterministic_sort(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            songs = Path(temporary)
            self.write_map(songs / "b.osu", 3, [128, 1])
            self.write_map(songs / "a.osu", 3, [128, 1])
            result = scan_maps(songs, min_ln_ratio=0, limit=1)
            self.assertEqual(Path(result[0]["path"]).name, "a.osu")

    def test_mania_replay_parses_columns_normal_notes_and_holds(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "map.osu"
            self.write_mania_map(path, [
                "0,192,100,1,0,0:0:0:0:",
                "128,192,200,128,0,500:0:0:0:0:",
                "511,192,300,1,0,0:0:0:0:",
            ])
            replay = parse_mania_replay(path, tap_duration_ms=25)
            self.assertEqual(replay.key_count, 4)
            self.assertEqual(replay.events, (
                ManiaEvent(100, 0, True), ManiaEvent(125, 0, False),
                ManiaEvent(200, 1, True), ManiaEvent(300, 3, True),
                ManiaEvent(325, 3, False), ManiaEvent(500, 1, False),
            ))

    def test_dense_taps_shorten_and_simultaneous_handoff_retriggers(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "map.osu"
            self.write_mania_map(path, [
                "64,192,100,1,0,0:0:0:0:",
                "64,192,200,1,0,0:0:0:0:",
                "64,192,350,128,0,500:0:0:0:0:",
            ])
            replay = parse_mania_replay(path, tap_duration_ms=150)
            self.assertEqual(replay.events, (
                ManiaEvent(100, 0, True),
                ManiaEvent(200, 0, False),
                ManiaEvent(200, 0, True),
                ManiaEvent(350, 0, False),
                ManiaEvent(350, 0, True),
                ManiaEvent(500, 0, False),
            ))
            self.assertEqual(sum(event.pressed for event in replay.events), 3)

    def test_parser_rejects_unplayable_same_column_overlap(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "map.osu"
            self.write_mania_map(path, [
                "64,192,100,128,0,300:0:0:0:0:",
                "64,192,150,1,0,0:0:0:0:",
            ])
            with self.assertRaisesRegex(ValidationError, "hold overlaps"):
                parse_mania_replay(path)

    def test_parser_validates_mode_circle_size_and_mapping(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "map.osu"
            self.write_mania_map(path, [], mode=0)
            with self.assertRaisesRegex(ValidationError, "Mode must be 3"):
                parse_mania_replay(path)
            self.write_mania_map(path, [], circle_size="4.0")
            with self.assertRaisesRegex(ValidationError, "integer"):
                parse_mania_replay(path)
            self.write_mania_map(path, ["512,192,100,1,0,0:0:0:0:"])
            replay = parse_mania_replay(path)
            self.assertEqual(replay.events[0], ManiaEvent(100, 3, True))
            self.write_mania_map(path, ["64,192,100,8,0,200:0:0:0:0:"])
            with self.assertRaisesRegex(ValidationError, "unsupported hit object type"):
                parse_mania_replay(path)
            self.write_mania_map(path, [], circle_size="11")
            with self.assertRaisesRegex(ValidationError, "above 10"):
                parse_mania_replay(path)
        self.assertEqual(parse_key_mapping(None, 4), ("D", "F", "J", "K"))
        self.assertEqual(parse_key_mapping("a,s,k,l", 4), ("A", "S", "K", "L"))
        with self.assertRaisesRegex(ValidationError, "exactly 4"):
            parse_key_mapping("D,F,J", 4)
        with self.assertRaisesRegex(ValidationError, "duplicate"):
            parse_key_mapping("D,D,J,K", 4)
        with self.assertRaisesRegex(ValidationError, "required"):
            parse_key_mapping(None, 7)

    def test_dry_run_prints_plan_without_timing(self) -> None:
        replay_events = (
            ManiaEvent(10, 0, True),
            ManiaEvent(50, 0, False),
        )

        output = io.StringIO()
        backend = DryRunBackend()
        backend.output = lambda line: print(line, file=output)
        with redirect_stdout(io.StringIO()):
            autoplay(ManiaReplay(4, replay_events), ("D", "F", "J", "K"), backend, 99.0)
        self.assertEqual(output.getvalue().splitlines(), [
            "     10 ms  C1 D down",
            "     50 ms  C1 D up",
        ])

    def test_autoplay_releases_active_keys_and_closes_after_error(self) -> None:
        class FailingBackend:
            def __init__(self) -> None:
                self.events: list[tuple[str, bool]] = []
                self.closed = False

            def emit(self, key: str, pressed: bool) -> None:
                self.events.append((key, pressed))
                if len(self.events) == 2:
                    raise KeyboardInterrupt

            def close(self) -> None:
                self.closed = True

        backend = FailingBackend()
        replay = ManiaReplay(1, (
            ManiaEvent(0, 0, True),
            ManiaEvent(0, 0, False),
        ))
        with self.assertRaises(KeyboardInterrupt):
            autoplay(replay, ("D",), backend, 0)
        self.assertEqual(backend.events, [("D", True), ("D", False), ("D", False)])
        self.assertTrue(backend.closed)

    def test_autoplay_attempts_every_release_when_cleanup_fails(self) -> None:
        class FailingReleaseBackend:
            def __init__(self) -> None:
                self.events: list[tuple[str, bool]] = []
                self.closed = False

            def emit(self, key: str, pressed: bool) -> None:
                self.events.append((key, pressed))
                if key == "D" and not pressed:
                    raise OSError("release failed")

            def close(self) -> None:
                self.closed = True

        backend = FailingReleaseBackend()
        replay = ManiaReplay(2, (
            ManiaEvent(0, 0, True),
            ManiaEvent(0, 1, True),
            ManiaEvent(1, 0, False),
        ))
        with self.assertRaisesRegex(OSError, "release failed"):
            autoplay(replay, ("D", "F"), backend, 0)
        self.assertEqual(backend.events, [
            ("D", True), ("F", True), ("D", False),
            ("D", False), ("F", False),
        ])
        self.assertTrue(backend.closed)

    @staticmethod
    def write_map(path: Path, mode: int, types: list[int]) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        objects = "\n".join(f"64,192,{index},{kind},0,0:0:0:0:" for index, kind in enumerate(types))
        path.write_text(f"osu file format v14\n\n[General]\nMode:{mode}\n\n[HitObjects]\n{objects}\n", encoding="utf-8")

    @staticmethod
    def write_mania_map(path: Path, objects: list[str], mode: int = 3, circle_size: str = "4") -> None:
        path.write_text(
            "osu file format v14\n\n"
            f"[General]\nMode:{mode}\n\n"
            f"[Difficulty]\nCircleSize:{circle_size}\n\n"
            f"[HitObjects]\n{chr(10).join(objects)}\n",
            encoding="utf-8",
        )


if __name__ == "__main__":
    unittest.main()
