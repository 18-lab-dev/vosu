import json
from pathlib import Path
import tempfile
import unittest

import parity_golden_pending as pending
from parity_golden_rules import GoldenError
from parity_golden_selection import RUN5_CAPTURE_SHA256


class PendingRecaptureTests(unittest.TestCase):
    def test_accepts_only_reviewed_run_five_case_pointers(self) -> None:
        platform, case_id = "windows-lazer", "state-http.json-v2.get.menu"
        manifest = f"{platform}/manifest.json"
        files = {
            manifest: json.dumps({"calculation_chart_migration": {
                "capture_sha256": RUN5_CAPTURE_SHA256}}).encode(),
            f"{platform}/cases/{case_id}.json": b"{}",
        }
        entry = {"platform": platform, "case": case_id, "part": "body", "frame": None,
                 "pointer": "/game/paused", "reason": "s2-raw-clock-pair-absent",
                 "decision": pending.DECISION, "review_id": "reviewed"}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / pending.FILE

            def write_entries(entries):
                path.write_text(json.dumps({"format": pending.FORMAT, "entries": entries}), "utf-8")

            write_entries([entry])
            self.assertEqual(pending.load(str(path), files)["entries"], [entry])
            with self.assertRaisesRegex(GoldenError, "invalid entry"):
                pending.load(str(path), {**files, manifest: b"{}"})
            write_entries([{**entry, "pointer": "/game/focused"}])
            with self.assertRaisesRegex(GoldenError, "invalid entry"):
                pending.load(str(path), files)
            write_entries([entry, entry])
            with self.assertRaisesRegex(GoldenError, "duplicate entry"):
                pending.load(str(path), files)
