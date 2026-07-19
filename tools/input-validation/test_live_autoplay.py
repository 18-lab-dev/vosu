from __future__ import annotations

import unittest

from input_validation import (
    ManiaEvent,
    ManiaReplay,
    ValidationError,
    jittered_events,
    live_autoplay,
    parse_live_state,
)
from run import build_parser


class FakeClock:
    def __init__(self) -> None:
        self.now = 0.0

    def monotonic(self) -> float:
        return self.now

    def sleep(self, duration: float) -> None:
        self.now += duration


class RecordingBackend:
    def __init__(self) -> None:
        self.events: list[tuple[str, bool]] = []
        self.closed = False

    def emit(self, key: str, pressed: bool) -> None:
        self.events.append((key, pressed))

    def close(self) -> None:
        self.closed = True

class TimedRecordingBackend(RecordingBackend):
    def __init__(self, clock: FakeClock) -> None:
        super().__init__()
        self.clock = clock
        self.timed_events: list[tuple[float, str, bool]] = []

    def emit(self, key: str, pressed: bool) -> None:
        super().emit(key, pressed)
        self.timed_events.append((self.clock.monotonic(), key, pressed))


class SequenceReader:
    def __init__(self, clock: FakeClock, states: list[tuple[str, float, bool]]) -> None:
        self.clock = clock
        self.states = iter(states)
        self.reads: list[tuple[str, float]] = []

    def __call__(self, base_url: str, timeout: float, monotonic):
        self.reads.append((base_url, timeout))
        self.clock.now += 0.001
        state, live_ms, paused = next(self.states)
        return self.clock.monotonic(), state, live_ms, paused


class TimeoutSequenceReader:
    def __init__(
        self,
        clock: FakeClock,
        outcomes: list[TimeoutError | tuple[str, float, bool]],
    ) -> None:
        self.clock = clock
        self.outcomes = iter(outcomes)
        self.reads = 0

    def __call__(self, base_url: str, timeout: float, monotonic):
        self.reads += 1
        self.clock.now += timeout
        outcome = next(self.outcomes)
        if isinstance(outcome, TimeoutError):
            raise outcome
        state, live_ms, paused = outcome
        return self.clock.monotonic(), state, live_ms, paused


