"""`golden-rehearsal`: the mandatory client-less full-matrix rehearsal before a pilot.

Owner amendment 2026-11-25 ("upto3", after the second full pilot aborted on a tooling
defect): a golden-capture pilot may start only after a rehearsal passed on the exact
vosu commit and tooling it runs. The rehearsal needs no capture authorization. On
Linux, with no game client, it builds and attests both daemons exactly as golden-capture
does (`build_harness_artifacts`), launches them, waits for the `no-client` barrier
with the stable-observation rule, then sends every route-matrix case through every
check a capture applies:

- golden cases: the whole-case sandwich with its retries, request ledger, barrier
  guards (both daemons must hold `no-client` before and after every attempt), volatile
  rules and `golden`'s own case validation, including the route-vs-dump and ledger
  rules (parity_golden_ledger);
- `upstream_crashed` cases, last, with the upstream relaunch and re-attestation;
- negative and diagnostic cases, recorded as a capture records them;
- finally `golden`'s whole-capture normalization, privacy scan and budget over the
  natively rendered golden cases.

A case the native linux-lazer plan excludes (Windows spellings, the stable skin
routes) is rendered for windows-lazer or windows-stable from a lexical layout and
sent anyway: on the Linux daemons it can only miss, which still exercises its checks.
Every barrier other than `no-client` needs a client and is not rehearsed: those cases
run at `no-client`, where their guards require both daemons to report it.

The rehearsal reports every failing case instead of stopping at the first. It writes
a private receipt (`--out`) and `<out>.transcript.json` with the raw records; the
receipt binds the vosu commit and `tooling_digest()`, and golden-capture refuses to
run without a passing receipt for its authorized commit and its own tooling digest.
"""
from __future__ import annotations

import copy
import dataclasses
import datetime
import hashlib
import json
from pathlib import Path
import sys
import time
from typing import Any, Callable, Iterable

from live_parity import PINNED_SOURCE_REVISION
import parity_golden
import parity_golden_capture as gc
import parity_golden_crash as crash
from parity_golden_rules import GoldenError
import parity_routes as pr
import route_matrix as rm
import route_security as rs

RECEIPT_FORMAT = "vosu-golden-rehearsal-receipt-v1"
SCENARIO = gc.ScenarioSet("golden-rehearsal-v1", ("no-client",))
NATIVE_PLATFORM = "linux-lazer"
FOREIGN_PLATFORMS = ("windows-lazer", "windows-stable")
FOREIGN_ROOT = "C:\\vosu-rehearsal"
# Executed tooling and the normative documents its checks read.
TOOLING_DOCUMENTS = ("tests/fixtures/upstream/GOLDEN_CONTRACT.md",
                     "tests/fixtures/upstream/DIVERGENCES.md")
# `golden`'s whole-capture check needs a client attestation; none ran.
NO_CLIENT = {"version": "no-client", "sha256": "0" * 64}


def tooling_digest(root: Path = parity_golden.REPO_ROOT) -> str:
    """SHA-256 over the live-parity modules (tests excluded) and the golden documents."""
    tools = sorted(path for path in (root / "tools" / "live-parity").glob("*.py")
                   if not path.name.startswith("test_"))
    digest = hashlib.sha256()
    for path in [*tools, *(root / name for name in TOOLING_DOCUMENTS)]:
        data = path.read_bytes()
        name = path.relative_to(root).as_posix().encode("utf-8")
        digest.update(len(name).to_bytes(8, "big") + name + len(data).to_bytes(8, "big") + data)
    return "sha256:" + digest.hexdigest()


def check_receipt(path: str | None, cfg: dict[str, Any]) -> dict[str, Any]:
    """The passing receipt golden-capture needs: the authorized vosu commit and this tooling."""
    hint = ("run `run.py golden-rehearsal` on Linux from the same commit first "
            "(verification.md, O0 pilot run)")
    if not path:
        raise gc.CaptureError(f"a pilot needs a passing rehearsal receipt; {hint}")
    try:
        data = Path(path).read_bytes()
        receipt = json.loads(data.decode("utf-8"))
    except (OSError, UnicodeDecodeError, ValueError) as exc:
        raise gc.CaptureError(f"cannot read the rehearsal receipt: {exc}; {hint}") from exc
    if not isinstance(receipt, dict) or receipt.get("format") != RECEIPT_FORMAT:
        raise gc.CaptureError(f"the rehearsal receipt is not a {RECEIPT_FORMAT} file")
    if receipt.get("result") != "pass":
        raise gc.CaptureError(f"the rehearsal did not pass; {hint}")
    commit = cfg["golden"]["authorization"]["vosu_commit"]
    if receipt.get("vosu_commit") != commit:
        raise gc.CaptureError(f"the rehearsal receipt is for vosu {receipt.get('vosu_commit')}, "
                              f"not the authorized {commit}; {hint}")
    digest = tooling_digest()
    if receipt.get("tooling_digest") != digest:
        raise gc.CaptureError(f"the rehearsal receipt is for other tooling than this run's "
                              f"{digest}; {hint}")
    if receipt.get("source_revision") != PINNED_SOURCE_REVISION:
        raise gc.CaptureError("the rehearsal receipt is for another upstream pin")
    return {"sha256": hashlib.sha256(data).hexdigest(), "vosu_commit": commit,
            "tooling_digest": digest, "completed_utc": receipt.get("completed_utc")}


