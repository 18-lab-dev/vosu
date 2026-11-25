"""`golden`: validate a private golden capture and write a reviewable candidate fixture.

`tests/fixtures/upstream/GOLDEN_CONTRACT.md` is normative. Upstream records are
the only expected values; vosu dumps are replay input only. The command reads
complete, attested `vosu-golden-capture-v1` files (one per platform), applies
only the rules in `parity_golden_rules`, and publishes a new candidate directory
with the `tests/fixtures/upstream/<pin>/` layout. It never writes committed
fixtures, and its errors never echo private content.

Capture shape (written by `golden-capture`; every object is closed):

    {"format": "vosu-golden-capture-v1", "source_revision": <pin>,
     "platform": "windows-lazer", "scenario_set": str,
     "authorization": {"reference", "platform", "source_revision", "vosu_commit",
                       "client_version", "scenario_set"},
     "attestation": {"source": {"revision", "kind": "source"|"native", "sha256"},
                     "vosu": {"commit", "version", "profile": "debug", "features",
                              "target", "sha256"},
                     "client": {"variant", "version", "sha256"}},
     "roots": [{"slot", "private"}],
     "assets": [{"id", "slot", "relative", "media_type", "generator", "license",
                 "sha256", "b64"}],
     "expected_cases": [case id, ...],
     "cases": [{"id", "family", "barrier", "dump_schema",
                "conditional": null|"etag"|"last-modified",
                "attempts": [{"attempt": 1, "outcome": "failed", "reason": str},
                             {"attempt": 2, "outcome": "complete",
                              "source": [record, record],
                              "vosu": {"dump_before": dump, "route": record,
                                       "dump_after": dump}}]}]}

    record = {"attempt", "request": bytes, "status_line", "headers": [[name, value]],
              "body": bytes (after transfer-coding removal), "frames": [frame] (ws only)}
           | {"attempt", "request": bytes, "outcome": "closed", "frames": [] (ws only)}
             (the connection closed without a response byte; expected {"outcome": "closed"})
    A final attempt may be `upstream_crashed` instead (parity_golden_crash): no case file.
    frame  = {"from": "client"|"server", "fin", "opcode", "masked", "mask_key": hex|null,
              "length", "payload": bytes}
    dump   = {"attempt", "envelope": vosu-adapter-capture-v1}
    bytes  = {"text": str} | {"b64": str}
"""
from __future__ import annotations

import base64
import binascii
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import parity_golden_crash as crash
import parity_golden_ledger as ledger
import parity_volatile as volatile
import shutil
import subprocess
import tempfile
from typing import Any, Literal
from urllib.parse import unquote

from live_parity import PINNED_SOURCE_REVISION, PINNED_VOSU_VERSION, SUPPORTED_MATRIX
from parity_golden_rules import (
    CAPABILITY_HEADER, CREDENTIAL_HEADERS, ENVELOPE_KEYS, NON_GOLDEN_SEGMENTS,
    NORMALIZATION_VERSION, OLD_BRAND, TRANSPORT_OUTCOMES, VALIDATOR_REQUEST_HEADERS,
    GoldenError, Root, RootRewriter, Validators, apply_timestamp_pointers,
    check_approved_rules, dump_sandwich_difference, entity_tag, normalize_response_headers,
    privacy_findings, rewrite_cbor_text, synthetic_root, validate_private_root,
)

REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_APPROVED_RULES = REPO_ROOT / "tests/fixtures/upstream/GOLDEN_CONTRACT.md"

Provenance = Literal["captured", "synthetic"]

CAPTURE_FORMAT = "vosu-golden-capture-v1"
ENVELOPE_FORMAT = "vosu-adapter-capture-v1"
FIXTURE_FORMAT = "vosu-golden-fixture-v1"
PLATFORM_FORMAT = "vosu-golden-platform-v1"
CASE_FORMAT = "vosu-golden-case-v1"
INVENTORY_FORMAT = "vosu-golden-inventory-v1"
ASSETS_FORMAT = "vosu-golden-assets-v1"

FAMILIES = ("state-http", "ws", "files", "path-security", "calculation", "origin-peer")
BARRIERS = ("no-client", "menu", "paused", "result")
ENVELOPE_SCHEMAS = ("v1", "sc", "v2", "v2-precise")
STATE_ROUTES = {"/json": "v1", "/json/sc": "sc", "/json/v2": "v2", "/json/v2/precise": "v2-precise"}
# GOLDEN_CONTRACT.md §6 header comparison set, plus `date` whose presence is compared.
COMPARED_HEADERS = frozenset({
    "content-type", "content-length", "cache-control", "etag", "last-modified",
    "accept-ranges", "content-range", "location", "content-security-policy",
    "x-content-type-options", "upgrade", "connection", "sec-websocket-accept",
    "sec-websocket-protocol", "sec-websocket-extensions", "sec-websocket-version", "date",
})
MAX_ATTEMPTS = 3
MAX_CAPTURE_BYTES = 512 << 20
JSON_BUDGET = 8 << 20
ASSET_BUDGET = 4 << 20

_SLUG = r"[a-z0-9]+(?:-[a-z0-9]+)*(?:\.[a-z0-9]+(?:-[a-z0-9]+)*)*"
_CASE_ID = re.compile(rf"(?:{'|'.join(map(re.escape, FAMILIES))})\.{_SLUG}")
_ASSET_ID = re.compile(_SLUG)
_WINDOWS_RESERVED = re.compile(r"(?i)(?:con|prn|aux|nul|com\d|lpt\d)(?:\..*)?")
_STATUS_LINE = re.compile(r"HTTP/1\.[01] ([1-5]\d\d)(?: [^\r\n]*)?")
_HEADER_NAME = re.compile(r"[!#$%&'*+.^_`|~0-9A-Za-z-]+")
_SESSION_KEYS = ("ordinal", "generation", "owner", "owner_instance", "game", "variant",
                 "foreground")
_DIGEST_KEYS = ("session", "focus_revision", "roster", "bus_revision", "namespace_revisions",
                "entries")


# ---------------------------------------------------------------- strict input


