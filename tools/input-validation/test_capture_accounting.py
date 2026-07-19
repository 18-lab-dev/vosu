from __future__ import annotations

import unittest

from input_validation import CaptureAccounting


def payload(pressed: bool, count: int, pressed_at=None, released_at=None, mode=3):
    return {"keypresses": {"mode": {"number": mode, "name": "mania"}, "keys": [{
        "name": "K1", "isPressed": pressed, "count": count,
        "pressedAt": pressed_at, "releasedAt": released_at,
    }]}}


def empty_payload(mode=3):
    return {"keypresses": {"mode": {"number": mode, "name": "mania"}, "keys": []}}


class CaptureAccountingTests(unittest.TestCase):
    def test_edges_durations_and_between_poll_count_jump(self) -> None:
        accounting = CaptureAccounting()
        accounting.process(payload(False, 10), 1_000)
        accounting.process(payload(True, 11, pressed_at=100.0), 1_100)
        accounting.process(payload(True, 13, pressed_at=100.0), 1_300)
        accounting.process(payload(False, 13, pressed_at=100.0, released_at=145.5), 1_600)

        result = accounting.result()
        key = result["keys"]["K1"]
        self.assertEqual(key["countDelta"], 3)
        self.assertEqual(key["observedPresses"], 1)
        self.assertEqual(key["observedReleases"], 1)
        self.assertEqual(key["estimatedMissedPresses"], 2)
        self.assertEqual(key["pressDurationsMs"], [45.5])
        self.assertEqual([event["edge"] for event in accounting.events], ["press", "release"])
        self.assertEqual(result["receiveFrameGapsNs"], {"count": 3, "min": 100, "max": 300, "mean": 200.0})
        self.assertEqual(result["invariantFailures"], [])

    def test_timestamp_invariants_are_reported(self) -> None:
        accounting = CaptureAccounting()
        accounting.process(payload(False, 0, pressed_at=50, released_at=40), 10)
        accounting.process(payload(True, 1, pressed_at=None, released_at=40), 20)
        accounting.process(payload(True, 1, pressed_at=60, released_at=39), 30)

        failures = accounting.result()["invariantFailures"]
        self.assertIn("missing", [failure["type"] for failure in failures])
        self.assertIn("changedWithoutEdge", [failure["type"] for failure in failures])
        self.assertIn("regressing", [failure["type"] for failure in failures])


    def test_empty_key_list_starts_a_new_preserved_segment(self) -> None:
        accounting = CaptureAccounting()
        accounting.process(payload(False, 5), 10)
        accounting.process(payload(True, 6, pressed_at=100), 20)
        accounting.process(empty_payload(), 30)
        accounting.process(payload(False, 0), 40)
        accounting.process(payload(True, 1, pressed_at=10), 50)

        result = accounting.result()
        self.assertEqual(list(result["keys"]), ["K1", "K1@2"])
        self.assertEqual(result["keys"]["K1"]["countDelta"], 1)
        self.assertEqual(result["keys"]["K1@2"]["countDelta"], 1)
        self.assertEqual([event["key"] for event in accounting.events], ["K1", "K1@2"])
        self.assertFalse(any(failure["type"] == "regressing" for failure in result["invariantFailures"]))

    def test_mode_change_resets_identity_and_preserves_both_segments(self) -> None:
        accounting = CaptureAccounting()
        accounting.process(payload(False, 8, pressed_at=500, mode=3), 10)
        accounting.process(payload(False, 0, pressed_at=None, mode=0), 20)
        accounting.process(payload(True, 1, pressed_at=20, mode=0), 30)

        result = accounting.result()
        self.assertEqual(list(result["keys"]), ["K1", "K1@2"])
        self.assertEqual(result["keys"]["K1"]["firstCount"], 8)
        self.assertEqual(result["keys"]["K1@2"]["firstCount"], 0)
        self.assertEqual(accounting.events, [{
            "key": "K1@2", "edge": "press", "apiGameplayMs": 20, "receiveNs": 30, "count": 1,
        }])
        self.assertEqual(result["invariantFailures"], [])

    def test_count_regression_with_nonempty_keys_starts_new_segment(self) -> None:
        accounting = CaptureAccounting()
        accounting.process(payload(False, 9, pressed_at=500, released_at=450), 10)
        accounting.process(payload(False, 0, pressed_at=None, released_at=None), 20)

        result = accounting.result()
        self.assertEqual(list(result["keys"]), ["K1", "K1@2"])
        self.assertEqual(result["keys"]["K1"]["firstCount"], 9)
        self.assertEqual(result["keys"]["K1"]["lastCount"], 9)
        self.assertEqual(result["keys"]["K1@2"]["firstCount"], 0)
        self.assertEqual(result["keys"]["K1@2"]["lastCount"], 0)
        self.assertEqual(result["invariantFailures"], [])
        self.assertEqual(accounting.events, [])

if __name__ == "__main__":
    unittest.main()
