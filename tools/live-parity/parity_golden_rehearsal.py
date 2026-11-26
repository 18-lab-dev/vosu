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
A case that every platform excludes, each for a reason `EXCLUSIONS` attributes to the
contract or ledger, is listed in the receipt as excluded with those authorities; any
other unrenderable case fails the rehearsal.
Every barrier other than `no-client` needs a client and is not rehearsed: those cases
run at `no-client`, where their guards require both daemons to report it, and one that
fails only for want of game data (`CLIENT_DATA`) is client-dependent, not a failure.

Both daemons discover osu! processes. The build runs on the host, where the
protected-tool checks see the real file owners; the daemons and every case then run as
PID 1 of a private PID namespace with its own /proc (`ISOLATION`), which the tool enters
itself and which refuses to run if it can see any osu! client, so a game the host runs
is invisible to both daemons. The phase handles termination signals itself (PID 1
ignores the rest) and exits as soon as its receipt is written (`main`). The source
worktree is removed by the outer phase, which adds its cleanup to the receipt: git's
trusted tool check cannot pass inside the user namespace.

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
import signal
import subprocess
import sys
import threading
import traceback
from typing import Any, Callable, Iterable, NoReturn

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
# The lexical stable skin file that the inside-root skin spellings (ADS, trailing dot
# and space) are rendered against; windows-stable is the only platform with a skin root.
FOREIGN_SKIN_FILE = "vosu-rehearsal/skin.ini"
# Executed tooling and the normative documents its checks read.
TOOLING_DOCUMENTS = ("tests/fixtures/upstream/GOLDEN_CONTRACT.md",
                     "tests/fixtures/upstream/DIVERGENCES.md")
# `golden`'s whole-capture check needs a client attestation; none ran.
NO_CLIENT = {"version": "no-client", "sha256": "0" * 64}
# Outcomes that only say the daemon had no game data to answer with: a WebSocket with no
# state frame, a conditional request whose priming request got no 2xx (no file to send a
# validator for). A case of a client barrier (menu, paused, result) that fails only on
# these at `no-client` is client-dependent; any other failure, and any `no-client` case,
# fails. Never client-dependent: a `timeout` (not one response byte), which hid seven
# deterministic upstream hangs until the first full pilot run, and a 2xx priming response
# without a validator, which hid upstream's missing file validators until the second
# (2026-11-26). The declared ones pass as their outcome (`rm.UPSTREAM_EXPECTED`).
CLIENT_DATA = re.compile(r"outcome 'upgraded-timeout' is not allowed"
                         r"|outcome 'validator-missing' is not allowed \(priming status "
                         r"(?:None|[013-9]\d\d)\)")
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
    inside = {prefix: value for prefix, value in native.security.inside_files.items()
              if prefix in roots}
    if stable:
        roots["/files/skin/"] = FOREIGN_ROOT + "\\osu\\Skins"
        inside["/files/skin/"] = FOREIGN_SKIN_FILE
    probe = f"{FOREIGN_ROOT}\\{rs.SENTINEL_DIR}\\"
    layout = rs.SecurityLayout(
        "windows", FOREIGN_ROOT, native.security.token, roots, inside,
        probe + rs.SENTINEL_FILE, probe + rs.SENTINEL_CHART, "0" * 64,
        other_roots={} if stable else {"files": files})
    values = dict(native.values)
    if "calc_relative" in values:
        values["calc_absolute_inside"] = files + "\\" + values["calc_relative"].replace("/", "\\")
    return rm.MatrixContext(platform, values, layout)


def plan(native: rm.MatrixContext, cases: Iterable[rm.RouteCase]
         ) -> tuple[list[tuple[str, rm.RenderedCase]], list[list[Any]], list[list[str]]]:
    """(rendering, case) for every case, natively where possible, then the cases every
    platform excludes under a listed authority (`gc.EXCLUSIONS`) with each platform's
    reason and authority, then the unrenderable rest, which fail the rehearsal."""
    foreign = [_foreign_context(native, platform) for platform in FOREIGN_PLATFORMS]
    planned, excluded, unrendered = [], [], []
    for case in cases:
        reasons = []
        for context in (native, *foreign):
            try:
                item = rm.render(case, context)
            except (rm.CaseUnavailable, rs.SecurityAbort) as exc:
                reasons.append([context.platform, str(exc),
                                gc.exclusion_authority(str(exc), context.platform)])
                continue
            if context is not native:  # Sent to the Linux daemons, where it can only miss.
                item = dataclasses.replace(item, lexical_only=False)
            elif item.lexical_only:
                raise gc.CaptureError(f"{case.id}: a native rendering is lexical only")
            planned.append((context.platform, item))
            break
        else:
            if all(authority is not None for _platform, _reason, authority in reasons):
                excluded.append([case.id, reasons])
            else:
                unrendered.append([case.id, "; ".join(f"{platform}: {reason}"
                                                      for platform, reason, _ in reasons)])
    order = {False: 0, True: 1}  # upstream_crashed cases last
    planned.sort(key=lambda entry: order[bool(rm.upstream_crash_ledger(entry[1].case))])
    return planned, excluded, unrendered


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
        planned, excluded, unrendered = plan(context, self.cases)
        self.transcript["excluded"] = excluded
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
            "handshake_only": [entry["case"] for entry in entries if "failure" not in entry
                               and "upgraded-timeout" in rm.CASES[entry["case"]].outcomes
                               and entry["role"] == rm.GOLDEN],
            "findings": self.transcript["findings"],
            "excluded": excluded,
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
        code = _relay([unshare, *ISOLATION, "--", sys.executable,
                       str(Path(__file__).resolve()), "--isolated", str(state)])
    finally:
        state.unlink(missing_ok=True)
        outer = _remove_leftovers(built)
        _record_outer_cleanup(out_path, outer)
    if outer["errors"]:
        raise gc.CaptureError("the outer cleanup failed: " + "; ".join(outer["errors"]))
    if code != 0:
        raise gc.CaptureError(f"the isolated rehearsal failed (exit {code}); receipt and "
                              f"transcript next to {out_path.name}")
    return 0