def _load_capture(path: str) -> Any:
    try:
        with open(path, "rb") as handle:
            raw = handle.read(MAX_CAPTURE_BYTES + 1)
    except OSError as exc:
        raise GoldenError(f"cannot read capture: {exc.strerror}") from exc
    if len(raw) > MAX_CAPTURE_BYTES:
        raise GoldenError(f"capture exceeds {MAX_CAPTURE_BYTES} bytes")

    def pairs(items: list[tuple[str, Any]]) -> dict[str, Any]:
        out: dict[str, Any] = {}
        for key, value in items:
            if key in out:
                raise GoldenError(f"capture has a duplicate JSON key {key!r}")
            out[key] = value
        return out

    def constant(name: str) -> Any:
        raise GoldenError(f"capture contains non-finite number {name}")

    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=pairs, parse_constant=constant)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GoldenError(f"capture is not UTF-8 JSON: {exc}") from exc


def _obj(value: Any, where: str, keys: tuple[str, ...], ordered: bool = False) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise GoldenError(f"{where}: expected an object")
    if ordered and tuple(value) != keys or set(value) != set(keys):
        raise GoldenError(
            f"{where}: expected keys {list(keys)}"
            + (" in order" if ordered else "")
            + f", missing {sorted(set(keys) - set(value))}, unknown {sorted(set(value) - set(keys))}")
    return value


def _list(value: Any, where: str) -> list[Any]:
    if not isinstance(value, list):
        raise GoldenError(f"{where}: expected an array")
    return value


def _str(value: Any, where: str, pattern: re.Pattern[str] | None = None,
         choices: tuple[str, ...] | None = None) -> str:
    if not isinstance(value, str) or not value:
        raise GoldenError(f"{where}: expected a non-empty string")
    if pattern is not None and pattern.fullmatch(value) is None:
        raise GoldenError(f"{where}: invalid value")
    if choices is not None and value not in choices:
        raise GoldenError(f"{where}: expected one of {list(choices)}")
    return value


def _int(value: Any, where: str, low: int = 0, high: int = (1 << 64) - 1) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
        raise GoldenError(f"{where}: expected an integer in [{low}, {high}]")
    return value


def _hex(value: Any, where: str, length: int) -> str:
    return _str(value, where, re.compile(f"[0-9a-f]{{{length}}}"))


def _bytes(value: Any, where: str) -> bytes:
    if not isinstance(value, dict) or len(value) != 1 or next(iter(value)) not in ("text", "b64"):
        raise GoldenError(f"{where}: expected exactly one of 'text' or 'b64'")
    (kind, data), = value.items()
    if not isinstance(data, str):
        raise GoldenError(f"{where}.{kind}: expected a string")
    if kind == "text":
        return data.encode("utf-8")
    try:
        return base64.b64decode(data, validate=True)
    except binascii.Error as exc:
        raise GoldenError(f"{where}.b64: invalid base64") from exc


def _utf8(data: bytes) -> str | None:
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError:
        return None


# ---------------------------------------------------------------- records


def _record(value: Any, where: str, attempt: int, ws: bool) -> dict[str, Any]:
    transport = isinstance(value, dict) and "outcome" in value
    keys = (("attempt", "request", "outcome") if transport
            else ("attempt", "request", "status_line", "headers", "body")) + (("frames",) if ws else ())
    _obj(value, where, keys)
    if _int(value["attempt"], f"{where}.attempt") != attempt:
        raise GoldenError(f"{where}: record belongs to attempt {value['attempt']}, not {attempt} "
                          "(attempts are never mixed)")
    request = _utf8(_bytes(value["request"], f"{where}.request"))
    if request is None:
        raise GoldenError(f"{where}.request: binary request bytes")
    if transport:
        # GOLDEN_CONTRACT §6: the peer closed the connection without a response byte.
        _str(value["outcome"], f"{where}.outcome", choices=TRANSPORT_OUTCOMES)
        if ws and value["frames"] != []:
            raise GoldenError(f"{where}.frames: a connection closed without a response has no frames")
        return {"request": request, "outcome": value["outcome"], "status": None, "headers": [],
                "body": b"", "frames": [] if ws else None}
    status = _STATUS_LINE.fullmatch(_str(value["status_line"], f"{where}.status_line"))
    if status is None:
        raise GoldenError(f"{where}.status_line: not an HTTP/1.x status line")
    headers: list[list[str]] = []
    for index, header in enumerate(_list(value["headers"], f"{where}.headers")):
        if (not isinstance(header, list) or len(header) != 2
                or not all(isinstance(part, str) for part in header)
                or _HEADER_NAME.fullmatch(header[0]) is None or re.search(r"[\r\n]", header[1])):
            raise GoldenError(f"{where}.headers[{index}]: expected [token name, single-line value]")
        headers.append([header[0], header[1]])
    frames = [_frame(frame, f"{where}.frames[{index}]")
              for index, frame in enumerate(_list(value["frames"], f"{where}.frames"))] if ws else None
    return {"request": request, "outcome": None, "status": int(status.group(1)),
            "headers": headers, "body": _bytes(value["body"], f"{where}.body"), "frames": frames}


def _frame(value: Any, where: str) -> dict[str, Any]:
    _obj(value, where, ("from", "fin", "opcode", "masked", "mask_key", "length", "payload"))
    _str(value["from"], f"{where}.from", choices=("client", "server"))
    if not isinstance(value["fin"], bool) or not isinstance(value["masked"], bool):
        raise GoldenError(f"{where}: fin and masked must be booleans")
    opcode = _int(value["opcode"], f"{where}.opcode", 0, 15)
    if opcode not in (0, 1, 2, 8, 9, 10):
        raise GoldenError(f"{where}.opcode: reserved opcode")
    if value["masked"] != (value["mask_key"] is not None):
        raise GoldenError(f"{where}: mask_key must be present exactly when masked")
    if value["mask_key"] is not None:
        _hex(value["mask_key"], f"{where}.mask_key", 8)
    payload = _bytes(value["payload"], f"{where}.payload")
    if _int(value["length"], f"{where}.length") != len(payload):
        raise GoldenError(f"{where}: length differs from the payload")
    return {"from": value["from"], "fin": value["fin"], "opcode": opcode,
            "masked": value["masked"], "payload": payload}


def _session(value: Any, where: str) -> dict[str, Any]:
    _obj(value, where, _SESSION_KEYS, ordered=True)
    for key in ("ordinal", "generation", "owner_instance"):
        _int(value[key], f"{where}.{key}")
    _str(value["owner"], f"{where}.owner")
    _str(value["game"], f"{where}.game")
    if not isinstance(value["variant"], str):
        raise GoldenError(f"{where}.variant: expected a string")
    if value["foreground"] is not None and not isinstance(value["foreground"], bool):
        raise GoldenError(f"{where}.foreground: expected a boolean or null")
    return value


