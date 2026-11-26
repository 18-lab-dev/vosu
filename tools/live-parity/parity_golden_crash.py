"""`upstream_crashed`: golden cases whose request kills the pinned upstream daemon.

GOLDEN_CONTRACT.md §6 is normative (Main's contract decision and the owner amendment of
2026-11-25, after the first full pilot aborted). The pinned upstream logs every HTTP
response from its `finish` handler through `decodeURIComponent(req.url)`; a request
target with a malformed percent escape makes that call throw after the response was
written, and the uncaught `URIError` ends the process. The route matrix names these
cases through the reviewed ledger row `rm.UPSTREAM_CRASH_LEDGER`.

`golden-capture` runs them last, after the no-client barrier and its cases, so they can
never abort the barrier sequence. Each attempt waits for a re-attested upstream (the
crashed one is replaced by a fresh launch) to report no-client, sends the request once,
records every byte upstream sent before it died and its exit code, then runs the usual
vosu dump -> route -> dump sandwich: vosu must answer and keep running. `golden` checks
the record and the ledger row and treats the case as a documented divergence: it writes
no case file (there is no upstream answer to replay) and lists it in its summary.

Capture shape of the final attempt (failed attempts are as for every case):

    {"attempt": n, "outcome": "upstream_crashed", "ledger_id": str,
     "source": {"attempt": n, "request": bytes, "received": bytes, "exit_code": int},
     "vosu": {"dump_before": dump, "route": record, "dump_after": dump}}
"""
from __future__ import annotations

import copy
import functools
import re
import socket
import subprocess
import sys
import time
from typing import TYPE_CHECKING, Any

import parity_golden
from parity_golden_rules import GoldenError, dump_sandwich_difference
import parity_probes
import parity_routes as pr
import route_matrix as rm

if TYPE_CHECKING:  # parity_golden_capture imports this module; at run time it is loaded.
    import parity_golden_capture as gc

OUTCOME = "upstream_crashed"
# Upstream answers and exits at once; a daemon still alive after this never crashed.
EXIT_TIMEOUT_S = 10.0
MAX_RECEIVED_BYTES = 1 << 20


# ---------------------------------------------------------------- golden-capture


def upstream_exchange(item: rm.RenderedCase, source: gc.Daemon) -> dict[str, Any]:
    """Send the case once to the attested upstream and keep every byte until it closes."""
    case_id = item.case.id
    started = time.monotonic_ns()
    parity_probes._assert_one_owner(source.evidence, f"route/{case_id}/crash/before", started)
    received, error = bytearray(), None
    sock = socket.create_connection((rm.HOST, source.port), timeout=pr.CASE_DEADLINE_S)
    try:
        parity_probes._assert_connection_owner(
            source.evidence, sock, f"route/{case_id}/crash/connection", started)
        sock.sendall(item.request)
        deadline = time.monotonic() + EXIT_TIMEOUT_S
        while len(received) <= MAX_RECEIVED_BYTES:
            sock.settimeout(max(0.05, deadline - time.monotonic()))
            chunk = sock.recv(65536)
            if not chunk:
                break
            received += chunk
    except OSError as exc:  # A reset or a timeout still keeps the bytes already received.
        error = f"{type(exc).__name__}: {exc}"
    finally:
        sock.close()
    return {"request": pr.encode_bytes(item.request), "received": pr.encode_bytes(bytes(received)),
            "error": error, "elapsed_ns": time.monotonic_ns() - started}


def _exit_code(process: subprocess.Popen[bytes]) -> int | None:
    try:
        return process.wait(timeout=EXIT_TIMEOUT_S)
    except subprocess.TimeoutExpired:
        return None


def _settle(session: gc.Session, source: gc.Daemon, dump: gc.HarnessDump) -> dict[str, Any]:
    """Both daemons report no-client across the stable observations (a fresh upstream
    needs a moment to read the client population)."""
    import parity_golden_capture as gc
    stable, seen = 0, {}
    while stable < gc.STABLE_OBSERVATIONS:
        if time.monotonic() > session.deadline:
            raise gc.CaptureError("upstream did not report no-client before the timeout")
        observation = session._observe(source, dump)
        seen = observation.seen
        stable = stable + 1 if session._holds("no-client", observation) else 0
        if stable < gc.STABLE_OBSERVATIONS:
            time.sleep(gc.POLL_S)
    return seen


