from __future__ import annotations

import json
from pathlib import Path
import tempfile
import unittest

from live_parity import Sample, dump_capture
from parity_diff import export_diffs


class DiffArtifactTests(unittest.TestCase):
    def test_exports_canonical_payloads_and_unified_diff(self) -> None:
        source = {"state": {"name": "menu", "number": 0}, "value": 1}
        vosu = {"state": {"name": "menu", "number": 0}, "value": 2}
        samples = [
            Sample("source", "v2", 100_000_000, source, transport="http"),
            Sample("vosu", "v2", 110_000_000, vosu, transport="http"),
        ]
        with tempfile.TemporaryDirectory() as tmp:
            capture = Path(tmp) / "capture.json"
            out = Path(tmp) / "diffs"
            dump_capture(str(capture), samples, {
                "poll_rate_ms": 150,
                "barrier_times": {
                    "source": {"menu": 100_000_000},
                    "vosu": {"menu": 110_000_000},
                },
            })

            self.assertEqual(0, export_diffs(str(capture), str(out)))

            source_text = (out / "menu.http.v2.source.json").read_text()
            vosu_text = (out / "menu.http.v2.vosu.json").read_text()
            diff_text = (out / "menu.http.v2.diff").read_text()
            manifest = json.loads((out / "manifest.json").read_text())
            self.assertEqual(source, json.loads(source_text)["payload"])
            self.assertEqual(vosu, json.loads(vosu_text)["payload"])
            self.assertIn('-    "value": 1', diff_text)
            self.assertIn('+    "value": 2', diff_text)
            self.assertEqual(diff_text, (out / "all.diff").read_text())
            self.assertEqual(1, len(manifest["pairs"]))
            self.assertFalse(manifest["pairs"][0]["identical"])


if __name__ == "__main__":
    unittest.main()