def _relay(command: list[str]) -> int:
    """Run the isolated phase, relaying its stderr (operator progress) as it happens and
    then its stdout. Both pipes drain at once: its daemons inherit its stdout, so reading
    stderr to the end first deadlocked once their output filled the stdout pipe
    (rehearsal 2026-11-25: the phase blocked in write after its receipt)."""
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          text=True) as phase:
        assert phase.stdout is not None and phase.stderr is not None
        stdout: list[str] = []
        drain = threading.Thread(target=lambda: stdout.append(phase.stdout.read()), daemon=True)
        drain.start()
        for line in phase.stderr:
            print(line, end="", file=sys.stderr, flush=True)
        drain.join()
    print("".join(stdout), end="", flush=True)
    return phase.returncode


def _remove_leftovers(build: dict[str, Any]) -> dict[str, Any]:
    """The source worktree, which only this outer phase can remove (git's trusted tool
    check fails inside the user namespace), and any build output the isolated phase
    did not reach. Every step runs; failures are reported, never raised."""
    report: dict[str, Any] = {"worktrees_removed": 0, "paths_removed": 0, "errors": []}
    for tree in build.get("owned_worktrees", []):
        if Path(tree["path"]).exists():
            try:
                gc._remove_worktree(tree["repository"], tree["path"])
                report["worktrees_removed"] += 1
            except Exception as exc:  # Cleanup must reach every owned resource.
                report["errors"].append(f"source worktree: {type(exc).__name__}: {exc}")
    for owned in build.get("owned_paths", []):
        if Path(owned).exists():
            try:
                shutil.rmtree(owned)
                report["paths_removed"] += 1
            except OSError as exc:
                report["errors"].append(f"build output: {type(exc).__name__}: {exc}")
    return report


def _record_outer_cleanup(out_path: Path, outer: dict[str, Any]) -> None:
    """Add the outer phase's cleanup to the receipt; a failed one fails the receipt."""
    if not out_path.exists():
        return  # The isolated phase wrote no receipt; its transcript says why.
    receipt = json.loads(out_path.read_text("utf-8"))
    receipt["outer_cleanup"] = outer
    if outer["errors"]:
        receipt["result"] = "fail"
        receipt["failures"].append(["outer cleanup", "; ".join(outer["errors"])])
    temporary = out_path.with_name(f".{out_path.name}.outer.tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(gc._serialize(receipt))
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, out_path)
    finally:
        temporary.unlink(missing_ok=True)


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
    # git's trusted tool check cannot pass inside the user namespace, so the source
    # worktree is the outer phase's to remove (`_remove_leftovers`), not this cleanup's.
    deferred = state["build"].get("owned_worktrees", [])
    build = {**state["build"], "owned_worktrees": [], "outer_phase_worktrees": deferred}
    session = Rehearsal(state["cfg"], out_path, timeout=state["timeout"],
                        build=lambda _cfg, _out: build,
                        cases=[rm.CASES[case_id] for case_id in state["cases"]],
                        operator=gc.stderr_operator)
    receipt: dict[str, Any] | None = None
    failure: BaseException | None = None
    try:
        receipt = session.run()
    except BaseException as exc:  # Cleanup and the transcript run for every failure.
        failure = exc
    cleanup = session.cleanup()
    if deferred:
        cleanup["deferred_to_outer_phase"] = ["source worktree"] * len(deferred)
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
        "outcomes", "handshake_only", "findings", "upstream_restarts", "failures")},
        sort_keys=True))
    if receipt["result"] != "pass":
        raise gc.CaptureError(f"the rehearsal failed {len(receipt['failures'])} checks; "
                              f"receipt: {out_path.name}")
    return 0


class Terminated(BaseException):
    """A termination signal reached the isolated phase; its cleanup still runs."""

    def __init__(self, signum: int) -> None:
        super().__init__(f"terminated by {signal.Signals(signum).name}")
        self.signum = signum


def _terminate(signum: int, _frame: Any) -> None:
    raise Terminated(signum)


def main(argv: list[str], phase: Callable[[str], int] = isolated_phase) -> NoReturn:
    """The isolated phase's entry, as PID 1 of its namespace.

    PID 1 ignores every signal it has no handler for, so SIGTERM, SIGHUP and SIGINT get
    one: each ends the phase through its cleanup and transcript. Once the receipt is
    written the process exits at once (`os._exit`): it never waits for a lingering
    thread, and PID 1's exit ends whatever is left in the namespace."""
    for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(signum, _terminate)
    if argv[1:2] != ["--isolated"] or len(argv) != 3:
        print("usage: run.py golden-rehearsal (this module's entry is internal)", file=sys.stderr)
        code = 2
    else:
        try:
            code = phase(argv[2])
        except gc.CaptureError as error:
            print(f"golden-rehearsal: {error}", file=sys.stderr)
            code = 2
        except Terminated as error:
            print(f"golden-rehearsal: {error}", file=sys.stderr)
            code = 128 + error.signum
        except BaseException:  # Reported, then the same immediate exit.
            traceback.print_exc()
            code = 1
    sys.stdout.flush()
    sys.stderr.flush()
    os._exit(code)


if __name__ == "__main__":
    main(sys.argv)
