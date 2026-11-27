"""Preserve recorded chart authority for selected-chart calculator replay (§13)."""
from __future__ import annotations

from typing import Any
from urllib.parse import parse_qsl

from parity_golden_rules import GoldenError

RUN5_CAPTURE_SHA256 = "e3b52216667347fc9b0783e287cc8293e2a14c3293fe01e8f89ab8c04c1b39d5"
RUN5_DONOR = "state-http.json-sc.get.menu"


def _entry(envelope: dict, ns: str, pointer: str) -> dict | None:
    matches = [entry for entry in envelope["entries"]
               if entry["ns"] == ns and entry["pointer"] == pointer]
    return matches[0] if len(matches) == 1 and matches[0]["value_cbor_b64"] is not None \
        and matches[0]["error"] is None else None


def _selected_calculation(case: dict) -> bool:
    if case["family"] != "calculation" or not case["dumps"][0]["session"]:
        return False
    method, target, _ = case["source"][0]["request"].split("\r\n", 1)[0].split(" ")
    path, _, query = target.partition("?")
    return method == "GET" and path == "/api/calculate/pp" \
        and not any(key == "path" for key, _ in parse_qsl(query, keep_blank_values=True))


def recorded_selections(cases: list[dict], capture_sha256: str, receipt: str | None
                        ) -> tuple[dict[str, dict], dict[str, Any] | None]:
    migrated = {}
    donor = next((case for case in cases if case["id"] == RUN5_DONOR), None)
    for case in cases:
        if not _selected_calculation(case):
            continue
        if all(_entry(dump, "vosu.chart", "/resources/file") for dump in case["dumps"]):
            continue
        if capture_sha256 != RUN5_CAPTURE_SHA256 or receipt is None or donor is None:
            raise GoldenError(f"case {case['id']}: selected calculator chart was not recorded")
        if donor["barrier"] != case["barrier"] or donor["dump_schema"] != "sc":
            raise GoldenError(f"case {case['id']}: calculation migration donor has a different barrier or schema")
        for original, recorded in zip(case["dumps"], donor["dumps"], strict=True):
            keys = ("session", "focus_revision", "roster", "bus_revision")
            paths = _entry(original, "vosu.osu", "/internal/paths")
            if (any(original[key] != recorded[key] for key in keys) or paths is None
                    or paths != _entry(recorded, "vosu.osu", "/internal/paths")
                    or not _entry(recorded, "vosu.chart", "/resources/file")):
                raise GoldenError(f"case {case['id']}: calculation migration donor is not the same recorded selection")
        # Replay an intact recorded SC envelope, not a union of different selections.
        migrated[case["id"]] = donor["dumps"][0]
    if not migrated:
        return migrated, None
    return migrated, {"capture_sha256": capture_sha256, "review_receipt": receipt,
                      "barrier": donor["barrier"], "donor_case": RUN5_DONOR,
                      "donor_attempt": donor["attempts"], "cases": sorted(migrated)}