class NoClient:
    """No game runs: the client has always exited."""

    def start(self) -> dict[str, Any]:
        return {"mode": "rehearsal", "client": None}

    def exited(self) -> bool:
        return True

    def stop(self) -> list[str]:
        return []


def _foreign_context(native: rm.MatrixContext, platform: str) -> rm.MatrixContext:
    """A lexical Windows layout mirroring the configured one; it creates no file."""
    assert native.security is not None
    stable = platform == "windows-stable"
    files = FOREIGN_ROOT + ("\\osu\\Songs" if stable else "\\client\\files")
    roots = {"/Songs/": files, "/files/beatmap/": files}
    if stable:
        roots["/files/skin/"] = FOREIGN_ROOT + "\\osu\\Skins"
    probe = f"{FOREIGN_ROOT}\\{rs.SENTINEL_DIR}\\"
    layout = rs.SecurityLayout(
        "windows", FOREIGN_ROOT, native.security.token, roots,
        {prefix: value for prefix, value in native.security.inside_files.items() if prefix in roots},
        probe + rs.SENTINEL_FILE, probe + rs.SENTINEL_CHART, "0" * 64,
        other_roots={} if stable else {"files": files})
    values = dict(native.values)
    if "calc_relative" in values:
        values["calc_absolute_inside"] = files + "\\" + values["calc_relative"].replace("/", "\\")
    return rm.MatrixContext(platform, values, layout)


def plan(native: rm.MatrixContext, cases: Iterable[rm.RouteCase]
         ) -> tuple[list[tuple[str, rm.RenderedCase]], list[list[str]]]:
    """(rendering, case) for every case, natively where possible; unrenderable ones listed."""
    foreign = [_foreign_context(native, platform) for platform in FOREIGN_PLATFORMS]
    planned, unrendered = [], []
    for case in cases:
        reasons = []
        for context in (native, *foreign):
            try:
                item = rm.render(case, context)
            except (rm.CaseUnavailable, rs.SecurityAbort) as exc:
                reasons.append(f"{context.platform}: {exc}")
                continue
            if context is not native:  # Sent to the Linux daemons, where it can only miss.
                item = dataclasses.replace(item, lexical_only=False)
            elif item.lexical_only:
                raise gc.CaptureError(f"{case.id}: a native rendering is lexical only")
            planned.append((context.platform, item))
            break
        else:
            unrendered.append([case.id, "; ".join(reasons)])
    order = {False: 0, True: 1}  # upstream_crashed cases last
    planned.sort(key=lambda entry: order[bool(rm.upstream_crash_ledger(entry[1].case))])
    return planned, unrendered


