"""The vosu route check of a state-route sandwich, and the ledger rows that decide it.

GOLDEN_CONTRACT.md §6 step 3: the adapter dump records the adapter's `GET` response, so
a state route's `GET` must match the dump's status and JSON payload, but only when the
request reaches the adapter. A divergence-ledger row can decide the route outcome before
it does; then the case is checked against that row's rule instead of the dump. Every
other case, including a ledger case whose request stays within the row's limits, is
checked strictly.

`adapter-surface-request-budget` ([DIVERGENCES.md](../../tests/fixtures/upstream/
DIVERGENCES.md)): a request with more than `MAX_QUERY_PAIRS` decoded query pairs, more
than `MAX_HEADERS` forwarded headers, more than `MAX_METADATA_BYTES` of decoded query
plus forwarded-header metadata, or a forwarded header value that is not visible ASCII or
tab never reaches a provider and gets the ordinary `Not Found` 404. The limits and the
forwarded header set mirror `crates/vosu-surface/src/routes.rs`
(`SurfaceRequest::valid_metadata`) and `src/server/service/surface.rs`
(`SurfaceRouter::dispatch`).
"""
from __future__ import annotations

import json
from typing import Any, Callable, NamedTuple
from urllib.parse import parse_qsl

import parity_volatile as volatile
from parity_golden_rules import GoldenError
import route_matrix as rm

REQUEST_BUDGET = "adapter-surface-request-budget"
MAX_QUERY_PAIRS = 128
MAX_HEADERS = 32
MAX_METADATA_BYTES = 16 * 1024
FORWARDED_HEADERS = ("range", "if-none-match", "if-modified-since", "if-range")


class RouteRule(NamedTuple):
    """A ledger row that decides the vosu route outcome, and why it applies."""
    ledger_id: str
    status: int
    reason: str


def request_budget_excess(request: str) -> str | None:
    """Why `request` exceeds the adapter surface request budget, or None within it."""
    head = request.partition("\r\n\r\n")[0].split("\r\n")
    parts = head[0].split(" ")
    target = parts[1] if len(parts) == 3 else ""
    query = target.partition("?")[2]
    pairs = parse_qsl(query, keep_blank_values=True, encoding="utf-8", errors="replace")
    headers = []
    for line in head[1:]:
        name, separator, value = line.partition(":")
        if separator and name.lower() in FORWARDED_HEADERS:
            headers.append((name.lower(), value.strip(" \t")))
    if len(pairs) > MAX_QUERY_PAIRS:
        return f"{len(pairs)} query pairs exceed MAX_QUERY_PAIRS {MAX_QUERY_PAIRS}"
    if len(headers) > MAX_HEADERS:
        return f"{len(headers)} forwarded headers exceed MAX_HEADERS {MAX_HEADERS}"
    metadata = sum(len(key.encode("utf-8")) + len(value.encode("utf-8"))
                   for key, value in (*pairs, *headers))
    if metadata > MAX_METADATA_BYTES:
        return f"{metadata} metadata bytes exceed MAX_METADATA_BYTES {MAX_METADATA_BYTES}"
    for name, value in headers:
        if any(not (ch == "\t" or 32 <= ord(ch) <= 126) for ch in value):
            return f"forwarded {name} value carries a control or non-ASCII byte"
    return None


def route_rule(case_id: str, request: str) -> RouteRule | None:
    """The ledger row deciding this case's vosu route, or None (the dump decides it)."""
    case = rm.CASES.get(case_id)
    if case is None or REQUEST_BUDGET not in case.ledger:
        return None
    excess = request_budget_excess(request)
    return None if excess is None else RouteRule(REQUEST_BUDGET, 404, excess)


