"""Declared upstream outcomes: golden cases the pinned upstream cannot answer in full.

GOLDEN_CONTRACT.md §6 is normative (Main's decisions of 2026-11-26, after the first and
second full pilot runs failed at the menu barrier). For each case `rm.UPSTREAM_EXPECTED`
names, by ID and under its reviewed ledger row, upstream's record is a complete
observation of what upstream lacks:

- `timeout`: no byte before the case deadline. Its `upgrade` handler leaves an Upgrade to
  a path it does not own unanswered, and its HTTP router throws on a Host with a
  non-numeric port before responding. vosu must not copy the hang.
- `validator-missing`: a 2xx priming response with no `ETag` or `Last-Modified`; the
  pinned upstream never sends either. vosu must answer its own conditional request.
- `private-path-echo`: an ordinary 5xx response whose body echoes a path under the
  attested private capture root (upstream joins a Windows absolute spelling under its
  songs folder). Checked structurally here; `golden` never stores those bytes. vosu must
  refuse cleanly: a 3xx/4xx answer (never 2xx serving the sentinel, never 5xx) whose
  status line, headers and body carry no absolute path and no capture root.

Both upstream sandwich records carry the declared outcome, vosu must answer, and its dumps
must agree. `golden` treats the case as a documented divergence: it writes no case file
and lists the case, its outcome, ledger ID and vosu's status in its summary. Every other
case with one of these outcomes still fails, and any other upstream outcome fails the case.
"""
from __future__ import annotations

import copy
from typing import Any
from urllib.parse import unquote

import parity_golden
import parity_golden_crash as crash
from parity_golden_rules import (TRANSPORT_OUTCOMES, GoldenError, Root, RootRewriter,
                                 privacy_findings, synthetic_root)
import route_matrix as rm

SUMMARY_KEY = "upstream_declared"
PATH_ECHO = "private-path-echo"


def _transport(expected: tuple[str, str] | None) -> tuple[str, ...]:
    raw = expected and rm.RAW_OUTCOME[expected[0]]
    return TRANSPORT_OUTCOMES + ((raw,) if raw and raw != "response" else ())


def transport_outcomes(record: dict[str, Any]) -> tuple[str, ...]:
    """The transport outcomes a capture record may carry: a declared one only from upstream."""
    return _transport(record.get("daemon") == "source" and rm.upstream_expected(record["case"])
                      or None)


def record_outcomes(case_id: str) -> tuple[str, ...]:
    """The transport outcomes `golden` accepts in an upstream record of `case_id`."""
    return _transport(rm.upstream_expected(case_id))


def applies(case: dict[str, Any]) -> bool:
    """A parsed complete case the route matrix declares an upstream outcome for."""
    return rm.upstream_expected(case["id"]) is not None


def rewriter_for(golden_cfg: dict[str, Any], os_name: str) -> RootRewriter:
    """The attested capture root as golden-capture records it (`capture.roots`)."""
    windows = os_name.lower() == "windows"
    separator = "\\" if windows else "/"
    slot = golden_cfg["capture_slot"]
    private = golden_cfg["capture_root"].rstrip(separator) + separator
    return RootRewriter([Root(slot, private, synthetic_root(slot, windows))], windows)


def check(case: dict[str, Any], rewriter: RootRewriter) -> None:
    """The checks `golden` applies to a parsed complete case."""
    if applies(case):
        check_case(case, rewriter)
    else:
        parity_golden._check_sandwiches(case)


def _text(data: bytes) -> str:
    return data.decode("utf-8", "replace")


def _check_path_echo(case_id: str, source: list[dict[str, Any]], route: dict[str, Any],
                     rewriter: RootRewriter) -> None:
    for record in source:
        status = record["status"]
        if status is None or not 500 <= status < 600:
            raise GoldenError(f"case {case_id}: upstream {PATH_ECHO} needs a 5xx response, "
                              f"not {record['outcome'] or status}")
        body = _text(record["body"])
        if not any(rewriter.leftover_slots(view) for view in (body, unquote(body))):
            raise GoldenError(f"case {case_id}: upstream {PATH_ECHO} without an echo of the "
                              f"private root (status {status})")
    status = route["status"]
    if status is None or not 300 <= status < 500:
        raise GoldenError(f"case {case_id}: vosu must refuse a {PATH_ECHO} case with 3xx/4xx, "
                          f"not {route['outcome'] or status}")
    parts = [route.get("status_line", ""), _text(route["body"]),
             *(f"{name}: {value}" for name, value in route["headers"])]
    for part in parts:
        if privacy_findings(part, rewriter):
            raise GoldenError(f"case {case_id}: vosu echoes a private or absolute path "
                              f"({privacy_findings(part, rewriter)[0]})")


def _without_validator(request: str, header: str) -> str:
    head, sep, body = request.partition("\r\n\r\n")
    lines = [line for line in head.split("\r\n")
             if line.partition(":")[0].strip().lower() != header.lower()]
    return "\r\n".join(lines) + sep + body


def check_case(case: dict[str, Any], rewriter: RootRewriter) -> dict[str, Any]:
    """Upstream recorded its declared outcome and vosu answered: the documented divergence."""
    case_id, route = case["id"], case["route"]
    expected = rm.upstream_expected(case_id)
    if expected is None:
        raise GoldenError(f"case {case_id}: no declared upstream outcome in the route matrix")
    outcome, ledger_id = expected
    if crash._ledger_rows().get(ledger_id) not in ("security", "contract", "licensing"):
        raise GoldenError(f"case {case_id}: ledger {ledger_id} is not a non-temporary "
                          "DIVERGENCES.md row")
    raw = None if outcome == PATH_ECHO else outcome  # A parsed response has outcome None.
    if any(record["outcome"] != raw for record in case["source"]):
        raise GoldenError(f"case {case_id}: both upstream sandwich records must be {outcome}")
    validator = rm.CASES[case_id].validator
    if (case["conditional"] is None) != (validator is None):
        raise GoldenError(f"case {case_id}: conditional does not match the route matrix")
    checked = case
    if validator is not None:
        # Upstream was sent only the priming request (no validator to send); vosu's
        # conditional request must be exactly it plus the validator line.
        checked = copy.deepcopy(case)
        checked["conditional"] = None
        checked["route"]["request"] = _without_validator(route["request"],
                                                         validator.request_header)
    parity_golden._check_sandwiches(checked)  # Requests, headers and the vosu dump sandwich.
    if outcome == PATH_ECHO:
        _check_path_echo(case_id, case["source"], route, rewriter)
    elif route["status"] is None:
        raise GoldenError(f"case {case_id}: vosu must answer where upstream {outcome}")
    return {"id": case_id, "outcome": outcome, "ledger_id": ledger_id,
            "attempts": case["attempts"], "vosu_status": route["status"]}
