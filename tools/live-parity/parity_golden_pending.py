"""Import a reviewed evidence-gap list into the run-five candidate inventory."""
import json
from pathlib import Path
import re

from parity_golden_rules import GoldenError
from parity_golden_selection import RUN5_CAPTURE_SHA256

FILE = "pending-recapture.json"
FORMAT = "vosu-golden-pending-recapture-v1"
DECISION = 'run5-replay-triage "Main decision"'
REASONS = {
    "s2-raw-clock-pair-absent": {"/game/paused"},
    "fft-pre-barrier-history-absent": {"/menu/mainMenu/bassDensity", "/settings/bassDensity"},
}
KEYS = {"platform", "case", "part", "frame", "pointer", "reason", "decision", "review_id"}


def load(path: str, files: dict[str, bytes]) -> dict:
    value = json.loads(Path(path).read_text(encoding="utf-8"))
    if (not isinstance(value, dict) or set(value) != {"format", "entries"}
            or value["format"] != FORMAT or type(value["entries"]) is not list
            or not value["entries"]):
        raise GoldenError(f"{FILE}: expected nonempty reviewed evidence gaps")
    seen = set()
    reviewed_platforms = {rel.removesuffix("/manifest.json") for rel, data in files.items()
                          if rel.endswith("/manifest.json")
                          and json.loads(data).get("calculation_chart_migration", {})
                          .get("capture_sha256") == RUN5_CAPTURE_SHA256}
    for entry in value["entries"]:
        if not isinstance(entry, dict) or set(entry) != KEYS or any(
                type(entry[k]) is not str for k in KEYS - {"frame"}):
            raise GoldenError(f"{FILE}: malformed entry")
        platform, case = entry["platform"], entry["case"]
        part, frame, pointer = entry["part"], entry["frame"], entry["pointer"]
        if (platform not in reviewed_platforms
                or f"{platform}/cases/{case}.json" not in files
                or not ((part == "body" and frame is None)
                        or (part == "ws.frame" and type(frame) is int and frame >= 0))
                or pointer not in REASONS.get(entry["reason"], ())
                or entry["decision"] != DECISION
                or not re.fullmatch(r"[A-Za-z0-9._:#-]{1,128}", entry["review_id"])):
            raise GoldenError(f"{FILE}: invalid entry {entry!r}")
        key = (platform, case, part, -1 if frame is None else frame, pointer)
        if key in seen:
            raise GoldenError(f"{FILE}: duplicate entry {entry!r}")
        seen.add(key)
    value["entries"].sort(key=lambda row: (
        row["platform"], row["case"], row["part"], row["frame"], row["pointer"]))
    return value
