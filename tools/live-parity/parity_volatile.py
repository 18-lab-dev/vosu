"""Reviewed volatile-field ledger for golden sandwiches and barriers (M4b O0 dry run).

A volatile field is a live-advancing value that two reads of one stable barrier can never
reproduce byte for byte: the menu track clock and upstream's decaying `bassDensity`. Each
entry names its exact scope, its rule and its authority. A sandwich compares a volatile
field by presence, JSON type and the stated rule, never by value; other values follow
GOLDEN_CONTRACT.md §7. Nothing is normalized in the fixture: the first upstream record
keeps its raw value, and the private receipt lists the IDs needed for the comparison.
"""
from __future__ import annotations

from dataclasses import dataclass
import base64
import json
import math
import struct
from typing import Any

AUTHORITY = ("Main contract decision 2026-11-25 (M4b O0 dry run, finding 2): live-advancing "
             "values are compared by presence, type and rule, never by value")

# GOLDEN_CONTRACT.md §7, also implemented in tests/adapter_golden/compare.rs.
JSON_RELATIVE_TOLERANCE = 1e-12

V2_PATHS = ("/json/v2", "/json/v2/precise", "/websocket/v2", "/websocket/v2/precise")
SC_PATHS = ("/json/sc", "/tokens")
V1_PATHS = ("/json", "/ws")
SCHEMA_ROUTE = {"v1": "/json", "sc": "/json/sc", "v2": "/json/v2", "v2-precise": "/json/v2/precise"}


@dataclass(frozen=True)
class VolatileField:
    """A JSON Pointer inside the payload of the named routes (HTTP body or WS text frame)."""
    id: str
    paths: tuple[str, ...]
    pointer: str
    rule: str
    reason: str


@dataclass(frozen=True)
class VolatileEntry:
    """A pointer inside one vosu dump entry's CBOR value."""
    id: str
    ns: str
    entry: str
    pointer: str
    rule: str
    reason: str


FIELDS = (
    VolatileField("upstream-bass-density", V2_PATHS, "/settings/bassDensity", "nonnegative-number",
                  "derived from the audio amplitude and decays continuously, even while paused"),
    VolatileField("menu-time-v2-live", V2_PATHS, "/beatmap/time/live", "nonnegative-number",
                  "the menu and song-select track clock advances while the barrier holds"),
    VolatileField("menu-time-v1-current", V1_PATHS, "/menu/bm/time/current", "nonnegative-number",
                  "the menu and song-select track clock advances while the barrier holds"),
    VolatileField("menu-time-sc-time", SC_PATHS, "/time", "nonnegative-number",
                  "the menu and song-select track clock advances while the barrier holds"),
    VolatileField("menu-time-sc-map-position", SC_PATHS, "/mapPosition", "string",
                  "formatted from the advancing track clock"),
    VolatileField("menu-time-sc-time-left", SC_PATHS, "/timeLeft", "string",
                  "formatted from the advancing track clock"),
)
ENTRIES = (
    VolatileEntry("dump-live-time", "vosu.osu", "/beatmap", "/time/live", "nonnegative-number",
                  "the published track clock entry advances while the barrier holds"),
)
# Bus counters that a volatile entry publication advances: compared as non-decreasing.
# The provider generation stays exact (only `sc` reads exempt it, GOLDEN_CONTRACT §6).
MONOTONIC_KEYS = ("bus_revision", "namespace_revisions")
LEDGER_IDS = frozenset([f.id for f in FIELDS] + [e.id for e in ENTRIES] + ["dump-revisions"])


class VolatileError(ValueError):
    """A volatile field broke its stated rule (wrong type, range or presence)."""


# ---------------------------------------------------------------- comparison values


def _number(token: str) -> float:
    value = float(token)
    if not math.isfinite(value):
        raise ValueError("JSON number is not finite binary64")
    return value


def _parse_json(text: str) -> Any:
    return json.loads(text, parse_float=_number, parse_int=_number, parse_constant=_number)


@dataclass(eq=False)
class JsonPayload:
    """A parsed body or frame; use the same equality inside every sandwich view."""
    value: Any

    def __eq__(self, other: object) -> bool:
        return isinstance(other, JsonPayload) and _json_equal(self.value, other.value)