class Rehearsal(gc.Session):
    """A golden-capture session with no client, at the `no-client` barrier throughout."""

    def __init__(self, cfg: dict[str, Any], out: Path, *, timeout: float,
                 build: Callable[[dict[str, Any], str], dict[str, Any]],
                 cases: Iterable[rm.RouteCase], operator: Callable[[str, str], None]) -> None:
        super().__init__(cfg, SCENARIO, out, timeout=timeout, build=build, client=NoClient(),
                         operator=operator)
        self.cases = tuple(cases)
        self.transcript["rehearsal"] = []

    def _holds(self, barrier: str, seen: dict[str, str | None]) -> bool:
        return all(value == "no-client" for value in seen.values())

    def _client_attestation(self) -> dict[str, Any]:
        return {"variant": self.cfg["client"].lower(), **NO_CLIENT}

    def _case(self, platform: str, item: rm.RenderedCase, source: gc.Daemon, vosu: gc.Daemon,
              dump: gc.HarnessDump) -> tuple[dict[str, Any], Any]:
        case = item.case
        entry: dict[str, Any] = {"case": case.id, "role": case.role, "rendering": platform,
                                 "barrier": case.barrier, "ledger": list(case.ledger)}
        value = None
        try:
            if not rm.golden_eligible(case):
                records = pr.run_identical(item, [source.endpoint, vosu.endpoint], ledger=self.ledger)
                entry["outcomes"] = [record["outcome"] for record in records]
                entry["local_findings"] = [f for record in records
                                           for f in pr.local_expectation_failures(case, record)]
                self.transcript["diagnostics"].append({"case": case.id, "records": records})
            else:
                if rm.upstream_crash_ledger(case):
                    (value,) = crash.run_cases(self, [item], source, vosu, dump)
                else:
                    value = self._run_case(item, source, vosu, dump)
                entry["outcome"] = value["attempts"][-1]["outcome"]
                entry["failed_attempts"] = [a["reason"] for a in value["attempts"][:-1]]
        except (gc.CaptureError, GoldenError, ValueError, OSError) as exc:
            entry["failure"] = f"{type(exc).__name__}: {exc}"
        return entry, value

    def _alive_source(self, source: gc.Daemon, entry: dict[str, Any], dump: gc.HarnessDump
                      ) -> gc.Daemon:
        """Upstream after a case: an unexpected exit fails the case; a fresh one continues."""
        if self.processes["source"].poll() is None:
            return source
        if entry.get("outcome") != crash.OUTCOME:
            entry["failure"] = (entry.get("failure", "") + "; upstream exited, but the case is "
                                f"not under ledger {rm.UPSTREAM_CRASH_LEDGER}").lstrip("; ")
        source = self.relaunch_source()
        crash._settle(self, source, dump)
        return source

    def run(self) -> dict[str, Any]:
        digest = tooling_digest()
        build = self.build(self.cfg, str(self.out))
        self.transcript["build"] = build
        identity = self._identity(build)
        context = self._prepare_layout()
        if context.platform != NATIVE_PLATFORM:
            raise gc.CaptureError(f"a rehearsal runs on {NATIVE_PLATFORM}, not {context.platform}")
        planned, unrendered = plan(context, self.cases)
        source, vosu = self._launch()
        self.transcript["launch"] = {"source": source.evidence, "vosu": vosu.evidence}
        dump = gc.HarnessDump(vosu, self.capability)
        self.transcript["harness_endpoint"] = dump.verify_mounted()
        self.transcript["client"] = self.client.start()
        self._wait_barrier("no-client", source, dump, [], identical_views=True)
        entries, golden_cases = [], []
        for platform, item in planned:
            self.operator("no-client", f"rehearsing {item.case.id} ({platform})")
            entry, value = self._case(platform, item, source, vosu, dump)
            source = self._alive_source(source, entry, dump)
            if self.processes["vosu"].poll() is not None:
                entry["failure"] = (entry.get("failure", "") + "; vosu exited").lstrip("; ")
            entries.append(entry)
            self.transcript["rehearsal"].append(entry)
            if value is not None and platform == NATIVE_PLATFORM:
                golden_cases.append(value)
            if self.processes["vosu"].poll() is not None:
                break
        summary, golden_failure = self._golden(golden_cases, identity)
        if tooling_digest() != digest:
            raise gc.CaptureError("the tooling changed while the rehearsal ran")
        failures = [[entry["case"], entry["failure"]] for entry in entries if "failure" in entry]
        failures += [[case_id, f"not rendered on any platform: {reason}"]
                     for case_id, reason in unrendered]
        if golden_failure:
            failures.append(["golden", golden_failure])
        if len(entries) != len(planned):
            failures.append(["rehearsal", f"stopped after {len(entries)} of {len(planned)} cases"])
        return {
            "format": RECEIPT_FORMAT, "result": "fail" if failures else "pass",
            "vosu_commit": identity["vosu"]["commit"], "vosu_sha256": identity["vosu"]["sha256"],
            "tooling_digest": digest, "source_revision": PINNED_SOURCE_REVISION,
            "source": identity["source"],
            "matrix_sha256": hashlib.sha256(json.dumps(rm.manifest(self.cases), sort_keys=True)
                                            .encode("utf-8")).hexdigest(),
            "cases": len(self.cases), "rehearsed": len(entries),
            "renderings": {name: sum(platform == name for platform, _item in planned)
                           for name in (NATIVE_PLATFORM, *FOREIGN_PLATFORMS)},
            "outcomes": _count(entry.get("outcome", entry["role"]) for entry in entries
                               if "failure" not in entry),
            "retried": [[entry["case"], entry["failed_attempts"]] for entry in entries
                        if entry.get("failed_attempts")],
            "local_findings": [[entry["case"], entry["local_findings"]] for entry in entries
                               if entry.get("local_findings")],
            "upstream_restarts": len(self.transcript.get("upstream_restarts", [])),
            "barriers": self.transcript["barriers"], "golden": summary, "failures": failures,
        }

    def _golden(self, cases: list[dict[str, Any]], identity: dict[str, Any]
                ) -> tuple[dict[str, Any] | None, str | None]:
        """`golden`'s whole-capture validation of the natively rendered golden cases."""
        if not cases:
            return None, "no golden case completed"
        root = self.golden_cfg["capture_root"]
        commit = identity["vosu"]["commit"]
        capture = {
            "format": parity_golden.CAPTURE_FORMAT, "source_revision": PINNED_SOURCE_REVISION,
            "platform": NATIVE_PLATFORM, "scenario_set": SCENARIO.id,
            "authorization": {"reference": "golden-rehearsal", "platform": NATIVE_PLATFORM,
                              "source_revision": PINNED_SOURCE_REVISION, "vosu_commit": commit,
                              "client_version": NO_CLIENT["version"], "scenario_set": SCENARIO.id},
            "attestation": identity,
            "roots": [{"slot": self.golden_cfg["capture_slot"], "private": root.rstrip("/") + "/"}],
            "assets": [], "expected_cases": [case["id"] for case in cases],
            "cases": copy.deepcopy(cases),
        }
        try:
            _platform, files, summary = parity_golden._platform_files(
                json.loads(json.dumps(capture)), None, "captured")
        except GoldenError as exc:
            return None, f"golden would reject the capture: {exc}"
        summary["files"] = len(files)
        return summary, None