class LiveAutoplayTests(unittest.TestCase):
    def run_live(self, replay: ManiaReplay, states: list[tuple[str, float, bool]]):
        clock = FakeClock()
        backend = RecordingBackend()
        reader = SequenceReader(clock, states)
        live_autoplay(
            replay,
            ("D",) * replay.key_count,
            backend,
            "http://vosu.test:24050",
            state_reader=reader,
            monotonic=clock.monotonic,
            sleep=clock.sleep,
        )
        return backend, reader

    def test_waits_through_stale_play_time_for_fresh_timeline_reset(self) -> None:
        replay = ManiaReplay(1, (ManiaEvent(0, 0, True), ManiaEvent(1, 0, False)))
        backend, reader = self.run_live(replay, [
            ("selectPlay", 83000, False),
            ("play", 83000, False),
            ("play", 0, False),
            ("play", 1, False),
        ])
        self.assertEqual(backend.events, [
            ("ENTER", True), ("ENTER", False), ("D", True), ("D", False),
        ])
        self.assertTrue(backend.closed)
        self.assertGreaterEqual(len(reader.reads), 4)

    def test_schedules_only_when_reported_live_time_reaches_event(self) -> None:
        replay = ManiaReplay(1, (ManiaEvent(100, 0, True), ManiaEvent(150, 0, False)))
        backend, reader = self.run_live(replay, [
            ("selectPlay", 3000, False),
            ("play", 0, False),
            ("play", 50, False),
            ("play", 99.9, False),
            ("play", 100, False),
            ("play", 120, True),
            ("play", 150, False),
        ])
        self.assertEqual(backend.events[-2:], [("D", True), ("D", False)])
        self.assertEqual(len(reader.reads), 7)

    def test_start_timeout_closes_backend(self) -> None:
        clock = FakeClock()
        backend = RecordingBackend()

        def reader(base_url: str, timeout: float, monotonic):
            clock.now += 0.6
            return clock.monotonic(), "selectPlay", 5000.0, False

        with self.assertRaisesRegex(ValidationError, "timed out"):
            live_autoplay(
                ManiaReplay(1, ()), ("D",), backend, "http://vosu.test",
                start_timeout=1.0, state_reader=reader,
                monotonic=clock.monotonic, sleep=clock.sleep,
            )
        self.assertEqual(backend.events, [("ENTER", True), ("ENTER", False)])
        self.assertTrue(backend.closed)

    def test_receive_timeouts_before_arming_are_bounded_by_start_timeout(self) -> None:
        clock = FakeClock()
        backend = RecordingBackend()
        reads = 0

        def reader(base_url: str, timeout: float, monotonic):
            nonlocal reads
            reads += 1
            clock.now += timeout
            raise TimeoutError

        with self.assertRaisesRegex(ValidationError, "timed out"):
            live_autoplay(
                ManiaReplay(1, ()), ("D",), backend, "http://vosu.test",
                start_timeout=1.0, request_timeout=0.25, state_reader=reader,
                monotonic=clock.monotonic, sleep=clock.sleep,
            )

        self.assertEqual(reads, 4)
        self.assertEqual(backend.events, [])
        self.assertTrue(backend.closed)

    def test_state_abort_releases_active_key(self) -> None:
        clock = FakeClock()
        backend = RecordingBackend()
        reader = SequenceReader(clock, [
            ("selectPlay", 1000, False),
            ("play", 0, False),
            ("status 7", 10, False),
        ])
        replay = ManiaReplay(1, (ManiaEvent(0, 0, True), ManiaEvent(100, 0, False)))
        with self.assertRaisesRegex(ValidationError, "left play state"):
            live_autoplay(
                replay, ("D",), backend, "http://vosu.test",
                state_reader=reader, monotonic=clock.monotonic, sleep=clock.sleep,
            )
        self.assertEqual(backend.events[-2:], [("D", True), ("D", False)])
        self.assertTrue(backend.closed)

    def test_retries_receive_timeouts_during_start_and_extrapolates_gameplay(self) -> None:
        clock = FakeClock()
        backend = RecordingBackend()
        reader = TimeoutSequenceReader(clock, [
            TimeoutError(),
            TimeoutError(),
            ("selectPlay", 5000, False),
            TimeoutError(),
            TimeoutError(),
            ("play", 0, False),
            TimeoutError(),
            TimeoutError(),
            ("play", 100, False),
        ])
        replay = ManiaReplay(1, (ManiaEvent(100, 0, True),))

        live_autoplay(
            replay, ("D",), backend, "http://vosu.test",
            start_timeout=2.0, request_timeout=0.1, state_reader=reader,
            monotonic=clock.monotonic, sleep=clock.sleep,
        )

        self.assertEqual(backend.events, [
            ("ENTER", True), ("ENTER", False), ("D", True), ("D", False),
        ])
        self.assertEqual(reader.reads, 9)
        self.assertTrue(backend.closed)

    def test_emits_at_extrapolated_deadline_without_a_fresh_frame(self) -> None:
        clock = FakeClock()
        backend = TimedRecordingBackend(clock)
        reader = TimeoutSequenceReader(clock, [
            ("selectPlay", 5000, False),
            ("play", 0, False),
            ("play", 40, False),
            TimeoutError(),
        ])

        live_autoplay(
            ManiaReplay(1, (ManiaEvent(100, 0, True),)),
            ("D",),
            backend,
            "http://vosu.test",
            request_timeout=0.25,
            state_reader=reader,
            monotonic=clock.monotonic,
            sleep=clock.sleep,
        )

        press_time = next(
            timestamp
            for timestamp, key, pressed in backend.timed_events
            if key == "D" and pressed
        )
        self.assertAlmostEqual(press_time, 0.70)
        self.assertEqual(reader.reads, 4)

    def test_duplicate_live_samples_do_not_reset_extrapolated_progress(self) -> None:
        clock = FakeClock()
        backend = TimedRecordingBackend(clock)
        call = 0

        def reader(base_url: str, timeout: float, monotonic):
            nonlocal call
            call += 1
            if call == 1:
                return clock.monotonic(), "selectPlay", 5000.0, False
            if call == 2:
                return clock.monotonic(), "play", 0.0, False
            if call == 3:
                clock.now += 0.05
                return clock.monotonic(), "play", 40.0, False
            if call == 4:
                clock.now += 0.02
                return clock.monotonic(), "play", 40.0, False
            clock.now += timeout
            raise TimeoutError

        live_autoplay(
            ManiaReplay(1, (ManiaEvent(100, 0, True),)),
            ("D",),
            backend,
            "http://vosu.test",
            request_timeout=0.25,
            state_reader=reader,
            monotonic=clock.monotonic,
            sleep=clock.sleep,
        )

        press_time = next(
            timestamp
            for timestamp, key, pressed in backend.timed_events
            if key == "D" and pressed
        )
        self.assertAlmostEqual(press_time, 0.15)

    def test_paused_sample_prevents_extrapolation_until_unpaused(self) -> None:
        clock = FakeClock()
        backend = TimedRecordingBackend(clock)
        call = 0
        unpaused_at = 0.0

        def reader(base_url: str, timeout: float, monotonic):
            nonlocal call, unpaused_at
            call += 1
            if call == 1:
                return clock.monotonic(), "selectPlay", 5000.0, False
            if call == 2:
                return clock.monotonic(), "play", 0.0, False
            if call == 3:
                clock.now += 0.05
                return clock.monotonic(), "play", 40.0, True
            if call == 4:
                clock.now += timeout
                raise TimeoutError
            if call == 5:
                clock.now += 0.01
                unpaused_at = clock.monotonic()
                return unpaused_at, "play", 40.0, False
            clock.now += timeout
            raise TimeoutError

        live_autoplay(
            ManiaReplay(1, (ManiaEvent(100, 0, True),)),
            ("D",),
            backend,
            "http://vosu.test",
            request_timeout=0.1,
            state_reader=reader,
            monotonic=clock.monotonic,
            sleep=clock.sleep,
        )

        press_time = next(
            timestamp
            for timestamp, key, pressed in backend.timed_events
            if key == "D" and pressed
        )
        self.assertAlmostEqual(press_time, unpaused_at + 0.06)
        self.assertGreater(press_time, 0.1)

    def test_gameplay_stall_timeout_bounds_paused_timeouts(self) -> None:
        clock = FakeClock()
        backend = RecordingBackend()
        call = 0

        def reader(base_url: str, timeout: float, monotonic):
            nonlocal call
            call += 1
            if call == 1:
                return clock.monotonic(), "selectPlay", 5000.0, False
            if call == 2:
                return clock.monotonic(), "play", 0.0, False
            if call == 3:
                return clock.monotonic(), "play", 0.0, True
            clock.now += timeout
            raise TimeoutError

        with self.assertRaisesRegex(
            ValidationError,
            r"stalled \(state='play', live_ms=0\.0, paused=True\)",
        ):
            live_autoplay(
                ManiaReplay(1, (ManiaEvent(100, 0, True),)),
                ("D",),
                backend,
                "http://vosu.test",
                request_timeout=0.1,
                stall_timeout=0.3,
                state_reader=reader,
                monotonic=clock.monotonic,
                sleep=clock.sleep,
            )

        self.assertAlmostEqual(clock.monotonic(), 0.34)
        self.assertTrue(backend.closed)

    def test_none_start_key_waits_without_emitting_start_input(self) -> None:
        clock = FakeClock()
        backend = RecordingBackend()
        reader = SequenceReader(clock, [
            ("selectPlay", 5000, False),
            ("play", 0, False),
        ])

        live_autoplay(
            ManiaReplay(1, ()), ("D",), backend, "http://vosu.test",
            start_key=None, state_reader=reader,
            monotonic=clock.monotonic, sleep=clock.sleep,
        )

        self.assertEqual(backend.events, [])
        self.assertTrue(backend.closed)

    def test_no_start_key_cli_option_maps_to_none(self) -> None:
        args = build_parser().parse_args([
            "live-autoplay",
            "map.osu",
            "--base-url",
            "http://vosu.test",
            "--no-start-key",
        ])
        self.assertIsNone(args.start_key)

    def test_seeded_jitter_is_paired_deterministic_and_ordered(self) -> None:
        events = (
            ManiaEvent(100, 0, True), ManiaEvent(101, 1, True),
            ManiaEvent(150, 0, False), ManiaEvent(151, 1, False),
        )
        first = jittered_events(events, 20.0, 7)
        second = jittered_events(events, 20.0, 7)
        other = jittered_events(events, 20.0, 8)
        self.assertEqual(first, second)
        self.assertNotEqual(first, other)
        timestamps = [timestamp for timestamp, _ in first]
        self.assertEqual(timestamps, sorted(timestamps))
        self.assertAlmostEqual(timestamps[2] - 150, timestamps[0] - 100)
        self.assertAlmostEqual(timestamps[3] - 151, timestamps[1] - 101)
        self.assertEqual(jittered_events(events, 0, 999), tuple((e.timestamp_ms, e) for e in events))

    def test_parses_numeric_v2_state(self) -> None:
        sample = parse_live_state({
            "state": {"number": 2, "name": "play"},
            "beatmap": {"time": {"live": 12.5}},
            "game": {"paused": False},
        }, 3.0)
        self.assertEqual(sample, (3.0, "play", 12.5, False))


if __name__ == "__main__":
    unittest.main()