def _json_equal(left: Any, right: Any) -> bool:
    if type(left) is not type(right):
        return False
    if isinstance(left, float):
        return abs(left - right) <= JSON_RELATIVE_TOLERANCE * max(abs(left), abs(right))
    if isinstance(left, dict):
        return left.keys() == right.keys() and all(
            _json_equal(value, right[key]) for key, value in left.items())
    if isinstance(left, list):
        return len(left) == len(right) and all(_json_equal(a, b) for a, b in zip(left, right))
    return left == right


def without_charset(value: str) -> str:
    """Drop only charset parameters; do not split a quoted parameter value."""
    start = 0
    quoted = escaped = False
    parts = []
    for index, char in enumerate(value):
        if escaped:
            escaped = False
        elif quoted and char == "\\":
            escaped = True
        elif char == '"':
            quoted = not quoted
        elif char == ";" and not quoted:
            parts.append(value[start:index])
            start = index + 1
    parts.append(value[start:])
    return ";".join([parts[0].rstrip(), *[
        part for part in parts[1:] if "=" not in part
        or part.split("=", 1)[0].strip().lower() != "charset"]])


def is_json_media_type(value: str) -> bool:
    media = value.partition(";")[0].strip().lower()
    return media == "application/json" or ("/" in media and media.endswith("+json"))


def body_length(status: int, size: int, *, head: bool) -> int | None:
    # HEAD and 304 can describe a representation not carried in the response.
    return None if head or status == 304 else size


def check_content_length(value: str, size: int | None) -> None:
    if not value.isascii() or not value.isdecimal() \
            or (size is not None and (value.lstrip("0") or "0") != str(size)):
        raise VolatileError("content-length differs from the body length"
                            if size is not None else "invalid content-length")


def _cbor(data: bytes, pos: int = 0) -> tuple[Any, int]:
    head = data[pos]
    major, info = head >> 5, head & 31
    pos += 1
    if info < 24:
        arg = info
    elif info in (24, 25, 26, 27):
        size = 1 << (info - 24)
        arg = int.from_bytes(data[pos:pos + size], "big")
        if major == 7:
            fmt = {2: ">e", 4: ">f", 8: ">d"}.get(size)
            if fmt is not None:
                return ("num", repr(struct.unpack(fmt, data[pos:pos + size])[0])), pos + size
        pos += size
    else:
        raise VolatileError("indefinite or reserved CBOR length")
    if major == 0:
        return ("num", str(arg)), pos
    if major == 1:
        return ("num", str(-1 - arg)), pos
    if major in (2, 3):
        raw = data[pos:pos + arg]
        return (("bytes", raw.hex()) if major == 2 else ("text", raw.decode("utf-8"))), pos + arg
    if major == 4:
        items = []
        for _ in range(arg):
            item, pos = _cbor(data, pos)
            items.append(item)
        return items, pos
    if major == 5:
        pairs = []
        for _ in range(arg):
            key, pos = _cbor(data, pos)
            value, pos = _cbor(data, pos)
            pairs.append([key, value])
        return ("{}", pairs), pos
    if major == 6:
        value, pos = _cbor(data, pos)
        return ("tag", arg, value), pos
    return ("simple", arg), pos


def _key(value: Any) -> str | None:
    if isinstance(value, str):
        return value
    if isinstance(value, tuple) and value[0] == "text":
        return value[1]
    return None


def cbor_text_fields(data: bytes, *paths: str) -> tuple[str | None, ...]:
    """Read selected text fields in a CBOR map by their slash-separated paths."""
    try:
        tree, end = _cbor(data)
        if end != len(data):
            raise VolatileError("trailing bytes in CBOR field lookup")
    except (IndexError, RecursionError, UnicodeDecodeError, struct.error) as exc:
        raise VolatileError("invalid CBOR in field lookup") from exc

    def text_at(path: str) -> str | None:
        node = tree
        for key in path.split("/")[1:]:
            if not isinstance(node, tuple) or node[0] != "{}":
                return None
            fields = [value for name, value in node[1] if _key(name) == key]
            if len(fields) != 1:
                return None
            node = fields[0]
        return node[1] if isinstance(node, tuple) and node[0] == "text" else None

    return tuple(text_at(path) for path in paths)


