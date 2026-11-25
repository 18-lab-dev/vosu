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
run at `no-client`, where their guards require both daemons to report it, and one that
fails only for want of game data (`CLIENT_DATA`) is client-dependent, not a failure.

Both daemons discover osu! processes. The build runs on the host, where the
protected-tool checks see the real file owners; the daemons and every case then run as
PID 1 of a private PID namespace with its own /proc (`ISOLATION`), which the tool enters
itself and which refuses to run if it can see any osu! client, so a game the host runs
is invisible to both daemons.

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
import os
import re
import shutil
import subprocess
import sys
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
# Outcomes that only say the daemon had no game data to answer with: a WebSocket with no
# state frame, a request with no answer, a conditional request with no validator to send.
# A case of a client barrier (menu, paused, result) that fails only on these at
# `no-client` is client-dependent; any other failure, and any `no-client` case, fails.
CLIENT_DATA = re.compile(r"outcome '(?:upgraded-timeout|timeout|validator-missing)' is not allowed")
# The private PID namespace with its own /proc that the daemons and cases run in (the
# user namespace makes it unprivileged; the network namespace stays shared).
ISOLATION = ("--user", "--map-current-user", "--pid", "--fork", "--mount-proc")


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
            elif rm.upstream_crash_ledger(case):
                (value,) = crash.run_cases(self, [item], source, vosu, dump)
            else:
                value = self._golden_case(item, entry, source, vosu, dump)
            if value is not None:
                entry["outcome"] = value["attempts"][-1]["outcome"]
                entry["failed_attempts"] = [a["reason"] for a in value["attempts"][:-1]]
        except (gc.CaptureError, GoldenError, ValueError, OSError) as exc:
            entry["failure"] = f"{type(exc).__name__}: {exc}"
        return entry, value

    def _golden_case(self, item: rm.RenderedCase, entry: dict[str, Any], source: gc.Daemon,
                     vosu: gc.Daemon, dump: gc.HarnessDump) -> dict[str, Any] | None:
        """`_run_case`, except that a client-barrier case whose every attempt lacked only
        the game's data is recorded as client-dependent instead of failing."""
        failed: list[dict[str, Any]] = []
        for number in range(1, gc.MAX_ATTEMPTS + 1):
            result = self._attempt(item, number, failed, source, vosu, dump)
            if isinstance(result, dict):
                return result
            failed.append({"attempt": number, "outcome": "failed", "reason": result})
        reasons = [attempt["reason"] for attempt in failed]
        if item.case.barrier != "no-client" and all(CLIENT_DATA.search(r) for r in reasons):
            entry["client_dependent"] = reasons
        else:
            entry["failure"] = "; ".join(reasons)
        return None

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
            "outcomes": _count(entry.get("outcome") or ("client-dependent" if "client_dependent"
                                                        in entry else entry["role"])
                               for entry in entries if "failure" not in entry),
            "client_dependent": [[entry["case"], entry["client_dependent"]] for entry in entries
                                 if "client_dependent" in entry],
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


def _outputs(out: str) -> tuple[Path, Path]:
    out_path = Path(out).resolve()
    transcript_path = out_path.with_name(out_path.name + ".transcript.json")
    for path in (out_path, transcript_path):
        if path.exists():
            raise gc.CaptureError(f"{path.name} exists; golden-rehearsal never replaces evidence")
    if not out_path.parent.is_dir():
        raise gc.CaptureError("the private receipt directory does not exist")
    return out_path, transcript_path