def check_state_route(case_id: str, path: str, route: dict[str, Any],
                      dump: dict[str, Any]) -> None:
    """A state route's vosu `GET` record against its ledger rule, else against the dump."""
    rule = route_rule(case_id, route["request"])
    if rule is not None:
        if route["status"] != rule.status:
            raise GoldenError(f"case {case_id}: ledger {rule.ledger_id} ({rule.reason}): vosu "
                              f"must answer {rule.status}, not {route['status']}")
        return
    if route["status"] != dump["http"]["status"]:
        raise GoldenError(f"case {case_id}: vosu route status differs from the dump's adapter status")
    try:
        route_body, _ = volatile.payload_view(path, route["body"])
        dump_body, _ = volatile.payload_view(path, dump["http"]["body_utf8"].encode("utf-8"))
    except volatile.VolatileError as exc:
        raise GoldenError(f"case {case_id}: {exc}") from exc
    if route_body != dump_body:
        raise GoldenError(f"case {case_id}: vosu route JSON differs from the dump's adapter payload")


ResponseSizes = dict[tuple[str, str, int], tuple[int, int]]


def get_response_sizes(cases: list[dict[str, Any]],
                       normalized_size: Callable[[dict, bytes], int]) -> list[ResponseSizes]:
    """Key each sandwich side's GET sizes by barrier, exact request and status."""
    sizes: list[ResponseSizes] = [{}, {}, {}]
    for case in cases:
        if case["method"] != "GET":
            continue
        for index, record in enumerate([*case["source"], case["route"]]):
            if record["outcome"] is not None or record["status"] == 304:
                continue
            body = record["body"]
            sizes[index].setdefault((case["barrier"], record["request"], record["status"]), (
                len(body), normalized_size(case, body) if index < 2 else len(body)))
    return sizes


def _matching_get(case: dict[str, Any], record: dict[str, Any],
                  sizes: ResponseSizes) -> tuple[int, int] | None:
    if record["status"] == 304 or not record["request"].startswith("HEAD "):
        return None
    return sizes.get((case["barrier"], "GET " + record["request"][len("HEAD "):], record["status"]))


def check_route_length(case: dict[str, Any], sizes: ResponseSizes | None = None) -> None:
    route = case["route"]
    head = route["request"].startswith("HEAD ")
    matched = _matching_get(case, route, sizes) if sizes is not None else None
    size = matched[0] if matched else volatile.body_length(route["status"], len(route["body"]),
                                                           head=head)
    for name, value in route["headers"]:
        if name.lower() == "content-length":
            _check_length(case["id"], "vosu route", value, size)


def normalize_lengths(headers: list[list[str]], case: dict, record: dict, length: int,
                      sizes: ResponseSizes) -> None:
    raw = volatile.body_length(record["status"], len(record["body"]), head=case["method"] == "HEAD")
    lengths = (raw, length) if raw is not None else _matching_get(case, record, sizes)
    for item in headers:
        if item[0] == "content-length":
            _check_length(case["id"], "upstream", item[1], lengths[0] if lengths else None)
            if lengths and lengths[0] != lengths[1]:
                item[1] = str(lengths[1])
        elif item[0] == "content-range" and lengths and lengths[0] != lengths[1]:
            raise GoldenError(f"case {case['id']}: root-prefix changed a partial body")


def _check_length(case_id: str, side: str, value: str, size: int | None) -> None:
    try:
        volatile.check_content_length(value, size)
    except volatile.VolatileError as exc:
        raise GoldenError(f"case {case_id}: {side} {exc}") from exc


def calculation_outside_rule(files: dict[str, bytes]) -> dict[str, Any] | None:
    """§10: keep the evidenced Windows lazer success; replay exactly vosu's rejection."""
    platform = "windows-lazer"
    case = "calculation.absolute-outside-root"
    data = files.get(f"{platform}/cases/{case}.json")
    if data is None:
        return None
    record = json.loads(data)
    if not record["request"]["text"].startswith("GET /api/calculate/pp?path=") \
            or record["expected"].get("status") != 200:
        return None
    error = '{"error":"invalid beatmap path: outside declared roots or unsafe spelling"}'
    parts = [("status", 400), ("body", error)]
    # Content-Length describes each side's own bytes, including for a body rule.
    return {"format": "vosu-golden-divergence-rules-v1", "rules": [
        {"ledger_id": "legacy-calculate-declared-roots", "platforms": [platform],
         "cases": [case], "part": part, "vosu_value": value} for part, value in parts]}