def _check(rule: str, value: Any, field_id: str) -> None:
    if rule == "string":
        ok = isinstance(value, str) or (isinstance(value, tuple) and value[0] == "text")
    else:
        try:
            number = value if isinstance(value, float) else (
                float(value[1]) if isinstance(value, tuple) and value[0] == "num" else None)
        except ValueError:
            number = None
        ok = number is not None and math.isfinite(number) and number >= 0
    if not ok:
        raise VolatileError(f"volatile field {field_id} breaks its rule {rule}")


def _mask(tree: Any, pointer: str, rule: str, field_id: str) -> tuple[Any, bool]:
    """`tree` with the value at `pointer` replaced by a rule-checked marker."""
    parts = [p.replace("~1", "/").replace("~0", "~") for p in pointer.split("/")[1:]]

    def walk(node: Any, index: int) -> tuple[Any, bool]:
        if index == len(parts):
            _check(rule, node, field_id)
            return ("volatile", field_id), True
        if isinstance(node, dict) and parts[index] in node:
            key = parts[index]
            item, found = walk(node[key], index + 1)
            return {**node, key: item}, found
        if isinstance(node, tuple) and node[0] == "{}":
            hits = [i for i, (k, _v) in enumerate(node[1]) if _key(k) == parts[index]]
            if len(hits) != 1:
                return node, False
            pairs = [list(p) for p in node[1]]
            pairs[hits[0]][1], found = walk(pairs[hits[0]][1], index + 1)
            return ("{}", pairs), found
        if isinstance(node, list) and parts[index].isdigit() and int(parts[index]) < len(node):
            items = list(node)
            items[int(parts[index])], found = walk(items[int(parts[index])], index + 1)
            return items, found
        return node, False
    return walk(tree, 0)


# ---------------------------------------------------------------- views


def payload_view(path: str, data: bytes, *, json_required: bool = True,
                 mask: bool = True) -> tuple[Any, list[str]]:
    """Parsed JSON and any ledger IDs used; untyped non-JSON text stays bytes."""
    if not data:
        return data, []
    try:
        tree = _parse_json(data.decode("utf-8"))
    except (ValueError, RecursionError) as exc:
        if json_required:
            raise VolatileError("expected a JSON payload") from exc
        return data, []
    used = []
    if mask:
        for field in FIELDS:
            if path in field.paths:
                tree, found = _mask(tree, field.pointer, field.rule, field.id)
                if found:
                    used.append(field.id)
    return JsonPayload(tree), used


def record_view(path: str, status: Any, headers: list[list[str]], body: bytes | dict,
                frames: list[tuple[Any, ...]] | None, *, head: bool = False,
                mask: bool = True) -> tuple[tuple[Any, ...], list[str]]:
    """Compare headers and body, plus (direction, fin, opcode, mask, payload) frames."""
    if isinstance(body, bytes):
        is_json = any(name.lower() == "content-type" and is_json_media_type(value)
                      for name, value in headers)
        view, used = payload_view(path, body, mask=mask) if is_json else (body, [])
        size = len(body)
    else:
        view, used, size = body, [], body["length"]
    shown = []
    for name, value in headers:
        name = name.lower()
        if name == "content-length":
            check_content_length(value, body_length(int(status), size, head=head))
            value = "<consistent length>"
        elif name == "content-type":
            value = without_charset(value)
        shown.append([name, value])
    frame_views = None
    if frames is not None:
        frame_views = []
        for direction, fin, opcode, masked, payload in frames:
            fview, fused = payload_view(path, payload, json_required=False, mask=mask) \
                if opcode == 1 and fin else (payload, [])
            used += [i for i in fused if i not in used]
            frame_views.append((direction, fin, opcode, masked, fview))
    return (status, sorted(shown), view, frame_views), used


