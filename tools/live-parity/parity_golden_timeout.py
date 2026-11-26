"""Upstream `timeout`: golden cases whose request the pinned upstream never answers.

GOLDEN_CONTRACT.md §6 is normative (Main's decision of 2026-11-26, after the first full
pilot run failed at the menu barrier). For each case `rm.UPSTREAM_TIMEOUT` names, by ID
and under its reviewed ledger row, upstream sends no byte before the case deadline: its
`upgrade` handler leaves an Upgrade to a path it does not own unanswered, and its HTTP
router throws on a Host with a non-numeric port before responding. That no-response is a
complete observation: both upstream sandwich records are `{"outcome": "timeout"}`. vosu
must not copy the hang; it must answer, and its dumps must agree. `golden` treats the case
as a documented divergence: with no upstream answer to replay it writes no case file and
lists the case, its ledger ID and vosu's status in its summary. Every other case that
times out still fails.
"""
from __future__ import annotations

from typing import Any

import parity_golden
import parity_golden_crash as crash
from parity_golden_rules import TRANSPORT_OUTCOMES, GoldenError
import route_matrix as rm

OUTCOME = "timeout"
SUMMARY_KEY = "upstream_timeout"


def transport_outcomes(record: dict[str, Any]) -> tuple[str, ...]:
    """The transport outcomes a capture record may carry: `timeout` only from upstream on a
    declared case."""
    declared = record.get("daemon") == "source" and rm.upstream_timeout_ledger(record["case"])
    return TRANSPORT_OUTCOMES + ((OUTCOME,) if declared else ())


def record_outcomes(case_id: str) -> tuple[str, ...]:
    """The transport outcomes `golden` accepts in an upstream record of `case_id`."""
    return TRANSPORT_OUTCOMES + ((OUTCOME,) if rm.upstream_timeout_ledger(case_id) else ())


def applies(case: dict[str, Any]) -> bool:
    """A parsed complete case in which upstream sent nothing."""
    return any(record["outcome"] == OUTCOME for record in case.get("source", ()))


def check(case: dict[str, Any]) -> None:
    """The checks `golden` applies to a parsed complete case."""
    if applies(case):
        check_case(case)
    else:
        parity_golden._check_sandwiches(case)


def check_case(case: dict[str, Any]) -> dict[str, Any]:
    """Upstream never answered and vosu did: the documented divergence `golden` reports."""
    case_id, route = case["id"], case["route"]
    ledger_id = rm.upstream_timeout_ledger(case_id)
    if ledger_id is None:
        raise GoldenError(f"case {case_id}: not an upstream-timeout case of the route matrix")
    if crash._ledger_rows().get(ledger_id) not in ("security", "contract", "licensing"):
        raise GoldenError(f"case {case_id}: ledger {ledger_id} is not a non-temporary "
                          "DIVERGENCES.md row")
    if any(record["outcome"] != OUTCOME for record in case["source"]):
        raise GoldenError(f"case {case_id}: both upstream sandwich records must time out")
    if case["conditional"] is not None:
        raise GoldenError(f"case {case_id}: an upstream-timeout case declares no validator")
    parity_golden._check_sandwiches(case)  # Requests, headers and the vosu dump sandwich.
    if route["status"] is None:
        raise GoldenError(f"case {case_id}: vosu must answer where upstream sent nothing")
    return {"id": case_id, "ledger_id": ledger_id, "attempts": case["attempts"],
            "vosu_status": route["status"]}
