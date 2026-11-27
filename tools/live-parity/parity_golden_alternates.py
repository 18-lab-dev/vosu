"""Reviewed run-five replay alternates; original upstream expectations remain intact.

See GOLDEN_CONTRACT §§6, 10, 13 and DIVERGENCES. This inventory applies only
to the exact attested capture, not to later captures or a family wildcard.
"""
from __future__ import annotations

import json
from typing import Any

from parity_golden_rules import GoldenError
from parity_volatile import without_charset
from route_matrix import COMPARED_HEADERS

NOT_FOUND = b"Not Found"
NEAR = "legacy-adapter-near-route-exactness"
BUDGET = "adapter-surface-request-budget"
WS_VALIDATION = "legacy-ws-strict-upgrade"
S3 = "legacy-lazer-song-select-settings"
SETTINGS = ("sort", "group")
STATE = ("json", "json-sc", "json-v2", "json-v2-precise")
FEEDS = ("ws", "tokens", "v2", "v2-precise")
LIVE = ("menu", "paused", "result")
METHODS = ("post", "put", "delete", "options")
NEAR_SUFFIXES = ("wrong-case", "suffix", "double-slash", "trailing-slash")
PATH_PROBES = ("ads", "backslash-encoded", "backslash-raw", "dot-segment", "double-encoded-parent",
               "encoded-parent", "trailing-dot", "trailing-space")


def _body(record: dict) -> str | dict:
    return record["body"].get("text", record["body"])


def _header(record: dict, name: str) -> str | None:
    values = [value for key, value in record["headers"] if key.lower() == name]
    if len(values) > 1:
        raise GoldenError(f"multiple {name} values on a reviewed alternate")
    return values[0] if values else None


def _status(record: dict, wanted: int, case: str) -> None:
    if record["status"] != wanted:
        raise GoldenError(f"case {case}: alternate's captured vosu status is not {wanted}")


def _settings(record: dict) -> dict:
    data = json.loads(record["dumps"][0]["http"]["body_utf8"])
    return {key: data["settings"][key] for key in SETTINGS}


def _with_settings(text: str, case: dict) -> str:
    data = json.loads(text)
    for key, value in _settings(case).items():
        if key not in data.get("settings", {}):
            raise GoldenError(f"case {case['id']}: sibling missing recorded settings member {key}")
        data["settings"][key] = value
    return json.dumps(data, ensure_ascii=False, separators=(",", ":"))