def _attempt(session: gc.Session, item: rm.RenderedCase, number: int,
             failed: list[dict[str, Any]], source: gc.Daemon, vosu: gc.Daemon,
             dump: gc.HarnessDump) -> dict[str, Any] | str:
    import parity_golden_capture as gc
    case, ledger_id = item.case, rm.upstream_crash_ledger(item.case)
    schema = gc.dump_schema(case)
    raw: dict[str, Any] = {"case": case.id, "attempt": number, "outcome": OUTCOME,
                           "ledger_id": ledger_id}
    session.transcript["cases"].append(raw)
    raw["barrier_before"] = _settle(session, source, dump)
    session.ledger.admit(item)
    upstream = upstream_exchange(item, source)
    exit_code = _exit_code(session.processes["source"])
    raw.update(source=upstream, source_exit_code=exit_code)
    before = dump.fetch(schema)
    route = pr.run_identical(item, [vosu.endpoint], ledger=session.ledger)[0]
    after = dump.fetch(schema)
    raw.update(vosu=route, dump_before=before, dump_after=after)
    if exit_code is None:
        return f"upstream survived a request the ledger {ledger_id} records as crashing it"
    if route["outcome"] != "response":
        return f"vosu outcome {route['outcome']!r}: vosu must answer where upstream crashed"
    attempt = {
        "attempt": number, "outcome": OUTCOME, "ledger_id": ledger_id,
        "source": {"attempt": number, "request": gc._g0_bytes(item.request),
                   "received": gc._g0_bytes(pr.decode_bytes(upstream["received"])),
                   "exit_code": exit_code},
        "vosu": {"dump_before": {"attempt": number, "envelope": before},
                 "route": gc.golden_record(route, number, case.family),
                 "dump_after": {"attempt": number, "envelope": after}}}
    value = {"id": case.id, "family": case.family, "barrier": case.barrier,
             "dump_schema": schema, "conditional": None, "attempts": [*failed, attempt]}
    try:  # The checks `golden` applies, so a recorded case is accepted unchanged.
        check_case(parity_golden._case(copy.deepcopy(value), 0))
    except GoldenError as exc:
        return str(exc)
    return value


def run_cases(session: gc.Session, items: list[rm.RenderedCase], source: gc.Daemon,
              vosu: gc.Daemon, dump: gc.HarnessDump) -> list[dict[str, Any]]:
    """Every upstream-crashing case, after the no-client barrier; upstream is relaunched
    and re-attested before each attempt that finds it exited."""
    import parity_golden_capture as gc
    cases = []
    for item in items:
        failed: list[dict[str, Any]] = []
        for number in range(1, gc.MAX_ATTEMPTS + 1):
            if session.processes["source"].poll() is not None:
                source = session.relaunch_source()
            result = _attempt(session, item, number, failed, source, vosu, dump)
            if isinstance(result, dict):
                cases.append(result)
                break
            failed.append({"attempt": number, "outcome": "failed", "reason": result})
            print(f"golden-capture: {item.case.id} attempt {number} failed: {result}",
                  file=sys.stderr, flush=True)
        else:
            raise gc.CaptureError(f"case {item.case.id} failed {gc.MAX_ATTEMPTS} whole-case "
                                  "attempts: " + "; ".join(entry["reason"] for entry in failed))
    return cases


# ---------------------------------------------------------------- golden


@functools.cache
def _ledger_rows() -> dict[str, str]:
    text = (parity_golden.REPO_ROOT / "tests/fixtures/upstream/DIVERGENCES.md").read_text("utf-8")
    return dict(re.findall(r"^\| ([a-zA-Z0-9.-]+) \| (security|contract|licensing|temporary) \|",
                           text, re.M))