def expected_view(path: str, expected: dict[str, Any], *, head: bool,
                  mask: bool) -> tuple[Any, list[str]]:
    """The comparable view of one normalized `golden` expected record."""
    if "outcome" in expected:
        return expected, []
    body = expected["body"]
    data = body["text"].encode("utf-8") if "text" in body else body
    frames = None
    if "frames" in expected:
        frames = [(f["from"], f["fin"], f["opcode"], f["masked"],
                   f["payload"]["text"].encode("utf-8") if "text" in f["payload"]
                   else bytes.fromhex(f["payload"]["hex"])) for f in expected["frames"]]
    return record_view(path, expected["status"], expected["headers"], data, frames,
                       head=head, mask=mask)


def sandwich(path: str, first: dict[str, Any], second: dict[str, Any], *,
             head: bool, synthetic: bool) -> list[str]:
    """Validate stability and return receipt diagnostics, never fixture metadata."""
    def view(record: dict[str, Any], *, mask: bool) -> tuple[Any, list[str]]:
        return expected_view(path, record, head=head, mask=mask)

    if view(first, mask=False)[0] == view(second, mask=False)[0]:
        if not synthetic:
            # Check scoped field types even when neither live value changed.
            view(first, mask=True)
        return []
    if synthetic:
        raise VolatileError("synthetic sandwich responses differ after approved comparison")
    first_view, used = view(first, mask=True)
    if first_view != view(second, mask=True)[0]:
        raise VolatileError("upstream sandwich responses differ after approved comparison "
                            "and the volatile ledger")
    return used


def _entries_view(entries: list[dict[str, Any]], used: list[str]) -> list[Any]:
    out = []
    for entry in entries:
        item = dict(entry)
        for rule in ENTRIES:
            if entry.get("ns") == rule.ns and entry.get("pointer") == rule.entry \
                    and isinstance(entry.get("value_cbor_b64"), str):
                tree, end = _cbor(base64.b64decode(entry["value_cbor_b64"]))
                if end != len(base64.b64decode(entry["value_cbor_b64"])):
                    raise VolatileError("trailing bytes after a dump entry's CBOR value")
                tree, found = _mask(tree, rule.pointer, rule.rule, rule.id)
                item["value_cbor_b64"] = tree
                if found and rule.id not in used:
                    used.append(rule.id)
        out.append(item)
    return out


def dump_view(envelope: dict[str, Any]) -> tuple[dict[str, Any], list[str]]:
    """An envelope without its volatile parts: revisions and digest dropped, the live entry
    and the adapter body masked. Used for barrier stability and the dump sandwich."""
    used: list[str] = []
    view = {key: value for key, value in envelope.items()
            if key not in MONOTONIC_KEYS and key != "digest"}
    view["entries"] = _entries_view(envelope.get("entries", []), used)
    http = dict(envelope.get("http") or {})
    if isinstance(http.get("body_utf8"), str):
        http["body_utf8"], body_used = payload_view(
            SCHEMA_ROUTE.get(envelope.get("schema"), ""), http["body_utf8"].encode("utf-8"))
        used += [i for i in body_used if i not in used]
    view["http"] = http
    return view, used


def monotonic_difference(before: dict[str, Any], after: dict[str, Any], key: str) -> bool:
    """True when a revision counter went backwards or its namespaces changed."""
    left, right = before.get(key), after.get(key)
    if key == "namespace_revisions":
        if not isinstance(left, list) or not isinstance(right, list) or len(left) != len(right):
            return left != right
        return any(a[0] != b[0] or b[1] < a[1] for a, b in zip(left, right))
    if isinstance(left, int) and isinstance(right, int):
        return right < left
    return left != right


def ledger() -> list[dict[str, str]]:
    """The ledger as reviewable records (written into the transcript)."""
    rows = [{"id": f.id, "scope": f"{','.join(f.paths)} {f.pointer}", "rule": f.rule,
             "reason": f.reason, "authority": AUTHORITY} for f in FIELDS]
    rows += [{"id": e.id, "scope": f"dump entry {e.ns}{e.entry} {e.pointer}", "rule": e.rule,
              "reason": e.reason, "authority": AUTHORITY} for e in ENTRIES]
    rows.append({"id": "dump-revisions", "scope": "dump " + ", ".join(MONOTONIC_KEYS),
                 "rule": "non-decreasing", "reason": "advanced by the volatile entry publication",
                 "authority": AUTHORITY})
    return rows
