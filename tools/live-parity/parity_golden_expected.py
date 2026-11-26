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

Both upstream sandwich records carry the declared outcome, vosu must answer, and its dumps
must agree. `golden` treats the case as a documented divergence: with no upstream answer
to replay it writes no case file and lists the case, its outcome, ledger ID and vosu's
status in its summary. Every other case with either outcome still fails.
"""
from __future__ import annotations

import copy
from typing import Any

import parity_golden
import parity_golden_crash as crash
from parity_golden_rules import TRANSPORT_OUTCOMES, GoldenError
import route_matrix as rm

SUMMARY_KEY = "upstream_declared"


def transport_outcomes(record: dict[str, Any]) -> tuple[str, ...]:
    """The transport outcomes a capture record may carry: a declared one only from upstream."""
    expected = record.get("daemon") == "source" and rm.upstream_expected(record["case"])
    return TRANSPORT_OUTCOMES + ((expected[0],) if expected else ())


def record_outcomes(case_id: str) -> tuple[str, ...]:
    """The transport outcomes `golden` accepts in an upstream record of `case_id`."""
    expected = rm.upstream_expected(case_id)
    return TRANSPORT_OUTCOMES + ((expected[0],) if expected else ())


def applies(case: dict[str, Any]) -> bool:
    """A parsed complete case in which upstream recorded a declared outcome."""
    return any(record["outcome"] not in (None, *TRANSPORT_OUTCOMES)
               for record in case.get("source", ()))


def check(case: dict[str, Any]) -> None:
    """The checks `golden` applies to a parsed complete case."""
    if applies(case):
        check_case(case)
    else:
        parity_golden._check_sandwiches(case)


def _without_validator(request: str, header: str) -> str:
    head, sep, body = request.partition("\r\n\r\n")
    lines = [line for line in head.split("\r\n")
             if line.partition(":")[0].strip().lower() != header.lower()]
    return "\r\n".join(lines) + sep + body


def check_case(case: dict[str, Any]) -> dict[str, Any]:
    """Upstream recorded its declared outcome and vosu answered: the documented divergence."""
    case_id, route = case["id"], case["route"]
    expected = rm.upstream_expected(case_id)
    if expected is None:
        raise GoldenError(f"case {case_id}: no declared upstream outcome in the route matrix")
    outcome, ledger_id = expected
    if crash._ledger_rows().get(ledger_id) not in ("security", "contract", "licensing"):
        raise GoldenError(f"case {case_id}: ledger {ledger_id} is not a non-temporary "
                          "DIVERGENCES.md row")
    if any(record["outcome"] != outcome for record in case["source"]):
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
    if route["status"] is None:
        raise GoldenError(f"case {case_id}: vosu must answer where upstream {outcome}")
    return {"id": case_id, "outcome": outcome, "ledger_id": ledger_id,
            "attempts": case["attempts"], "vosu_status": route["status"]}