def parse_attempt(value: dict[str, Any], at: str, case_id: str, family: str,
                  number: int) -> dict[str, Any]:
    """The strict shape of a final `upstream_crashed` attempt (see the module docstring)."""
    g = parity_golden
    g._obj(value, at, ("attempt", "outcome", "ledger_id", "source", "vosu"))
    ledger_id = g._str(value["ledger_id"], f"{at}.ledger_id")
    case = rm.CASES.get(case_id)
    if case is None or rm.upstream_crash_ledger(case) != ledger_id:
        raise GoldenError(f"case {case_id}: not an upstream_crashed case of the route matrix "
                          f"under ledger {ledger_id}")
    if _ledger_rows().get(ledger_id) not in ("security", "contract", "licensing"):
        raise GoldenError(f"case {case_id}: ledger {ledger_id} is not a non-temporary "
                          "DIVERGENCES.md row")
    if family == "ws" or case.validator is not None:
        raise GoldenError(f"case {case_id}: an upstream_crashed case is one plain HTTP request")
    source = g._obj(value["source"], f"{at}.source", ("attempt", "request", "received", "exit_code"))
    if g._int(source["attempt"], f"{at}.source.attempt") != number:
        raise GoldenError(f"{at}.source: record belongs to another attempt (attempts are never mixed)")
    request = g._utf8(g._bytes(source["request"], f"{at}.source.request"))
    if request is None:
        raise GoldenError(f"{at}.source.request: binary request bytes")
    exit_code = source["exit_code"]
    if isinstance(exit_code, bool) or not isinstance(exit_code, int) or exit_code == 0 \
            or not -(1 << 31) <= exit_code < 1 << 32:
        raise GoldenError(f"{at}.source.exit_code: upstream must have exited with a failure code")
    vosu = g._obj(value["vosu"], f"{at}.vosu", ("dump_before", "route", "dump_after"))
    dumps = []
    for key in ("dump_before", "dump_after"):
        dump = g._obj(vosu[key], f"{at}.vosu.{key}", ("attempt", "envelope"))
        if g._int(dump["attempt"], f"{at}.vosu.{key}.attempt") != number:
            raise GoldenError(f"case {case_id}: {key} belongs to another attempt "
                              "(attempts are never mixed)")
        dumps.append(g._envelope(dump["envelope"], f"case {case_id}.{key}"))
    return {"ledger_id": ledger_id, "request": request, "exit_code": exit_code,
            "received": g._bytes(source["received"], f"{at}.source.received"),
            "route": g._record(vosu["route"], f"{at}.vosu.route", number, False), "dumps": dumps}


def check_case(case: dict[str, Any]) -> dict[str, Any]:
    """Upstream crashed and vosu answered: the documented divergence `golden` reports."""
    case_id, crashed = case["id"], case[OUTCOME]
    route = crashed["route"]
    for request, part in ((crashed["request"], "upstream"), (route["request"], "vosu route")):
        parity_golden._check_target(case_id, request)
        parity_golden._check_headers(case_id, parity_golden._request_headers(request),
                                     f"{part} request")
    parity_golden._check_headers(case_id, route["headers"], "vosu route response")
    if route["request"] != crashed["request"]:
        raise GoldenError(f"case {case_id}: vosu and upstream request bytes differ")
    if route["status"] is None:
        raise GoldenError(f"case {case_id}: vosu must answer where upstream crashed")
    before, after = crashed["dumps"]
    differs = dump_sandwich_difference(before, after)
    if differs is not None:
        raise GoldenError(f"case {case_id}: vosu dump sandwich differs in {differs}")
    if before["schema"] != case["dump_schema"]:
        raise GoldenError(f"case {case_id}: dump schema differs from the case's dump_schema")
    return {"id": case_id, "ledger_id": crashed["ledger_id"], "attempts": case["attempts"],
            "upstream_exit_code": crashed["exit_code"],
            "upstream_received_bytes": len(crashed["received"]), "vosu_status": route["status"]}