def envelope_digest(envelope: dict[str, Any]) -> str:
    selection = {key: envelope[key] for key in _DIGEST_KEYS}
    encoded = json.dumps(selection, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return "sha256:" + hashlib.sha256(encoded).hexdigest()


def _envelope(value: Any, where: str) -> dict[str, Any]:
    _obj(value, where, ENVELOPE_KEYS, ordered=True)
    if value["format"] != ENVELOPE_FORMAT:
        raise GoldenError(f"{where}.format: expected {ENVELOPE_FORMAT}")
    _str(value["schema"], f"{where}.schema", choices=ENVELOPE_SCHEMAS)
    for key in ("provider_generation", "focus_revision", "bus_revision"):
        _int(value[key], f"{where}.{key}")
    selected = None if value["session"] is None else _session(value["session"], f"{where}.session")
    roster = _obj(value["roster"], f"{where}.roster", ("revision", "sessions"), ordered=True)
    _int(roster["revision"], f"{where}.roster.revision")
    sessions = [_session(item, f"{where}.roster.sessions[{index}]")
                for index, item in enumerate(_list(roster["sessions"], f"{where}.roster.sessions"))]
    for index, pair in enumerate(_list(value["namespace_revisions"], f"{where}.namespace_revisions")):
        if not isinstance(pair, list) or len(pair) != 2:
            raise GoldenError(f"{where}.namespace_revisions[{index}]: expected [namespace, revision]")
        _str(pair[0], f"{where}.namespace_revisions[{index}][0]")
        _int(pair[1], f"{where}.namespace_revisions[{index}][1]")
    for index, entry in enumerate(_list(value["entries"], f"{where}.entries")):
        at = f"{where}.entries[{index}]"
        _obj(entry, at, ("ns", "pointer", "value_cbor_b64", "error"), ordered=True)
        _str(entry["ns"], f"{at}.ns")
        if not isinstance(entry["pointer"], str):
            raise GoldenError(f"{at}.pointer: expected a string")
        if entry["value_cbor_b64"] is not None:
            _bytes({"b64": entry["value_cbor_b64"]}, f"{at}.value_cbor")
        if entry["error"] is not None:
            error = _obj(entry["error"], f"{at}.error", ("code", "message"))
            if not isinstance(error["code"], str) or not isinstance(error["message"], str):
                raise GoldenError(f"{at}.error: expected string code and message")
    derived = _obj(value["derived"], f"{where}.derived", ("revision", "status"), ordered=True)
    _int(derived["revision"], f"{where}.derived.revision")
    _str(derived["status"], f"{where}.derived.status", choices=("ready", "pending", "none"))
    http = _obj(value["http"], f"{where}.http", ("kind", "status", "body_utf8"), ordered=True)
    _str(http["kind"], f"{where}.http.kind", choices=("ready", "not_ready", "failed"))
    _int(http["status"], f"{where}.http.status", 100, 599)
    if not isinstance(http["body_utf8"], str):
        raise GoldenError(f"{where}.http.body_utf8: expected a string")
    _str(value["digest"], f"{where}.digest", re.compile(r"sha256:[0-9a-f]{64}"))
    if envelope_digest(value) != value["digest"]:
        raise GoldenError(f"{where}.digest: does not match the selected entries")
    _check_identity_ordinals(selected, sessions, where)
    return value


def _check_identity_ordinals(selected: dict[str, Any] | None, sessions: list[dict[str, Any]],
                             where: str) -> None:
    """`envelope-identity-ordinal`: first-appearance ordinals, roster order then selection."""
    order = list(sessions)
    for index, session in enumerate(sessions):
        if session["ordinal"] != index:
            raise GoldenError(f"{where}.roster: session ordinals are not first-appearance ordinals")
    if selected is not None:
        if selected["ordinal"] < len(sessions):
            if selected != sessions[selected["ordinal"]]:
                raise GoldenError(f"{where}.session: differs from its roster entry")
        elif selected["ordinal"] == len(sessions):
            order.append(selected)
        else:
            raise GoldenError(f"{where}.session: ordinal is not a first-appearance ordinal")
    seen = 0
    for session in order:
        if session["owner_instance"] > seen:
            raise GoldenError(f"{where}: owner_instance is not a first-appearance ordinal")
        seen += session["owner_instance"] == seen


# ---------------------------------------------------------------- capture header


def _check_identity(capture: dict[str, Any]) -> dict[str, Any]:
    if capture["format"] != CAPTURE_FORMAT:
        raise GoldenError(f"capture.format: expected {CAPTURE_FORMAT}")
    if capture["source_revision"] != PINNED_SOURCE_REVISION:
        raise GoldenError("capture.source_revision: wrong pin (differs from PINNED_SOURCE_REVISION)")
    platform = _str(capture["platform"], "capture.platform")
    os_name, _, variant = platform.partition("-")
    if (os_name, variant) not in SUPPORTED_MATRIX:
        raise GoldenError(f"capture.platform: {platform!r} is not a supported platform")
    scenario = _str(capture["scenario_set"], "capture.scenario_set")
    attestation = _obj(capture["attestation"], "capture.attestation", ("source", "vosu", "client"))
    source = _obj(attestation["source"], "attestation.source", ("revision", "kind", "sha256"))
    if source["revision"] != PINNED_SOURCE_REVISION:
        raise GoldenError("attestation.source.revision: wrong pin")
    _str(source["kind"], "attestation.source.kind", choices=("source", "native"))
    _hex(source["sha256"], "attestation.source.sha256", 64)
    vosu = _obj(attestation["vosu"], "attestation.vosu",
                ("commit", "version", "profile", "features", "target", "sha256"))
    _hex(vosu["commit"], "attestation.vosu.commit", 40)
    if vosu["version"] != PINNED_VOSU_VERSION:
        raise GoldenError("attestation.vosu.version: not the pinned vosu version")
    features = _list(vosu["features"], "attestation.vosu.features")
    if vosu["profile"] != "debug" or "test-harness" not in features:
        raise GoldenError("attestation.vosu: dumps require the attested debug test-harness build")
    for index, feature in enumerate(features):
        _str(feature, f"attestation.vosu.features[{index}]", re.compile(r"[a-z0-9-]+"))
    _str(vosu["target"], "attestation.vosu.target", re.compile(r"[a-z0-9_]+(?:-[a-z0-9_.]+)+"))
    _hex(vosu["sha256"], "attestation.vosu.sha256", 64)
    client = _obj(attestation["client"], "attestation.client", ("variant", "version", "sha256"))
    if client["variant"] != variant:
        raise GoldenError("attestation.client.variant: wrong platform client")
    _str(client["version"], "attestation.client.version", re.compile(r"[0-9A-Za-z.+_-]{1,64}"))
    _hex(client["sha256"], "attestation.client.sha256", 64)
    authorization = _obj(capture["authorization"], "capture.authorization", (
        "reference", "platform", "source_revision", "vosu_commit", "client_version", "scenario_set"))
    _str(authorization["reference"], "authorization.reference", re.compile(r"[A-Za-z0-9._:#-]{1,128}"))
    expected = {"platform": platform, "source_revision": PINNED_SOURCE_REVISION,
                "vosu_commit": vosu["commit"], "client_version": client["version"],
                "scenario_set": scenario}
    for key, value in expected.items():
        if authorization[key] != value:
            raise GoldenError(f"authorization.{key}: capture differs from the owner authorization")
    return {"platform": platform, "windows": os_name == "windows", "scenario_set": scenario,
            "source": {"kind": source["kind"], "sha256": source["sha256"]},
            "vosu": {key: vosu[key] for key in ("commit", "version", "profile", "features",
                                                 "target", "sha256")},
            "client": dict(client), "authorization": authorization["reference"]}


def _roots(value: Any, windows: bool) -> list[Root]:
    roots: list[Root] = []
    for index, item in enumerate(_list(value, "capture.roots")):
        _obj(item, f"capture.roots[{index}]", ("slot", "private"))
        slot = _str(item["slot"], f"capture.roots[{index}].slot")
        private = _str(item["private"], f"capture.roots[{index}].private")
        validate_private_root(slot, private, windows)
        roots.append(Root(slot, private, synthetic_root(slot, windows)))
    if len({root.slot for root in roots}) != len(roots):
        raise GoldenError("capture.roots: duplicate slot")
    return roots


def _assets(value: Any, roots: list[Root]) -> dict[str, dict[str, Any]]:
    slots = {root.slot for root in roots}
    assets: dict[str, dict[str, Any]] = {}
    placements: set[tuple[str, str]] = set()
    for index, item in enumerate(_list(value, "capture.assets")):
        at = f"capture.assets[{index}]"
        _obj(item, at, ("id", "slot", "relative", "media_type", "generator", "license",
                        "sha256", "b64"))
        asset_id = _str(item["id"], f"{at}.id", _ASSET_ID)
        if len(asset_id) > 120 or _WINDOWS_RESERVED.fullmatch(asset_id) or asset_id in assets:
            raise GoldenError(f"{at}.id: duplicate, reserved or too long")
        if item["slot"] not in slots:
            raise GoldenError(f"{at}.slot: not an attested root slot")
        relative = _str(item["relative"], f"{at}.relative")
        segments = relative.split("/")
        if any(segment in ("", ".", "..") or re.search(r'[\\:*?"<>|\0]', segment)
               for segment in segments):
            raise GoldenError(f"{at}.relative: expected a plain relative path")
        if (item["slot"], relative) in placements:
            raise GoldenError(f"{at}: two assets share one replay path")
        placements.add((item["slot"], relative))
        for key in ("media_type", "generator", "license"):
            _str(item[key], f"{at}.{key}")
        data = _bytes({"b64": item["b64"]}, f"{at}.b64")
        if hashlib.sha256(data).hexdigest() != _hex(item["sha256"], f"{at}.sha256", 64):
            raise GoldenError(f"{at}: bytes do not match sha256")
        assets[asset_id] = {"id": asset_id, "slot": item["slot"], "relative": relative,
                            "media_type": item["media_type"], "generator": item["generator"],
                            "license": item["license"], "sha256": item["sha256"],
                            "bytes": len(data), "data": data}
    return dict(sorted(assets.items()))


# ---------------------------------------------------------------- cases


def _request_line(request: str) -> tuple[str, str]:
    line = request.split("\r\n", 1)[0]
    parts = line.split(" ")
    return (parts[0], parts[1]) if len(parts) == 3 else ("", "")


def _check_target(case_id: str, request: str) -> None:
    line = request.split("\r\n", 1)[0]
    decoded = unquote(unquote(line)).lower().replace("\\", "/")
    decoded = re.sub(r"/+", "/", decoded)
    if "/api/ingame" in decoded:
        raise GoldenError(f"case {case_id}: /api/ingame is excluded from M4b goldens")
    if "/__vosu_test" in decoded:
        raise GoldenError(f"case {case_id}: the harness capture endpoint is not a golden route")


def _check_headers(case_id: str, headers: list[list[str]], part: str) -> None:
    for name, _value in headers:
        lowered = name.lower()
        if lowered == CAPABILITY_HEADER:
            raise GoldenError(f"case {case_id}: {part} carries the capability header")
        if lowered in CREDENTIAL_HEADERS:
            raise GoldenError(f"case {case_id}: {part} carries credential header {lowered}")


def _request_headers(request: str) -> list[list[str]]:
    head = request.partition("\r\n\r\n")[0]
    headers = []
    for line in head.split("\r\n")[1:]:
        name, sep, value = line.partition(":")
        if sep:
            headers.append([name.strip(), value.strip()])
    return headers


def _validator_view(request: str) -> str:
    """The request with conditional validator header values blanked."""
    head, sep, body = request.partition("\r\n\r\n")
    lines = head.split("\r\n")
    for index, line in enumerate(lines[1:], start=1):
        name, colon, _value = line.partition(":")
        if colon and name.strip().lower() in VALIDATOR_REQUEST_HEADERS:
            lines[index] = f"{name}:"
    return "\r\n".join(lines) + sep + body


def _case(value: Any, index: int) -> dict[str, Any]:
    where = f"capture.cases[{index}]"
    _obj(value, where, ("id", "family", "barrier", "dump_schema", "conditional", "attempts"))
    case_id = _str(value["id"], f"{where}.id", _CASE_ID)
    if len(case_id) > 120:
        raise GoldenError(f"{where}.id: longer than 120 characters")
    family = _str(value["family"], f"case {case_id}.family", choices=FAMILIES)
    if NON_GOLDEN_SEGMENTS & set(case_id.split(".")):
        raise GoldenError(f"case {case_id}: negative and diagnostic route cases are never goldens")
    if not case_id.startswith(family + "."):
        raise GoldenError(f"case {case_id}: id does not start with its family")
    _str(value["barrier"], f"case {case_id}.barrier", choices=BARRIERS)
    _str(value["dump_schema"], f"case {case_id}.dump_schema", choices=ENVELOPE_SCHEMAS)
    conditional = value["conditional"]
    if conditional is not None and (family != "files" or conditional not in ("etag", "last-modified")):
        raise GoldenError(f"case {case_id}.conditional: only files cases declare etag/last-modified")
    attempts = _list(value["attempts"], f"case {case_id}.attempts")
    if not 1 <= len(attempts) <= MAX_ATTEMPTS:
        raise GoldenError(f"case {case_id}: expected 1..{MAX_ATTEMPTS} attempts")
    ws = family == "ws"
    for number, attempt in enumerate(attempts, start=1):
        at = f"case {case_id}.attempts[{number - 1}]"
        if not isinstance(attempt, dict) or _int(attempt.get("attempt"), f"{at}.attempt") != number:
            raise GoldenError(f"{at}: attempts must be numbered 1..n in order")
        last = number == len(attempts)
        if attempt.get("outcome") == "failed" and not last:
            _obj(attempt, at, ("attempt", "outcome", "reason"))
            _str(attempt["reason"], f"{at}.reason")
            continue
        if attempt.get("outcome") == crash.OUTCOME and last:
            return {"id": case_id, "family": family, "barrier": value["barrier"],
                    "dump_schema": value["dump_schema"], "attempts": len(attempts),
                    crash.OUTCOME: crash.parse_attempt(attempt, at, case_id, family, number)}
        if attempt.get("outcome") != "complete" or not last:
            raise GoldenError(f"case {case_id}: incomplete; only the final attempt may be "
                              "complete and every earlier one must be failed")
        _obj(attempt, at, ("attempt", "outcome", "source", "vosu"))
        source = _list(attempt["source"], f"{at}.source")
        if len(source) != 2:
            raise GoldenError(f"case {case_id}: upstream sandwich needs exactly two records")
        records = [_record(item, f"{at}.source[{side}]", number, ws)
                   for side, item in enumerate(source)]
        vosu = _obj(attempt["vosu"], f"{at}.vosu", ("dump_before", "route", "dump_after"))
        route = _record(vosu["route"], f"{at}.vosu.route", number, ws)
        dumps = []
        for key in ("dump_before", "dump_after"):
            dump = _obj(vosu[key], f"{at}.vosu.{key}", ("attempt", "envelope"))
            if _int(dump["attempt"], f"{at}.vosu.{key}.attempt") != number:
                raise GoldenError(f"case {case_id}: {key} belongs to another attempt "
                                  "(attempts are never mixed)")
            dumps.append(_envelope(dump["envelope"], f"case {case_id}.{key}"))
        return {"id": case_id, "family": family, "barrier": value["barrier"],
                "dump_schema": value["dump_schema"], "conditional": conditional,
                "attempts": len(attempts), "source": records, "route": route, "dumps": dumps}
    raise GoldenError(f"case {case_id}: no complete attempt")


def _check_sandwiches(case: dict[str, Any]) -> None:
    case_id = case["id"]
    first, second = case["source"]
    route = case["route"]
    for record, part in ((first, "upstream"), (second, "upstream"), (route, "vosu route")):
        _check_target(case_id, record["request"])
        _check_headers(case_id, _request_headers(record["request"]), f"{part} request")
        _check_headers(case_id, record["headers"], f"{part} response")
    if first["request"] != second["request"]:
        raise GoldenError(f"case {case_id}: upstream sandwich requests differ")
    if case["conditional"] is None:
        if route["request"] != first["request"]:
            raise GoldenError(f"case {case_id}: vosu and upstream request bytes differ")
    elif _validator_view(route["request"]) != _validator_view(first["request"]):
        raise GoldenError(f"case {case_id}: vosu and upstream requests differ beyond the "
                          "declared conditional validator")
    before, after = case["dumps"]
    differs = dump_sandwich_difference(before, after)
    if differs is not None:
        raise GoldenError(f"case {case_id}: vosu dump sandwich differs in {differs}")
    if before["schema"] != case["dump_schema"]:
        raise GoldenError(f"case {case_id}: dump schema differs from the case's dump_schema")
    method, target = _request_line(first["request"])
    path = target.partition("?")[0]
    if case["family"] == "state-http" and method in ("GET", "HEAD") and path in STATE_ROUTES:
        if STATE_ROUTES[path] != before["schema"]:
            raise GoldenError(f"case {case_id}: state route must dump its own schema")
        # The dump records the adapter's GET response unless a ledger row decides the route
        # first; a HEAD status is a route property (GOLDEN_CONTRACT §6: status and headers
        # come from the route record).
        if method == "GET":
            ledger.check_state_route(case_id, path, route, before)
    if case["family"] == "files":
        for name in ("etag", "last-modified"):
            values = [[v for n, v in record["headers"] if n.lower() == name]
                      for record in (first, second)]
            if values[0] != values[1]:
                rule = "header-etag" if name == "etag" else "header-last-modified"
                raise GoldenError(f"case {case_id}: {rule} invariant: {name} must be present and "
                                  "equal in both upstream sandwich responses")


# ---------------------------------------------------------------- normalization


class _Platform:
    """Normalization state for one platform capture."""

    def __init__(self, identity: dict[str, Any], roots: list[Root],
                 assets: dict[str, dict[str, Any]]) -> None:
        self.identity = identity
        self.rewriter = RootRewriter(roots, identity["windows"])
        self.validators = Validators()
        self.assets = assets
        self.cbor_texts: list[str] = []
        self.cbor_blobs: list[bytes] = []

    def text(self, value: str, family: str, part: str | None, used: set[str], *,
             request_line: bool = False) -> str:
        value, count = (self.rewriter.request_line if request_line else self.rewriter.text)(value)
        if count:
            used.add("root-prefix")
        if part is not None:
            value, stamped = apply_timestamp_pointers(value, family, part)
            if stamped:
                used.add("client-timestamp-pointer")
        return value

    def request(self, case: dict[str, Any], used: set[str]) -> str:
        head, sep, body = case["source"][0]["request"].partition("\r\n\r\n")
        if self.rewriter.text(body)[1]:
            raise GoldenError(f"case {case['id']}: root-prefix inside a request body is unsupported")
        lines = head.split("\r\n")
        validators: set[str] = set()
        for index, line in enumerate(lines):
            name, colon, value = line.partition(":")
            if index and colon and name.strip().lower() in VALIDATOR_REQUEST_HEADERS:
                stripped = value.strip()
                replaced, rules = self.validators.request_value(name.strip(), stripped)
                line = f"{name}:{value.replace(stripped, replaced, 1)}" if stripped else line
                validators |= rules
            lines[index] = self.text(line, case["family"], None, used, request_line=not index)
        if case["conditional"] is not None:
            rule = f"header-{case['conditional']}"
            if rule not in validators:
                raise GoldenError(f"case {case['id']}: declared {case['conditional']} validator "
                                  "was not observed in an upstream files response")
        used |= validators
        return "\r\n".join(lines) + sep + body

    def body(self, case_id: str, family: str, data: bytes, used: set[str]) -> tuple[dict[str, Any], int]:
        text = _utf8(data)
        if text is not None:
            normalized = self.text(text, family, "body", used)
            return {"text": normalized}, len(normalized.encode("utf-8"))
        for asset in self.assets.values():
            offset = asset["data"].find(data)
            if offset >= 0:
                return {"asset": asset["id"], "offset": offset, "length": len(data)}, len(data)
        raise GoldenError(f"case {case_id}: binary body is not an approved asset or a slice of one")

    def expected(self, case: dict[str, Any], record: dict[str, Any], used: set[str],
                 get_lengths: dict[str, tuple[int, int]]) -> dict[str, Any]:
        family = case["family"]
        if record["outcome"] is not None:
            return {"outcome": record["outcome"]}
        headers, rules = normalize_response_headers(
            record["headers"], family == "files", self.validators)
        used |= rules
        body, length = self.body(case["id"], family, record["body"], used)
        lengths = (len(record["body"]), length)
        if case["method"] == "HEAD":
            # A HEAD content-length describes the matching GET body (GOLDEN_CONTRACT §6).
            lengths = get_lengths.get("GET " + record["request"][len("HEAD "):], (0, 0))
        projected: list[list[str]] = []
        for name, value in headers:
            lowered = name.lower()
            if lowered in COMPARED_HEADERS or lowered.startswith("access-control-"):
                projected.append([lowered, self.text(value, family, None, used)])
        if lengths[0] != lengths[1]:
            for item in projected:
                if item[0] == "content-range":
                    raise GoldenError(f"case {case['id']}: root-prefix changed a partial body")
                if item[0] == "content-length":
                    if item[1] != str(lengths[0]):
                        raise GoldenError(f"case {case['id']}: raw content-length differs from "
                                          "the raw body length; cannot recompute it")
                    item[1] = str(lengths[1])
        projected.sort(key=lambda item: item[0])
        expected: dict[str, Any] = {"status": record["status"], "headers": projected, "body": body}
        if record["frames"] is not None:
            expected["frames"] = [self.frame(case, frame, used) for frame in record["frames"]]
        return expected

    def frame(self, case: dict[str, Any], frame: dict[str, Any], used: set[str]) -> dict[str, Any]:
        text = _utf8(frame["payload"])
        if frame["opcode"] >= 8:
            payload = {"text": text} if text is not None else {"hex": frame["payload"].hex()}
            length = len(frame["payload"])
        elif text is None:
            raise GoldenError(f"case {case['id']}: binary WebSocket data frame is not a golden payload")
        elif frame["fin"] and frame["opcode"] == 1:
            text = self.text(text, case["family"], "ws.frame", used)
            payload, length = {"text": text}, len(text.encode("utf-8"))
        elif self.rewriter.text(text)[1]:
            raise GoldenError(f"case {case['id']}: root-prefix inside a fragmented frame is unsupported")
        else:
            payload, length = {"text": text}, len(frame["payload"])
        return {"from": frame["from"], "fin": frame["fin"], "opcode": frame["opcode"],
                "masked": frame["masked"], "length": length, "payload": payload}

    def envelope(self, envelope: dict[str, Any], used: set[str]) -> dict[str, Any]:
        out = copy.deepcopy(envelope)

        def rewrite(value: str) -> str:
            return self.text(value, "", None, used)

        def walk(value: Any) -> Any:
            if isinstance(value, str):
                return rewrite(value)
            if isinstance(value, list):
                return [walk(item) for item in value]
            if isinstance(value, dict):
                return {key: item if key in ("value_cbor_b64", "digest", "format", "schema")
                        else walk(item) for key, item in value.items()}
            return value

        out = walk(out)
        for entry in out["entries"]:
            if entry["value_cbor_b64"] is None:
                continue
            data = base64.b64decode(entry["value_cbor_b64"], validate=True)
            rewritten, texts, blobs = rewrite_cbor_text(data, rewrite)
            self.cbor_texts.extend(texts)
            self.cbor_blobs.extend(blobs)
            if rewritten != data:
                entry["value_cbor_b64"] = base64.b64encode(rewritten).decode("ascii")
        out["digest"] = envelope_digest(out)
        return out


def _dump(value: Any) -> bytes:
    return (json.dumps(value, indent=2, ensure_ascii=False) + "\n").encode("utf-8")


def _platform_files(capture: Any, review_receipt: str | None,
                    provenance: Provenance) -> tuple[str, dict[str, bytes], dict[str, Any]]:
    _obj(capture, "capture", ("format", "source_revision", "platform", "scenario_set",
                              "authorization", "attestation", "roots", "assets",
                              "expected_cases", "cases"))
    identity = _check_identity(capture)
    roots = _roots(capture["roots"], identity["windows"])
    state = _Platform(identity, roots, _assets(capture["assets"], roots))
    cases = [_case(item, index) for index, item in enumerate(_list(capture["cases"], "capture.cases"))]
    ids = [case["id"] for case in cases]
    if not cases or len(set(ids)) != len(ids):
        raise GoldenError("capture.cases: empty or duplicate case IDs")
    expected_ids = _list(capture["expected_cases"], "capture.expected_cases")
    if (not all(isinstance(item, str) for item in expected_ids)
            or sorted(expected_ids) != sorted(ids) or len(set(expected_ids)) != len(expected_ids)):
        raise GoldenError("capture is incomplete: recorded cases differ from expected_cases")
    crashed = [crash.check_case(case) for case in cases if crash.OUTCOME in case]
    cases = [case for case in cases if crash.OUTCOME not in case]
    cases.sort(key=lambda case: case["id"])
    for case in cases:
        _check_sandwiches(case)
        case["method"] = _request_line(case["source"][0]["request"])[0]
        if case["family"] == "files":
            for record in case["source"]:
                state.validators.observe(record["headers"])
    get_lengths: dict[str, tuple[int, int]] = {}
    for case in cases:
        record = case["source"][0]
        if case["method"] == "GET" and record["outcome"] is None and _utf8(record["body"]) is not None:
            get_lengths[record["request"]] = (
                len(record["body"]), state.body(case["id"], case["family"], record["body"], set())[1])

    platform = identity["platform"]
    files: dict[str, bytes] = {}
    listing = []
    for case in cases:
        used: set[str] = set()
        request = state.request(case, used)
        first = state.expected(case, case["source"][0], used, get_lengths)
        second = state.expected(case, case["source"][1], set(), get_lengths)
        target = _request_line(case["source"][0]["request"])[1].partition("?")[0]
        try:
            views = [volatile.expected_view(target, item) for item in (first, second)]
        except volatile.VolatileError as exc:
            raise GoldenError(f"case {case['id']}: {exc}") from exc
        if views[0][0] != views[1][0]:
            raise GoldenError(f"case {case['id']}: upstream sandwich responses differ after "
                              "approved normalization and the volatile ledger")
        case_file = {
            "format": CASE_FORMAT, "id": case["id"], "family": case["family"],
            "barrier": case["barrier"], "conditional": case["conditional"],
            "attempts": case["attempts"], "normalized": sorted(used),
            "request": {"text": request}, "expected": first,
            "replay": state.envelope(case["dumps"][0], used),
        }
        if views[0][1]:
            case_file["volatile"] = views[0][1]
        files[f"cases/{case['id']}.json"] = _dump(case_file)
        listing.append({"id": case["id"], "family": case["family"], "barrier": case["barrier"],
                        "attempts": case["attempts"], "file": f"cases/{case['id']}.json"})
    for asset in state.assets.values():
        files[f"assets/{asset['id']}"] = asset["data"]
    files["assets.json"] = _dump({"format": ASSETS_FORMAT, "assets": [
        {key: asset[key] for key in ("id", "slot", "relative", "media_type", "generator",
                                      "license", "sha256", "bytes")}
        for asset in state.assets.values()]})
    files["manifest.json"] = _dump({
        "format": PLATFORM_FORMAT, "normalization_version": NORMALIZATION_VERSION,
        "provenance": provenance, "source_revision": PINNED_SOURCE_REVISION,
        "platform": platform, "client": identity["client"], "source": identity["source"],
        "vosu": identity["vosu"], "scenario_set": identity["scenario_set"],
        "authorization": identity["authorization"], "review_receipt": review_receipt,
        "synthetic_roots": {root.slot: root.synthetic for root in sorted(roots, key=lambda r: r.slot)},
        "case_counts": {family: sum(case["family"] == family for case in cases) for family in FAMILIES},
        "cases": listing,
    })
    scan = _scan(platform, files, state)
    files["brand-inventory.txt"] = _brand_inventory(platform, files)
    json_bytes = sum(len(data) for rel, data in files.items() if not rel.startswith("assets/"))
    asset_bytes = sum(len(data) for rel, data in files.items() if rel.startswith("assets/"))
    inventory = {
        "format": INVENTORY_FORMAT, "source_revision": PINNED_SOURCE_REVISION,
        "platform": platform, "normalization_version": NORMALIZATION_VERSION,
        "files": _inventory_entries(files), "privacy_scan": scan,
        "budget": {"json_bytes": json_bytes, "json_limit": JSON_BUDGET,
                   "asset_bytes": asset_bytes, "asset_limit": ASSET_BUDGET},
    }
    files["inventory.json"] = _dump(inventory)
    json_bytes += len(files["inventory.json"])
    if json_bytes > JSON_BUDGET or asset_bytes > ASSET_BUDGET:
        raise GoldenError(f"{platform}: over budget ({json_bytes} JSON bytes of {JSON_BUDGET}, "
                          f"{asset_bytes} asset bytes of {ASSET_BUDGET}); reduce the scenario "
                          "or get owner approval")
    summary = {"cases": len(cases), "families": json.loads(files["manifest.json"])["case_counts"],
               crash.OUTCOME: sorted(crashed, key=lambda case: case["id"])}
    return platform, files, summary


def _string_leaves(value: Any, key: str | None = None) -> list[str]:
    if isinstance(value, str):
        return [] if key == "value_cbor_b64" else [value]
    if isinstance(value, list):
        return [leaf for item in value for leaf in _string_leaves(item)]
    if isinstance(value, dict):
        return [leaf for name, item in value.items() for leaf in [name, *_string_leaves(item, name)]]
    return []


def _scan(platform: str, files: dict[str, bytes], state: _Platform) -> dict[str, Any]:
    """Fail on private data or hidden old-brand text; return the clean scan record."""
    rewriter = state.rewriter
    text_files = 0
    for rel, data in sorted(files.items()):
        text = _utf8(data)
        if text is None:
            findings = [f"unnormalized private root {slot!r}"
                        for slot in rewriter.leftover_slots(data.decode("latin-1"))]
        else:
            text_files += 1
            # A JSON file adds one escaping level; scan its decoded strings, where roots
            # keep the spelling they had on the wire. Dump CBOR is scanned decoded below.
            leaves = _string_leaves(json.loads(text)) if rel.endswith(".json") else [text]
            findings = [finding for leaf in leaves for finding in privacy_findings(leaf, rewriter)]
        if findings:
            raise GoldenError(f"privacy scan: {findings[0]} in {platform}/{rel}")
    # Dump entry values are base64 CBOR, invisible to check-brand.py and to the file scan
    # above: every decoded text string at any depth, map keys included, is scanned here
    # after root-prefix rewrote it.
    for text in state.cbor_texts:
        findings = privacy_findings(text, rewriter)
        if findings:
            raise GoldenError(f"privacy scan: {findings[0]} in a {platform} dump CBOR string")
        if OLD_BRAND.search(text):
            raise GoldenError(f"{platform}: old-brand text inside base64 CBOR would hide it from "
                              "check-brand; it cannot be a golden replay input")
    for blob in state.cbor_blobs:
        latin = blob.decode("latin-1")
        if rewriter.leftover_slots(latin) or OLD_BRAND.search(latin):
            raise GoldenError(f"{platform}: private root or old-brand text inside a CBOR byte string")
    return {"result": "clean", "text_files": text_files,
            "binary_files": len(files) - text_files,
            "cbor_text_strings": len(state.cbor_texts),
            "cbor_byte_strings": len(state.cbor_blobs),
            "cbor_old_brand_strings": 0,
            "attested_roots": len(rewriter.roots),
            "checks": ["attested private roots", "drive, admin-share and device paths",
                       "profile and private POSIX directories", "capability header",
                       "credential headers",
                       "dump CBOR text strings at any depth including map keys",
                       "old-brand text hidden in dump CBOR"]}


def _brand_inventory(platform: str, files: dict[str, bytes]) -> bytes:
    lines = [f"# Exact scripts/brand-allowlist.txt candidate lines for {platform}, "
             "generated by golden; add them only in the reviewed fixture commit."]
    for rel, data in sorted(files.items()):
        text = _utf8(data)
        count = len(OLD_BRAND.findall(text)) if text is not None else 0
        if count:
            category, reason = (
                ("fixture", "generated vosu-owned replay asset at the pinned source revision")
                if rel.startswith("assets/") else
                ("parity", "upstream wire observation and its replay input at the pinned source revision"))
            lines.append(f"tests/fixtures/upstream/{PINNED_SOURCE_REVISION}/{platform}/{rel} "
                         f"{count} {category} {reason}")
    return ("\n".join(lines) + "\n").encode("utf-8")


def _inventory_entries(files: dict[str, bytes]) -> list[dict[str, Any]]:
    return [{"path": rel, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
            for rel, data in sorted(files.items())]


# ---------------------------------------------------------------- candidate directory


def _cargo_pin(repo_root: Path) -> str:
    try:
        text = (repo_root / "Cargo.toml").read_text(encoding="utf-8")
    except OSError as exc:
        raise GoldenError(f"cannot read Cargo.toml: {exc.strerror}") from exc
    match = re.search(r'(?m)^source-commit\s*=\s*"([0-9a-f]{40})"\s*$', text)
    if match is None:
        raise GoldenError("Cargo.toml has no source-commit pin")
    return match.group(1)


def _prepare_target(candidate_dir: str, repo_root: Path) -> Path:
    target = Path(candidate_dir).resolve()
    fixtures = (repo_root / "tests/fixtures/upstream").resolve()
    if target.is_relative_to(fixtures):
        raise GoldenError("candidate directory is inside the committed fixture tree "
                          "tests/fixtures/upstream/; write it elsewhere for review")
    if target.is_relative_to(repo_root.resolve()) and (repo_root / ".git").exists():
        listed = subprocess.run(
            ["git", "-C", str(repo_root), "ls-files", "-z", "--", str(target)],
            check=False, capture_output=True)
        if listed.returncode != 0:
            raise GoldenError("cannot list git-tracked files under the candidate directory")
        tracked = listed.stdout
        if tracked:
            raise GoldenError("candidate directory contains git-tracked files")
    if target.exists() and (not target.is_dir() or any(target.iterdir())):
        raise GoldenError("candidate directory exists and is not an empty directory")
    return target


def _publish(target: Path, files: dict[str, bytes]) -> None:
    """Stage beside the target, then publish the whole directory with one rename."""
    target.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(tempfile.mkdtemp(prefix=".golden-candidate-", dir=target.parent))
    try:
        for rel, data in sorted(files.items()):
            path = staging / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            with open(path, "xb") as handle:
                handle.write(data)
                handle.flush()
                os.fsync(handle.fileno())
        if target.exists():
            target.rmdir()  # Fails rather than replacing anything written concurrently.
        os.rename(staging, target)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def golden(captures: list[str], candidate_dir: str, approved_rules: str | None = None, *,
           review_receipt: str | None = None, provenance: Provenance = "captured",
           repo_root: Path = REPO_ROOT) -> int:
    """Write one candidate. `provenance="synthetic"` labels a vosu-generated mechanics
    fixture (GOLDEN_CONTRACT.md §9); it is never pilot or full evidence, and
    `adapter_golden` refuses it under `tests/fixtures/upstream/<pin>/`."""
    if provenance not in ("captured", "synthetic"):
        raise GoldenError("provenance must be 'captured' or 'synthetic'")
    rules_path = Path(approved_rules) if approved_rules else DEFAULT_APPROVED_RULES
    try:
        check_approved_rules(rules_path.read_text(encoding="utf-8"))
    except OSError as exc:
        raise GoldenError(f"cannot read approved rules: {exc.strerror}") from exc
    if _cargo_pin(repo_root) != PINNED_SOURCE_REVISION:
        raise GoldenError("Cargo source-commit differs from PINNED_SOURCE_REVISION")
    if review_receipt is not None:
        _str(review_receipt, "--review-receipt", re.compile(r"[A-Za-z0-9._:#-]{1,128}"))
    target = _prepare_target(candidate_dir, repo_root)
    files: dict[str, bytes] = {}
    platforms: dict[str, Any] = {}
    for path in captures:
        platform, platform_files, summary = _platform_files(_load_capture(path), review_receipt,
                                                            provenance)
        if platform in platforms:
            raise GoldenError(f"two captures for platform {platform}")
        platforms[platform] = summary
        files.update({f"{platform}/{rel}": data for rel, data in platform_files.items()})
        summary["inventory_sha256"] = hashlib.sha256(platform_files["inventory.json"]).hexdigest()
    files["manifest.json"] = _dump({
        "format": FIXTURE_FORMAT, "normalization_version": NORMALIZATION_VERSION,
        "provenance": provenance, "source_revision": PINNED_SOURCE_REVISION,
        "platforms": sorted(platforms),
    })
    files["inventory.json"] = _dump({
        "format": INVENTORY_FORMAT, "source_revision": PINNED_SOURCE_REVISION,
        "normalization_version": NORMALIZATION_VERSION, "files": _inventory_entries(files),
    })
    _publish(target, files)
    print(json.dumps({
        "candidate": str(target), "source_revision": PINNED_SOURCE_REVISION,
        "inventory_sha256": hashlib.sha256(files["inventory.json"]).hexdigest(),
        "files": len(files), "platforms": dict(sorted(platforms.items())),
        "privacy_scan": "clean",
    }, sort_keys=True))
    return 0