def rehearse(cfg: dict[str, Any], out: str, *, timeout: float = 3600.0,
             build: Callable[[dict[str, Any], str], dict[str, Any]] | None = None,
             cases: Iterable[rm.RouteCase] = rm.MATRIX) -> int:
    """Build both daemons, then rehearse in a private PID namespace; 0 only on a pass.

    The build runs here, where the protected-tool checks see the real file owners. The
    daemons and every case run in `unshare ISOLATION`, whose own /proc shows no host
    process: neither daemon can find a game the host runs, and the phase refuses one it
    can see. Loopback is shared, so the configured ports work unchanged."""
    if not sys.platform.startswith("linux") or cfg["os"].lower() != "linux":
        raise gc.CaptureError("the rehearsal runs on Linux")
    if gc._elevated():
        raise gc.CaptureError("golden-rehearsal refuses an elevated or root process")
    if not 0 < timeout < float("inf"):
        raise gc.CaptureError("timeout must be finite and positive")
    out_path, _transcript = _outputs(out)
    unshare = shutil.which("unshare")
    if unshare is None:
        raise gc.CaptureError("the rehearsal needs util-linux `unshare` for its PID namespace")
    if build is None:
        from parity_build import build_harness_artifacts
        build = build_harness_artifacts
    built = build(cfg, str(out_path))
    state = out_path.with_name(f".{out_path.name}.state.json")
    try:
        gc._write_private(state, json.dumps({
            "cfg": cfg, "build": built, "out": str(out_path), "timeout": timeout,
            "cases": [case.id for case in cases]}).encode("utf-8"))
        with subprocess.Popen([unshare, *ISOLATION, "--", sys.executable,
                               str(Path(__file__).resolve()), "--isolated", str(state)],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as phase:
            assert phase.stdout is not None and phase.stderr is not None
            for line in phase.stderr:  # Operator progress, as it happens.
                print(line, end="", file=sys.stderr, flush=True)
            print(phase.stdout.read(), end="", flush=True)
        code = phase.returncode
    finally:
        state.unlink(missing_ok=True)
        _remove_leftovers(built)
    if code != 0:
        raise gc.CaptureError(f"the isolated rehearsal failed (exit {code}); receipt and "
                              f"transcript next to {out_path.name}")
    return 0


def _remove_leftovers(build: dict[str, Any]) -> None:
    """Build outputs the isolated phase did not reach (it removes them itself)."""
    for tree in build.get("owned_worktrees", []):
        if Path(tree["path"]).exists():
            gc._remove_worktree(tree["repository"], tree["path"])
    for owned in build.get("owned_paths", []):
        if Path(owned).exists():
            shutil.rmtree(owned)


def isolated_phase(state_path: str) -> int:
    """The daemons and every case, as PID 1 of the private PID namespace."""
    if os.getpid() != 1:
        raise gc.CaptureError("the rehearsal phase runs only as PID 1 of its private PID "
                              "namespace; start it with `run.py golden-rehearsal`")
    running = sorted(gc.daemon_mod._running_osu_processes())
    if running:  # Both daemons would find it and read that game's memory.
        raise gc.CaptureError(f"an osu! client is visible (PIDs {running}); the rehearsal "
                              "runs with no game")
    state = json.loads(Path(state_path).read_text("utf-8"))
    out_path, transcript_path = _outputs(state["out"])
    session = Rehearsal(state["cfg"], out_path, timeout=state["timeout"],
                        build=lambda _cfg, _out: state["build"],
                        cases=[rm.CASES[case_id] for case_id in state["cases"]],
                        operator=gc.stderr_operator)
    receipt: dict[str, Any] | None = None
    failure: BaseException | None = None
    try:
        receipt = session.run()
    except BaseException as exc:  # Cleanup and the transcript run for every failure.
        failure = exc
    cleanup = session.cleanup()
    if receipt is not None:
        receipt["isolation"] = {"unshare": list(ISOLATION), "pid": os.getpid()}
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


if __name__ == "__main__":
    if sys.argv[1:2] != ["--isolated"] or len(sys.argv) != 3:
        raise SystemExit("usage: run.py golden-rehearsal (this module's entry is internal)")
    try:
        raise SystemExit(isolated_phase(sys.argv[2]))
    except gc.CaptureError as error:
        print(f"golden-rehearsal: {error}", file=sys.stderr)
        raise SystemExit(2) from None
