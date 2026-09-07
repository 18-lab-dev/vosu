from __future__ import annotations

import unittest

from validation_error import ValidationError
from input_validation import navigate_ui, navigation_key_names
from run import build_parser


class RecordingBackend:
    def __init__(self) -> None:
        self.events: list[tuple[str, bool]] = []
        self.closed = False
        self.now = 0.0
        self.down_times: list[tuple[str, float]] = []

    def emit(self, key: str, pressed: bool) -> None:
        self.events.append((key, pressed))
        if pressed:
            self.down_times.append((key, self.now))

    def sleep(self, duration: float) -> None:
        self.now += duration

    def close(self) -> None:
        self.closed = True


class UiNavigationTests(unittest.TestCase):
    def test_lazer_song_select_uses_three_presses_with_100ms_gaps(self) -> None:
        backend = RecordingBackend()
        navigate_ui("lazer", "song-select", backend, sleep=backend.sleep)

        self.assertEqual(
            backend.events,
            [("P", True), ("P", False)] * 3,
        )
        self.assertEqual(backend.down_times, [("P", 0.0), ("P", 0.1), ("P", 0.2)])
        self.assertTrue(backend.closed)

    def test_client_specific_multiplayer_paths(self) -> None:
        self.assertEqual(
            navigation_key_names("stable", "multiplayer", None),
            ("P", "M"),
        )
        self.assertEqual(
            navigation_key_names("lazer", "multiplayer", None),
            ("P", "M"),
        )

        stable = RecordingBackend()
        lazer = RecordingBackend()
        navigate_ui("stable", "multiplayer", stable, sleep=lambda _: None)
        navigate_ui("lazer", "multiplayer", lazer, sleep=lambda _: None)
        self.assertEqual(
            stable.events,
            [("P", True), ("P", False)] * 2 + [("M", True), ("M", False)],
        )
        self.assertEqual(
            lazer.events,
            [("P", True), ("P", False)] * 2 + [("M", True), ("M", False)] * 2,
        )

    def test_song_search_types_directly_with_1ms_onset_gaps(self) -> None:
        backend = RecordingBackend()

        navigate_ui(
            "lazer",
            "song-select",
            backend,
            search="a-1",
            transition_delay=0.5,
            sleep=backend.sleep,
        )

        self.assertEqual(
            backend.events[-6:],
            [
                ("A", True),
                ("A", False),
                ("MINUS", True),
                ("MINUS", False),
                ("1", True),
                ("1", False),
            ],
        )
        search_downs = backend.down_times[-3:]
        self.assertEqual([key for key, _ in search_downs], ["A", "MINUS", "1"])
        self.assertAlmostEqual(search_downs[1][1] - search_downs[0][1], 0.001)
        self.assertAlmostEqual(search_downs[2][1] - search_downs[1][1], 0.001)
        self.assertTrue(backend.closed)

    def test_search_is_rejected_outside_song_select_and_backend_closes(self) -> None:
        backend = RecordingBackend()
        with self.assertRaisesRegex(ValidationError, "search requires song-select"):
            navigate_ui("lazer", "multiplayer", backend, search="map")
        self.assertTrue(backend.closed)

    def test_navigation_cli_defaults_match_client_timing(self) -> None:
        args = build_parser().parse_args(
            ["navigate", "--client", "lazer", "--target", "song-select"]
        )
        self.assertEqual(args.key_delay, 0.1)
        self.assertEqual(args.typing_delay, 0.001)
        self.assertEqual(args.transition_delay, 1.0)


if __name__ == "__main__":
    unittest.main()