def _count(values: Iterable[str]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for value in values:
        counts[value] = counts.get(value, 0) + 1
    return dict(sorted(counts.items()))


def rehearse(cfg: dict[str, Any], out: str, *, timeout: float = 3600.0,
             build: Callable[[dict[str, Any], str], dict[str, Any]] | None = None,
             cases: Iterable[rm.RouteCase] = rm.MATRIX,
             operator: Callable[[str, str], None] = gc.stderr_operator) -> int:
    """Run the rehearsal; write the receipt and transcript; 0 only when it passed."""
    if not sys.platform.startswith("linux") or cfg["os"].lower() != "linux":
        raise gc.CaptureError("the rehearsal runs on Linux")
    if gc._elevated():
        raise gc.CaptureError("golden-rehearsal refuses an elevated or root process")
    if not 0 < timeout < float("inf"):
        raise gc.CaptureError("timeout must be finite and positive")
    out_path = Path(out).resolve()
    transcript_path = out_path.with_name(out_path.name + ".transcript.json")
    for path in (out_path, transcript_path):
        if path.exists():
            raise gc.CaptureError(f"{path.name} exists; golden-rehearsal never replaces evidence")
    if not out_path.parent.is_dir():
        raise gc.CaptureError("the private receipt directory does not exist")
    if build is None:
        from parity_build import build_harness_artifacts
        build = build_harness_artifacts
    session = Rehearsal(cfg, out_path, timeout=timeout, build=build, cases=cases,
                        operator=operator)
    receipt: dict[str, Any] | None = None
    failure: BaseException | None = None
    try:
        receipt = session.run()
    except BaseException as exc:  # Cleanup and the transcript run for every failure.
        failure = exc
    cleanup = session.cleanup()
    if receipt is not None:
        receipt["cleanup"] = cleanup
        receipt["completed_utc"] = datetime.datetime.now(datetime.timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ")
        if cleanup["errors"]:
            receipt["result"] = "fail"
            receipt["failures"].append(["cleanup", "; ".join(cleanup["errors"])])
    session.transcript["result"] = (f"failed: {failure}" if failure is not None
                                    else receipt["result"] if receipt else "failed")
    outputs = [(transcript_path, gc._serialize(session.transcript))]
    if receipt is not None:
        receipt["transcript"] = transcript_path.name
        outputs.insert(0, (out_path, gc._serialize(receipt)))
    if any(session.capability.encode("ascii") in data for _path, data in outputs):
        raise gc.CaptureError("the adapter capture capability leaked into the rehearsal "
                              "evidence; nothing was written") from failure
    for path, data in outputs:
        gc._write_private(path, data)
    if failure is not None:
        if isinstance(failure, (gc.CaptureError, ValueError, OSError)):
            raise gc.CaptureError(f"{failure}; transcript: {transcript_path.name}") from failure
        raise failure
    assert receipt is not None
    print(json.dumps({key: receipt[key] for key in (
        "result", "vosu_commit", "tooling_digest", "cases", "rehearsed", "renderings",
        "outcomes", "upstream_restarts", "failures")}, sort_keys=True))
    if receipt["result"] != "pass":
        raise gc.CaptureError(f"the rehearsal failed {len(receipt['failures'])} checks; "
                              f"receipt: {out_path.name}")
    return 0