def rules(cases: list[dict], files: dict[str, bytes], render_text: Any) -> list[dict]:
    capture = {case["id"]: case for case in cases}
    expected = {case: json.loads(files[f"cases/{case}.json"])["expected"] for case in capture}
    out: list[dict] = []

    def add(case, ledger_id, part, value, *, replacements=False, indices=None):
        rule = {"ledger_id": ledger_id, "platforms": ["windows-lazer"], "cases": [case], "part": part,
                "vosu_replacements" if replacements else "vosu_value": value}
        if indices is not None:
            rule["frame_indices"] = indices
        out.append(rule)

    def header(case, ledger_id, name, value):
        old = _header(expected[case], name)
        def normalize(item):
            return without_charset(item) if item is not None and name == "content-type" else item
        if normalize(old) != normalize(value):
            add(case, ledger_id, f"headers.{name}", value)

    def parts(case, ledger_id, status, body, headers):
        original = expected[case]
        if original["status"] != status:
            add(case, ledger_id, "status", status)
        if _body(original) != body:
            add(case, ledger_id, "body", body)
        for name in sorted(COMPARED_HEADERS):
            header(case, ledger_id, name, headers.get(name))

    def observed(case, ledger_id, status, exact_body=None):
        recorded = capture[case]["route"]
        _status(recorded, status, case)
        if exact_body is not None and recorded["body"] != exact_body:
            raise GoldenError(f"case {case}: unexpected body in alternate witness")
        body = render_text(recorded["body"], capture[case]["family"], "body")
        parts(case, ledger_id, status, body,
              {name: _header(recorded, name) for name in COMPARED_HEADERS})

    for schema in STATE:
        prefix = f"state-http.{schema}"
        near = NEAR_SUFFIXES + (() if schema == "json" else ("inner-double-slash",))
        for suffix in near:
            observed(f"{prefix}.near.{suffix}", NEAR, 404, NOT_FOUND)
        for barrier in (*LIVE, "no-client"):
            observed(f"{prefix}.head.{barrier}", NEAR, 404, b"")
        for method in METHODS:
            observed(f"{prefix}.{method}", NEAR, 404, NOT_FOUND)
    for case in ("calculation.post", "files.beatmap.file.post"):
        observed(case, NEAR, 404, NOT_FOUND)
    observed("ws.near.v2-subscribe-plain", "adr0021-removed-selected-v2-feed", 404, NOT_FOUND)
    for route in ("songs", "beatmap"):
        for probe in PATH_PROBES:
            observed(f"path-security.{route}.{probe}", "legacy-confined-path-probes", 404, NOT_FOUND)
    for case in ("state-http.json-v2.budget.headers-33", "state-http.json-v2.budget.query-pairs-129",
                 "state-http.json-v2.budget.query-bytes-16385"):
        observed(case, BUDGET, 404, NOT_FOUND)
    observed("files.budget.headers-33", "legacy-adapter-wire-header-budget", 404, NOT_FOUND)

    for case, ledger in {
        "state-http.json-v2.budget.query-bytes-16384": "legacy-adapter-wire-header-budget",
        "origin-peer.http.null": "legacy-opaque-origin-read",
        "origin-peer.host.empty": "legacy-overlay-host-field",
        "origin-peer.host.missing": "legacy-overlay-host-field",
    }.items():
        donor = "state-http.json-v2.get.menu"
        if capture[case]["barrier"] != capture[donor]["barrier"]:
            raise GoldenError(f"case {case}: admitted-read donor must share the capture barrier")
        _status(capture[case]["route"], 200, case)
        _status(expected[donor], 200, donor)
        parts(case, ledger, 200, _with_settings(_body(expected[donor]), capture[case]),
              {name: _header(expected[donor], name) for name in COMPARED_HEADERS})
    observed("origin-peer.ws.foreign", "legacy-opaque-origin-read", 403, NOT_FOUND)
    case, donor = "origin-peer.ws.null", "ws.v2.upgrade.menu"
    _status(capture[case]["route"], 101, case)
    if capture[case]["barrier"] != capture[donor]["barrier"]:
        raise GoldenError(f"case {case}: admitted-feed donor must share the capture barrier")
    frame = dict(expected[donor]["frames"][0])
    text = _with_settings(frame["payload"]["text"], capture[case])
    frame.update(payload={"text": text}, length=len(text.encode("utf-8")))
    parts(case, "legacy-opaque-origin-read", 101, "", {})
    add(case, "legacy-opaque-origin-read", "ws.sequence", [frame])

    for feed in FEEDS:
        for suffix in ("wrong-version", "get-plain", "missing-connection", "missing-upgrade", "missing-key"):
            case = f"ws.{feed}.invalid.{suffix}"
            observed(case, WS_VALIDATION, 400)
            if suffix == "wrong-version":
                add(case, WS_VALIDATION, "ws.sequence", [])
        observed(f"ws.{feed}.invalid.post-upgrade", WS_VALIDATION, 404, NOT_FOUND)

    for case in ("files.beatmap.relative.get", "files.beatmap.relative.head"):
        observed(case, "legacy-beatmap-shortcut-exact", 404,
                 b"" if case.endswith("head") else NOT_FOUND)
    for suffix in ("get", "head"):
        body = b"" if suffix == "head" else NOT_FOUND
        observed(f"files.skin.index.{suffix}", "adr0021-removed-legacy-skin-index", 404, body)
        observed(f"files.skin.lazer-file.{suffix}", "lazer-legacy-skin-404", 404, body)
    for case in ("files.background-image.head", "files.songs.file.head"):
        observed(case, NEAR, 404, b"")
    observed("files.songs.dir.get", "legacy-directory-safe-index", 200)
    observed("files.range.multiple", "legacy-single-range", 416, b"")
    for suffix in ("if-modified-since-future", "if-none-match-star"):
        observed(f"files.conditional.{suffix}", "upstream-no-file-validators", 304, b"")

    # Exact representations of the approved generated chart, PNG and WAV;
    # the capture mislabeled or omitted labels on these case IDs only.
    for case, media in {
        "files.background-image.get": "image/png", "files.beatmap.background.get": "image/png",
        "files.beatmap.audio.get": "audio/wav", "files.range.single": "audio/wav",
        "files.conditional.if-range-other": "audio/wav", "files.conditional.if-range-past": "audio/wav",
        "files.songs.file.get": "application/octet-stream",
    }.items():
        header(case, "legacy-file-media-type", "content-type", media)
    for case, status, media, donor in (
        ("files.beatmap.background.head", 200, "image/png", "files.beatmap.background.get"),
        ("files.beatmap.audio.head", 200, "audio/wav", "files.beatmap.audio.get"),
        ("files.beatmap.file.head", 200, "text/plain", "files.beatmap.file.get"),
        ("files.range.single-head", 206, "audio/wav", "files.range.single"),
    ):
        reply = expected[donor]
        _status(reply, status, donor)
        parts(case, "legacy-file-head", status, "", {"accept-ranges": _header(reply, "accept-ranges"),
              "content-range": _header(reply, "content-range"), "content-type": media})

    for suffix in ("mods-acronyms", "mods-json"):
        observed(f"calculation.{suffix}", "legacy-calculate-mod-inputs", 200)
    for suffix, message in {
        "invalid-accuracy": "invalid acc", "invalid-clock-rate": "clockRate must be between 0.01 and 100",
        "mods-invalid": "invalid mods acronyms", "invalid-count": "invalid combo",
        "invalid-mode": "mode must be 0, 1, 2, or 3",
    }.items():
        observed(f"calculation.{suffix}", "legacy-calculate-client-errors", 400,
                 json.dumps({"error": message}, separators=(",", ":")).encode("utf-8"))

    s3_http = [*(f"state-http.json-v2.get.{s}" for s in LIVE), "state-http.json-v2.near.query",
               "state-http.json-v2.budget.headers-32", "state-http.json-v2.budget.query-pairs-128",
               "origin-peer.host.duplicate", "origin-peer.host.foreign", "origin-peer.http.localhost",
               "origin-peer.http.loopback-ip", "origin-peer.http.none"]
    s3_ws = [*(f"ws.v2.upgrade.{s}" for s in LIVE),
             *(f"ws.v2.filter.{s}" for s in ("nested", "repeated-message", "duplicate-keys", "invalid-json", "oversized"))]
    for case in [*s3_http, *s3_ws]:
        replacements = [{"pointer": f"/settings/{key}", "value": value}
                        for key, value in _settings(capture[case]).items()]
        is_ws = case in s3_ws
        indices = ([0, 2] if case == "ws.v2.filter.invalid-json" else [0]) if is_ws else None
        add(case, S3, "ws.frame" if is_ws else "body", replacements=True,
            value=replacements, indices=indices)
    return sorted(out, key=lambda rule: (rule["cases"][0], rule["part"]))
