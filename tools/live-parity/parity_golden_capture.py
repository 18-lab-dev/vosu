"""`golden-capture`: attested private `vosu-golden-capture-v1` captures (plan §5 P1).

GOLDEN_CONTRACT.md is normative. One run builds and attests the debug test-harness
vosu (A4b.2), launches and owns the pinned upstream daemon, the harness vosu daemon
(with a fresh per-launch adapter capture capability) and the client, prepares the
run-owned path-security layout, then traverses the scenario's barriers in the order
menu -> play -> paused -> result -> no-client. At each stable barrier every case of
P0's route matrix for that barrier runs as two sandwiches: upstream twice with
identical bytes, then vosu dump -> route -> dump. A failed sandwich retries the whole
case, at most three attempts, and every attempt is recorded; attempts are never
mixed. Upstream records are the only expected values; vosu dumps are replay input.
Cases whose request kills upstream run last, after the no-client barrier, and record
the `upstream_crashed` outcome (`parity_golden_crash`). A run starts only with a passing
client-less rehearsal receipt for its authorized commit and tooling (`parity_golden_rehearsal`).

Outputs, both private and never published: the capture `--out`, exactly the shape
`golden` reads, and `<out>.transcript.json` with the raw records, attestations,
barrier log and cleanup result. Neither may contain the capability. Every owned
process and file is removed before the command returns.
"""
from __future__ import annotations

import base64
from dataclasses import dataclass
import copy
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time
from typing import Any, Callable, Protocol

from live_parity import PINNED_SOURCE_REVISION, Sample, barrier_of
import parity_daemon as daemon_mod
import parity_golden
from parity_golden import CAPTURE_FORMAT, ENVELOPE_FORMAT, GoldenError
from parity_golden_rules import TRANSPORT_OUTCOMES, dump_sandwich_difference
from parity_probes import HttpProbe, _assert_one_owner
from parity_process import _process_alive
import parity_routes as pr
import route_matrix as rm
import route_security as rs
import parity_volatile as volatile
import parity_golden_crash as crash
from parity_golden_client import (check_client_root, log_names, remove_new_shared_logs,
                                  shared_profile_logs)

TRANSCRIPT_FORMAT = "vosu-golden-capture-transcript-v1"
CAPABILITY_ENV = "VOSU_TEST_ADAPTER_CAPTURE"
CAPABILITY_HEADER = "X-Vosu-Test-Capture"
ENDPOINT = "/__vosu_test/v1/adapter-capture"
REDACTED = "<redacted>"
TRAVERSAL = ("menu", "play", "paused", "result", "no-client")
MAX_ATTEMPTS = parity_golden.MAX_ATTEMPTS
MAX_DUMP_BYTES = 8 << 20
POLL_S = 0.1
# A barrier holds once both daemons report it across this many consecutive observations.
# A barrier with cases also needs the non-volatile views of the upstream v2 payload and
# the vosu v2 dump to stay identical across them; a case-less one (play: its clock and
# the provider generation advance on every read) needs only the reported state.
STABLE_OBSERVATIONS = 3
# A record must carry an HTTP response to be a golden record. `upgraded-timeout` is the
# handshake of a feed that sent no frame in its window (route_matrix.HANDSHAKE_ONLY).
RESPONSE_OUTCOMES = frozenset({"response", "rejected", "upgraded", "upgraded-timeout",
                               "server-closed"})
_SCHEMA_OF_PATH = {"/json": "v1", "/json/sc": "sc", "/json/v2": "v2",
                   "/json/v2/precise": "v2-precise", "/ws": "v1", "/tokens": "sc",
                   "/websocket/v2": "v2", "/websocket/v2/precise": "v2-precise"}
OPERATOR_STEPS = {
    "menu": "open song select on the approved map (menu)",
    "play": "start the approved map (active play)",
    "paused": "pause gameplay and leave it paused",
    "result": "resume, finish the map and stay on the result screen",
    "no-client": "close the client",
}


class CaptureError(ValueError):
    """golden-capture cannot produce a complete attested capture."""


@dataclass(frozen=True)
class ScenarioSet:
    """Barriers traversed, in contract order, and the golden cases recorded at them.

    `cases` None selects every golden-role route-matrix case at a traversed barrier."""
    id: str
    barriers: tuple[str, ...]
    cases: tuple[str, ...] | None = None

    def __post_init__(self) -> None:
        order = [TRAVERSAL.index(b) for b in self.barriers if b in TRAVERSAL]
        if len(order) != len(self.barriers) or order != sorted(set(order)):
            raise ValueError(f"scenario {self.id}: barriers must follow {' -> '.join(TRAVERSAL)}")


SCENARIO_SETS = {
    "m4b-full-v1": ScenarioSet("m4b-full-v1", TRAVERSAL),
    # Mandatory O0 dry run: one barrier, three cases, then `golden` (verification.md).
    "m4b-dry-run-v1": ScenarioSet("m4b-dry-run-v1", ("menu",), (
        "state-http.json-sc.get.menu", "state-http.json-v2.get.menu", "ws.v2.upgrade.menu")),
}


class Client(Protocol):
    def start(self) -> dict[str, Any]: ...
    def exited(self) -> bool: ...
    def stop(self) -> list[str]: ...


class LiveClient:
    """The attested osu client of the run configuration (parity_daemon lifecycle)."""

    def __init__(self, cfg: dict[str, Any]) -> None:
        self.cfg = cfg
        self.artifact = cfg["_artifact_identity"]["client"]
        self.process: subprocess.Popen[bytes] | None = None
        self.shared_logs = shared_profile_logs(cfg)
        self.shared_profile: dict[str, Any] | None = None

    def start(self) -> dict[str, Any]:
        self.shared_before = log_names(self.shared_logs)
        self.process, evidence = daemon_mod._launch_client(self.cfg)
        return evidence

    def exited(self) -> bool:
        return not daemon_mod._assert_client_population(self.artifact, True)

    def stop(self) -> list[str]:
        errors = []
        if "pid" not in self.artifact:
            return errors
        daemon_mod._close_windows_client_job(self.artifact)
        daemon_mod._terminate_client_descendants(self.artifact)
        if self.process is not None:
            daemon_mod._signal_process_tree(self.process)
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon_mod._signal_process_tree(self.process, hard=True)
                self.process.wait(timeout=5)
        if not daemon_mod._wait_for_client_descendants(self.artifact, 5):
            daemon_mod._terminate_client_descendants(self.artifact, hard=True)
            if not daemon_mod._wait_for_client_descendants(self.artifact, 1):
                errors.append("attested osu client descendants survived cleanup")
        self.shared_profile = remove_new_shared_logs(self.shared_logs, self.shared_before,
                                                     exited=self._released)
        errors += [f"shared lazer log {entry['name']} is still open after the client exited: "
                   f"{entry['error']}" for entry in self.shared_profile["locked"]]
        return errors

    def _released(self) -> bool:
        """The attested client has exited (an unapproved process keeps the wait going)."""
        try:
            return self.exited()
        except ValueError:
            return False


def stderr_operator(barrier: str, message: str) -> None:
    print(f"golden-capture: {message}", file=sys.stderr, flush=True)


# ---------------------------------------------------------------- case plan


# The only reasons a golden matrix case may be missing from a run, each with the
# contract or ledger authority that records it as not applicable on that platform.
EXCLUSIONS = (
    (re.compile(r"requires stable client"),
     "GOLDEN_CONTRACT §6 files: the stable /files/skin/<name> variant; lazer records "
     "files.skin.lazer-file instead (ledger lazer-legacy-skin-404)"),
    (re.compile(r"requires lazer client"),
     "GOLDEN_CONTRACT §6 files: the lazer /files/skin/<name> variant "
     "(ledger lazer-legacy-skin-404) exists only for lazer"),
    (re.compile(r"requires windows"),
     "GOLDEN_CONTRACT §6 path-security: drive, UNC, ADS and trailing dot/space spellings "
     "are Windows only"),
    (re.compile(r"requires linux"),
     "GOLDEN_CONTRACT §6 path-security: POSIX absolute spellings (Windows probes the "
     "drive spellings)"),
    (re.compile(r"no declared root for /files/skin/ on (?:windows|linux)-lazer"),
     "ledger lazer-legacy-skin-404: a lazer client has no legacy skin root to traverse from"),
    (re.compile(r"symlink fixture unavailable: .+ on windows-(?:stable|lazer)"),
     "GOLDEN_CONTRACT §6 path-security: a symlink or reparse fixture only where the "
     "platform permits"),
)


def exclusion_authority(reason: str, platform: str) -> str | None:
    """The authority admitting every unmet prerequisite in `reason`, or None."""
    authorities = []
    for part in reason.split(": ", 1)[-1].split("; "):
        if part.startswith("symlink fixture unavailable"):
            part = f"{part} on {platform}"
        found = [text for pattern, text in EXCLUSIONS if pattern.fullmatch(part)]
        if not found:
            return None
        authorities.append(found[0])
    return "; ".join(dict.fromkeys(authorities))


def plan_cases(scenario: ScenarioSet, context: rm.MatrixContext
               ) -> tuple[list[rm.RenderedCase], list[rm.RenderedCase], list[list[str]]]:
    """(golden cases, other-role cases, exclusions) for the run, in matrix order.

    A golden case may be excluded only for a reason `EXCLUSIONS` attributes to the
    contract or ledger; any other unmet prerequisite makes the case set incomplete.
    Exclusions are (case, reason, authority) triples."""
    wanted = set(scenario.cases or ())
    unknown = wanted - {case.id for case in rm.MATRIX if rm.golden_eligible(case)}
    if unknown:
        raise CaptureError(f"scenario {scenario.id} names non-golden or unknown cases {sorted(unknown)}")
    candidates = [case for case in rm.MATRIX if case.barrier in scenario.barriers
                  and (scenario.cases is None or case.id in wanted)]
    rendered, excluded = rm.select(candidates, context)
    exclusions = [[case_id, reason, exclusion_authority(reason, context.platform)]
                  for case_id, reason in excluded if rm.golden_eligible(rm.CASES[case_id])]
    incomplete = [f"{case_id}: {reason}" for case_id, reason, authority in exclusions
                  if authority is None]
    if incomplete:
        raise CaptureError("incomplete case set; fix the golden configuration: "
                           + "; ".join(incomplete))
    if any(item.lexical_only for item in rendered):
        raise CaptureError("path cases must run on their native platform")
    golden = [item for item in rendered if rm.golden_eligible(item.case)]
    others = [item for item in rendered if not rm.golden_eligible(item.case)]
    if not golden:
        raise CaptureError(f"scenario {scenario.id} selects no golden case on {context.platform}")
    return golden, others, exclusions


def check_expected_cases(expected: list[str], scenario: ScenarioSet,
                         context: rm.MatrixContext) -> None:
    """Cross-check the recorded expected_cases against P0's route matrix."""
    golden, _others, _excluded = plan_cases(scenario, context)
    if expected != [item.case.id for item in golden]:
        raise CaptureError("expected_cases differ from the route matrix plan")
    for case_id in expected:
        case = rm.CASES.get(case_id)
        if case is None or not rm.golden_eligible(case) or case.barrier not in scenario.barriers:
            raise CaptureError(f"expected case {case_id} is not a traversed golden matrix case")


def dump_schema(case: rm.RouteCase) -> str:
    return _SCHEMA_OF_PATH.get(case.target.partition("?")[0], "v2")


# ---------------------------------------------------------------- daemons


@dataclass
class Daemon:
    name: str
    port: int
    evidence: dict[str, Any]

    @property
    def endpoint(self) -> pr.RouteEndpoint:
        return pr.RouteEndpoint(self.name, self.port, self.evidence)


class HarnessDump:
    """The vosu adapter capture endpoint. Dump calls are never route records."""

    def __init__(self, daemon: Daemon, capability: str) -> None:
        self.daemon = daemon
        self.capability = capability

    def _request(self, schema: str, capability: str | None) -> bytes:
        lines = [f"GET {ENDPOINT}?schema={schema} HTTP/1.1", f"Host: 127.0.0.1:{self.daemon.port}"]
        if capability is not None:
            lines.append(f"{CAPABILITY_HEADER}: {capability}")
        return ("\r\n".join([*lines, "Connection: close"]) + "\r\n\r\n").encode("ascii")

    def _exchange(self, schema: str, capability: str | None) -> tuple[int, dict[str, str], bytes]:
        started = time.monotonic_ns()
        _assert_one_owner(self.daemon.evidence, f"dump/{schema}/before", started)
        sock = socket.create_connection(("127.0.0.1", self.daemon.port), timeout=10)
        try:
            if sock.getpeername()[0] != "127.0.0.1":
                raise CaptureError("harness endpoint is not loopback")
            sock.sendall(self._request(schema, capability))
            response = http.client.HTTPResponse(sock, method="GET")
            response.begin()
            body = response.read(MAX_DUMP_BYTES + 1)
            headers = {name.lower(): value for name, value in response.getheaders()}
            status = response.status
            response.close()
        finally:
            sock.close()
        _assert_one_owner(self.daemon.evidence, f"dump/{schema}/after", started)
        if len(body) > MAX_DUMP_BYTES:
            raise CaptureError("harness dump exceeds 8 MiB")
        return status, headers, body

    def fetch(self, schema: str) -> dict[str, Any]:
        status, headers, body = self._exchange(schema, self.capability)
        if status != 200 or headers.get("content-type") != "application/json" \
                or headers.get("cache-control") != "no-store":
            raise CaptureError(f"harness dump for {schema} answered {status}")
        try:
            envelope = json.loads(body.decode("utf-8"), object_pairs_hook=_unique)
        except (UnicodeDecodeError, ValueError) as exc:
            raise CaptureError(f"harness dump for {schema} is not strict JSON") from exc
        if not isinstance(envelope, dict) or envelope.get("format") != ENVELOPE_FORMAT \
                or envelope.get("schema") != schema:
            raise CaptureError(f"harness dump for {schema} is not a {ENVELOPE_FORMAT} envelope")
        try:
            digest = parity_golden.envelope_digest(envelope)
        except KeyError as exc:
            raise CaptureError(f"harness dump for {schema} lacks {exc}") from exc
        if envelope.get("digest") != digest:
            raise CaptureError(f"harness dump for {schema} has a wrong digest")
        return envelope

    def verify_mounted(self) -> dict[str, Any]:
        """The harness build serves the endpoint only with this launch's capability."""
        served = self._exchange("v2", self.capability)[0]
        if served != 200:
            raise CaptureError(
                f"vosu answered {served} on the adapter capture endpoint with the launch "
                "capability; a release or featureless build cannot make golden captures")
        absent = self._exchange("v2", None)[0]
        wrong = self._exchange("v2", secrets.token_hex(32))[0]
        if absent != 404 or wrong != 404:
            raise CaptureError("the adapter capture endpoint answers without the launch capability")
        self.fetch("v2")
        return {"with_capability": served, "without_capability": absent,
                "wrong_capability": wrong}


def _unique(items: list[tuple[str, Any]]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for key, value in items:
        if key in out:
            raise ValueError(f"duplicate JSON key {key!r}")
        out[key] = value
    return out


# ---------------------------------------------------------------- records


def _g0_bytes(data: bytes) -> dict[str, str]:
    try:
        return {"text": data.decode("utf-8")}
    except UnicodeDecodeError:
        return {"b64": base64.b64encode(data).decode("ascii")}


def _unmask(frame: bytes, mask_hex: str, length: int) -> bytes:
    mask = bytes.fromhex(mask_hex)
    payload = frame[len(frame) - length:]
    return bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))


def golden_record(record: dict[str, Any], attempt: int, family: str) -> dict[str, Any]:
    """One P0 raw record as a `vosu-golden-capture-v1` record (body after transfer decoding).

    A connection closed before any response byte is the explicit transport record
    `{"outcome": "closed"}`, compared like any other part; it is never dropped."""
    response = record.get("response")
    if record.get("outcome") in TRANSPORT_OUTCOMES and not response \
            and record["received"]["length"] == 0:
        value = {"attempt": attempt, "request": _g0_bytes(pr.decode_bytes(record["request"])),
                 "outcome": record["outcome"]}
        if family == "ws":
            value["frames"] = []
        return value
    if record.get("outcome") not in RESPONSE_OUTCOMES or not response:
        raise CaptureError(f"outcome {record.get('outcome')!r} is neither a response nor a "
                           "connection closed without a response byte")
    value: dict[str, Any] = {
        "attempt": attempt, "request": _g0_bytes(pr.decode_bytes(record["request"])),
        "status_line": response["status_line"], "headers": response["headers"],
        "body": _g0_bytes(pr.decode_bytes(response["body"])),
    }
    if family == "ws":
        frames = []
        for frame in (record.get("websocket") or {}).get("frames", []):
            if frame["rsv"]:
                raise CaptureError("WebSocket frame sets RSV bits")
            if frame["direction"] == "client":
                payload = _unmask(pr.decode_bytes(frame["bytes"]), frame["mask"], frame["length"])
                mask = frame["mask"]
            else:
                payload, mask = pr.decode_bytes(frame["payload"]), frame["mask"]
            frames.append({"from": frame["direction"], "fin": frame["fin"],
                           "opcode": frame["opcode"], "masked": frame["masked"],
                           "mask_key": mask, "length": len(payload),
                           "payload": _g0_bytes(payload)})
        value["frames"] = frames
    return value


def _raw(value: dict[str, str]) -> bytes:
    return value["text"].encode("utf-8") if "text" in value else base64.b64decode(value["b64"])


def _comparable(record: dict[str, Any]) -> tuple[Any, ...]:
    """The parts a golden compares; the Date value is normalized, its presence is not.

    Volatile-ledger fields are compared by presence, type and rule (`parity_volatile`);
    a record breaking a rule never compares equal to anything."""
    if "outcome" in record:
        return (record["outcome"],)
    request = _raw(record["request"]).decode("utf-8", "replace").split(" ", 2)
    path = request[1].partition("?")[0] if len(request) > 1 else ""
    headers = [[name, "<date>" if name.lower() == "date" else value]
               for name, value in record["headers"]
               if name.lower() in parity_golden.COMPARED_HEADERS
               or name.lower().startswith("access-control-")]
    frames = None if record.get("frames") is None else [
        (f["from"], f["fin"], f["opcode"], (f["masked"], f["mask_key"]), _raw(f["payload"]))
        for f in record["frames"]]
    try:
        return volatile.record_view(path, record["status_line"].split(" ", 2)[1], headers,
                                    _raw(record["body"]), frames)[0]
    except volatile.VolatileError as exc:
        return ("volatile rule broken", str(exc), id(record))


def _elevated() -> bool:
    if sys.platform == "win32":
        import ctypes
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    return os.geteuid() == 0


def _remove_worktree(repository: str, path: str) -> None:
    """Unregister and delete a run-owned `git worktree` of the source repository."""
    env = daemon_mod._git_environment()
    subprocess.run(daemon_mod._git_command(Path(repository), "worktree", "remove", "--force",
                                           path), capture_output=True, env=env, check=False)
    if Path(path).exists():
        shutil.rmtree(path)
    subprocess.run(daemon_mod._git_command(Path(repository), "worktree", "prune"),
                   capture_output=True, env=env, check=True)


# ---------------------------------------------------------------- run


class Session:
    """One golden capture run; owns every process and file it creates."""

    def __init__(self, cfg: dict[str, Any], scenario: ScenarioSet, out: Path, *,
                 timeout: float, build: Callable[[dict[str, Any], str], dict[str, Any]],
                 client: Client, operator: Callable[[str, str], None]) -> None:
        self.cfg, self.scenario, self.out = cfg, scenario, out
        self.golden_cfg = cfg["golden"]
        self.deadline = time.monotonic() + timeout
        self.build, self.client, self.operator = build, client, operator
        self.capability = secrets.token_hex(32)
        self.processes: dict[str, subprocess.Popen[bytes]] = {}
        self.starts: dict[str, str] = {}
        self.layout: rs.SecurityLayout | None = None
        self.ledger = rm.RequestLedger()
        self.transcript: dict[str, Any] = {
            "format": TRANSCRIPT_FORMAT, "scenario_set": scenario.id, "barriers": [],
            "cases": [], "diagnostics": [], "excluded": [], "findings": [], "cleanup": None}
        self.client_started = False

    # -- setup

    def _authorize(self, vosu_commit: str | None) -> None:
        authorization = self.golden_cfg["authorization"]
        client = self.cfg["_artifact_identity"]["client"]
        expected = {"platform": self.platform, "source_revision": PINNED_SOURCE_REVISION,
                    "scenario_set": self.scenario.id, "client_version": client["version"]}
        if vosu_commit is not None:
            expected["vosu_commit"] = vosu_commit
        for key, value in expected.items():
            if authorization[key] != value:
                raise CaptureError(f"authorization.{key} differs from this run; capture refused")

    @property
    def platform(self) -> str:
        return f"{self.cfg['os'].lower()}-{self.cfg['client'].lower()}"

    def _prepare_layout(self) -> rm.MatrixContext:
        golden = self.golden_cfg
        self.layout = rs.prepare_security_layout(
            golden["capture_root"], golden["route_roots"], golden["inside_files"],
            golden.get("sentinel_chart"), golden["other_roots"])
        return rm.MatrixContext(self.platform, golden["fixture_values"], self.layout)

    def _launch(self) -> tuple[Daemon, Daemon]:
        vosu_spec = dict(self.cfg["daemons"]["vosu"])
        env = dict(vosu_spec.get("env", {}))
        if CAPABILITY_ENV in env or CAPABILITY_ENV in self.cfg["daemons"]["source"].get("env", {}):
            raise CaptureError(f"{CAPABILITY_ENV} is generated per launch, never configured")
        env[CAPABILITY_ENV] = self.capability
        if sys.platform == "win32":
            # vosu keeps its broker credential under LOCALAPPDATA: make that run-owned.
            self.vosu_local = daemon_mod._private_runtime_dir("vosu-live-parity-vosu-")
            env["LOCALAPPDATA"] = str(self.vosu_local)
        vosu_spec["env"] = env
        launch_cfg = {**self.cfg, "daemons": {**self.cfg["daemons"], "vosu": vosu_spec}}
        processes, evidence = daemon_mod.launch(launch_cfg, source_static=self.layout.upstream_static)
        self.processes = processes
        self.starts = {name: evidence[name]["start_identity"] for name in processes}
        daemons = [Daemon(name, evidence[name]["port"], evidence[name])
                   for name in ("source", "vosu")]
        return daemons[0], daemons[1]

    def relaunch_source(self) -> Daemon:
        """A fresh, re-attested upstream in place of one an `upstream_crashed` case ended;
        the exited process stays owned, under its own label, until cleanup."""
        restarts = self.transcript.setdefault("upstream_restarts", [])
        label = f"source-exited-{len(restarts) + 1}"
        self.processes[label] = self.processes.pop("source")
        self.starts[label] = self.starts.pop("source")
        processes, evidence = daemon_mod.launch(self.cfg, ("source",), after_exit=True,
                                                source_static=self.layout.upstream_static)
        self.processes["source"] = processes["source"]
        self.starts["source"] = evidence["source"]["start_identity"]
        restarts.append(evidence["source"])
        return Daemon("source", evidence["source"]["port"], evidence["source"])

    # -- barriers

    def _observe(self, source: Daemon, dump: HarnessDump) -> tuple[dict[str, str | None], Any]:
        """Both daemons' barriers, and the non-volatile view of their v2 state."""
        with HttpProbe(f"http://127.0.0.1:{source.port}", "source", "v2", source.evidence) as probe:
            sample = probe.fetch()
        upstream = barrier_of(sample)
        envelope = dump.fetch("v2")
        if envelope["http"]["kind"] == "not_ready":
            vosu = "process-exit"
        else:
            try:
                payload = json.loads(envelope["http"]["body_utf8"])
            except ValueError:
                payload = None
            vosu = barrier_of(Sample("vosu", "v2", 0, payload))
        rename = {"process-exit": "no-client"}
        try:
            view = (volatile.payload_view("/json/v2", json.dumps(sample.payload).encode("utf-8"))[0],
                    volatile.dump_view(envelope)[0])
        except volatile.VolatileError:
            view = None  # never stable: a volatile field broke its rule
        return {"source": rename.get(upstream, upstream), "vosu": rename.get(vosu, vosu)}, view

    def _holds(self, barrier: str, seen: dict[str, str | None]) -> bool:
        """Both daemons report the barrier, and the attested client is alive in a client
        barrier and its exit was observed for no-client (unapproved clients raise)."""
        exited = self.client.exited()
        return exited == (barrier == "no-client") and all(value == barrier for value in seen.values())

    def _wait_barrier(self, barrier: str, source: Daemon, dump: HarnessDump,
                      reached: list[str], *, identical_views: bool = True) -> None:
        index = TRAVERSAL.index(barrier)
        self.operator(barrier, f"waiting for barrier {barrier}: {OPERATOR_STEPS[barrier]}")
        stable, previous = 0, None
        while stable < STABLE_OBSERVATIONS:
            if time.monotonic() > self.deadline:
                raise CaptureError(f"barrier {barrier} was not reached before the timeout")
            seen, view = self._observe(source, dump)
            for daemon, value in seen.items():
                if value not in TRAVERSAL or TRAVERSAL.index(value) <= index:
                    continue
                if value == "no-client" and not reached:
                    continue  # The client is still starting.
                raise CaptureError(f"barrier out of order: {daemon} reached {value} while "
                                   f"waiting for {barrier} after {reached or ['launch']}")
            if not self._holds(barrier, seen) or (identical_views and view is None):
                stable, previous = 0, None
            elif not identical_views:
                stable += 1
            else:
                stable = stable + 1 if stable and view == previous else 1
                previous = view
            if stable < STABLE_OBSERVATIONS:
                time.sleep(POLL_S)
        reached.append(barrier)
        self.transcript["barriers"].append({"barrier": barrier, "monotonic_ns": time.monotonic_ns(),
                                            "stable_observations": STABLE_OBSERVATIONS,
                                            "identical_views": identical_views})

    # -- cases

    def _attempt(self, item: rm.RenderedCase, number: int, failed: list[dict[str, Any]],
                 source: Daemon, vosu: Daemon, dump: HarnessDump) -> dict[str, Any] | str:
        case = item.case
        schema = dump_schema(case)
        raw: dict[str, Any] = {"case": case.id, "attempt": number}
        self.transcript["cases"].append(raw)
        guard = self._observe(source, dump)[0]
        raw["barrier_before"] = guard
        if not self._holds(case.barrier, guard):
            return f"barrier {case.barrier} not held before the attempt: {guard}"
        upstream = pr.run_identical(item, [source.endpoint, source.endpoint], ledger=self.ledger)
        before = dump.fetch(schema)
        route = pr.run_identical(item, [vosu.endpoint], ledger=self.ledger)[0]
        after = dump.fetch(schema)
        guard = self._observe(source, dump)[0]
        raw.update(source=upstream, vosu=route, dump_before=before, dump_after=after,
                   barrier_after=guard)
        if not self._holds(case.barrier, guard):
            return f"barrier {case.barrier} not held after the attempt: {guard}"
        for record in (*upstream, route):
            if not record["outcome_allowed"]:
                return f"{record['daemon']} outcome {record['outcome']!r} is not allowed"
        try:
            records = [golden_record(record, number, case.family) for record in upstream]
            vosu_record = golden_record(route, number, case.family)
        except CaptureError as exc:
            return str(exc)
        if _comparable(records[0]) != _comparable(records[1]):
            return "upstream sandwich responses differ"
        differs = dump_sandwich_difference(before, after)
        if differs is not None:
            return f"vosu dump sandwich differs in {differs}"
        validator = case.validator.response_header if case.validator else None
        attempt = {"attempt": number, "outcome": "complete", "source": records,
                   "vosu": {"dump_before": {"attempt": number, "envelope": before},
                            "route": vosu_record,
                            "dump_after": {"attempt": number, "envelope": after}}}
        value = {"id": case.id, "family": case.family, "barrier": case.barrier,
                 "dump_schema": schema, "conditional": validator,
                 "attempts": [*failed, attempt]}
        try:  # The same validators `golden` applies, so a recorded case is accepted unchanged.
            parity_golden._check_sandwiches(parity_golden._case(copy.deepcopy(value), 0))
        except GoldenError as exc:
            return str(exc)
        finding = rm.handshake_only_finding(case, upstream[0]["outcome"], route["outcome"])
        if finding is not None:
            self.transcript["findings"].append({**finding, "attempt": number})
        return value

    def _run_case(self, item: rm.RenderedCase, source: Daemon, vosu: Daemon,
                  dump: HarnessDump) -> dict[str, Any]:
        failed: list[dict[str, Any]] = []
        for number in range(1, MAX_ATTEMPTS + 1):
            result = self._attempt(item, number, failed, source, vosu, dump)
            if isinstance(result, dict):
                return result
            failed.append({"attempt": number, "outcome": "failed", "reason": result})
            print(f"golden-capture: {item.case.id} attempt {number} failed: {result}",
                  file=sys.stderr, flush=True)
        raise CaptureError(f"case {item.case.id} failed {MAX_ATTEMPTS} whole-case attempts: "
                           + "; ".join(entry["reason"] for entry in failed))

    # -- whole run

    def _identity(self, build: dict[str, Any]) -> dict[str, Any]:
        source = self.cfg["_artifact_identity"]["source"]
        if self.cfg["source_mode"] == "node-build":
            manifest = json.dumps(build.get("source_dist_manifest"), sort_keys=True,
                                  separators=(",", ":")).encode("utf-8")
            source_record = {"revision": PINNED_SOURCE_REVISION, "kind": "source",
                             "sha256": hashlib.sha256(manifest).hexdigest()}
        else:
            source_record = {"revision": PINNED_SOURCE_REVISION, "kind": "native",
                             "sha256": source["sha256"]}
        return {"source": source_record, "vosu": build["vosu"], "client": self._client_attestation()}

    def _client_attestation(self) -> dict[str, Any]:
        client = self.cfg["_artifact_identity"]["client"]
        return {"variant": self.cfg["client"].lower(), "version": client["version"],
                "sha256": client["sha256"]}

    def _assets(self) -> list[dict[str, Any]]:
        root = Path(self.golden_cfg["capture_root"]).resolve(strict=True)
        assets = []
        for asset in self.golden_cfg["assets"]:
            path = (root / asset["relative"]).resolve(strict=True)
            if not path.is_relative_to(root) or not path.is_file():
                raise CaptureError(f"asset {asset['id']} is not a file inside the capture root")
            data = path.read_bytes()
            assets.append({"id": asset["id"], "slot": self.golden_cfg["capture_slot"],
                           "relative": asset["relative"], "media_type": asset["media_type"],
                           "generator": asset["generator"], "license": asset["license"],
                           "sha256": hashlib.sha256(data).hexdigest(),
                           "b64": base64.b64encode(data).decode("ascii")})
        return assets

    def run(self) -> dict[str, Any]:
        self._authorize(None)
        self.transcript["volatile_ledger"] = volatile.ledger()
        self.transcript["client_root"] = check_client_root(self.cfg)
        build = self.build(self.cfg, str(self.out))
        self.transcript["build"] = build
        identity = self._identity(build)
        self._authorize(identity["vosu"]["commit"])
        context = self._prepare_layout()
        golden, others, excluded = plan_cases(self.scenario, context)
        self.transcript["excluded"] = excluded
        source, vosu = self._launch()
        self.transcript["launch"] = {"source": source.evidence, "vosu": vosu.evidence}
        dump = HarnessDump(vosu, self.capability)
        self.transcript["harness_endpoint"] = dump.verify_mounted()
        self.transcript["client"] = self.client.start()
        self.client_started = True
        cases: list[dict[str, Any]] = []
        reached: list[str] = []
        crashing = [item for item in golden if rm.upstream_crash_ledger(item.case)]
        for barrier in self.scenario.barriers:
            has_cases = any(item.case.barrier == barrier for item in (*golden, *others))
            self._wait_barrier(barrier, source, dump, reached, identical_views=has_cases)
            for item in golden:
                if item.case.barrier == barrier and item not in crashing:
                    cases.append(self._run_case(item, source, vosu, dump))
            for item in others:
                if item.case.barrier == barrier:
                    records = pr.run_identical(item, [source.endpoint, vosu.endpoint],
                                               ledger=self.ledger)
                    self.transcript["diagnostics"].append({
                        "case": item.case.id, "role": item.case.role, "records": records,
                        "local_failures": [f for r in records
                                           for f in pr.local_expectation_failures(item.case, r)]})
            self.operator(barrier, f"barrier {barrier} recorded")
        if crashing:  # Last: no barrier is left for a crashed upstream to abort.
            self.operator("no-client", f"running {len(crashing)} upstream_crashed cases")
            cases += crash.run_cases(self, crashing, source, vosu, dump)
        expected = [item.case.id for item in golden]
        check_expected_cases(expected, self.scenario, context)
        recorded = {case["id"]: case for case in cases}
        if len(recorded) != len(cases) or sorted(recorded) != sorted(expected):
            raise CaptureError("recorded cases differ from expected_cases")
        cases = [recorded[case_id] for case_id in expected]
        root = self.golden_cfg["capture_root"]
        separator = "\\" if self.cfg["os"].lower() == "windows" else "/"
        capture = {
            "format": CAPTURE_FORMAT, "source_revision": PINNED_SOURCE_REVISION,
            "platform": self.platform, "scenario_set": self.scenario.id,
            "authorization": dict(self.golden_cfg["authorization"]),
            "attestation": identity,
            "roots": [{"slot": self.golden_cfg["capture_slot"],
                       "private": root.rstrip(separator) + separator}],
            "assets": self._assets(), "expected_cases": expected, "cases": cases,
        }
        try:
            parity_golden._platform_files(json.loads(json.dumps(capture)), None, "captured")
        except GoldenError as exc:
            raise CaptureError(f"golden would reject this capture: {exc}") from exc
        return capture

    def cleanup(self) -> dict[str, Any]:
        """Stop every owned process, then remove every owned file; report survivors.

        Every step runs even when an earlier one fails; failures become errors."""
        errors: list[str] = []

        def step(label: str, action: Callable[[], Any]) -> Any:
            try:
                return action()
            except Exception as exc:  # Cleanup must reach every owned resource.
                errors.append(f"{label}: {type(exc).__name__}: {exc}")
                return None

        if self.client_started:
            errors += step("client", self.client.stop) or []
            self.transcript["shared_profile"] = getattr(self.client, "shared_profile", None)
        groups = {name: process.pid for name, process in self.processes.items()}
        # Record each daemon's process tree while it still runs: a child that outlives
        # its daemon is reparented and only this record still ties it to the run.
        table = step("process table", _process_table) or {}
        trees = {name: descendants(table, {process.pid: int(self.starts[name])})
                 for name, process in self.processes.items() if name in self.starts}
        for name, process in self.processes.items():
            step(f"{name} daemon", lambda process=process: daemon_mod._signal_process_tree(process))
        for name, process in self.processes.items():
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                step(f"{name} daemon", lambda process=process: (
                    daemon_mod._signal_process_tree(process, hard=True), process.wait(timeout=5)))
        for name, pgid in groups.items():
            tree = trees.get(name, {})
            if not _group_gone(pgid) or step(f"{name} process tree",
                                             lambda tree=tree: _tree_survivors(tree)):
                errors.append(f"orphan process survived {name} daemon cleanup")
        for name, process in self.processes.items():
            runtime_dir = getattr(process, "_live_parity_runtime_dir", None)
            if isinstance(runtime_dir, Path):
                step(f"{name} runtime directory", lambda path=runtime_dir: shutil.rmtree(path))
            config_path = getattr(process, "_live_parity_seeded_config", None)
            if isinstance(config_path, Path):
                step(f"{name} seeded configuration",
                     lambda path=config_path: path.unlink(missing_ok=True))
        if self.layout is not None:
            step("path-security layout", lambda: rs.remove_security_layout(self.layout))
        build = self.transcript.get("build") or {}
        for tree in build.get("owned_worktrees", []):
            step("source worktree", lambda tree=tree: _remove_worktree(tree["repository"], tree["path"]))
        for owned in build.get("owned_paths", []):
            if Path(owned).exists():
                step("capture build output", lambda path=owned: shutil.rmtree(path))
        vosu_local = getattr(self, "vosu_local", None)
        if isinstance(vosu_local, Path) and vosu_local.exists():
            step("vosu local data", lambda: shutil.rmtree(vosu_local))
        parents = [*build.get("owned_parents", []), *(
            str(Path(p).parent) for p in [vosu_local, *(getattr(process, "_live_parity_runtime_dir", None)
                                                     for process in self.processes.values())]
            if isinstance(p, Path) and Path(p).parent.name == "vosu-live-parity")]
        for parent in dict.fromkeys(parents):
            step("empty run parent", lambda path=Path(parent): path.is_dir() and not any(
                path.iterdir()) and path.rmdir())
        report = {"errors": errors, "daemons": sorted(groups)}
        self.transcript["cleanup"] = report
        return report


def _process_table() -> dict[int, tuple[int, int]]:
    """pid -> (parent pid, creation identity) for every live process."""
    if sys.platform == "win32":
        from parity_windows_job import windows_process_snapshot
        return {pid: (record["parent_pid"], int(record["start_identity"]))
                for pid, record in windows_process_snapshot().items()}
    table = {}
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            stat = (proc / "stat").read_text(encoding="ascii")
        except OSError:
            continue
        fields = stat[stat.rfind(")") + 2:].split()
        if fields[0] != "Z":
            table[int(proc.name)] = (int(fields[1]), int(fields[19]))
    return table


def descendants(table: dict[int, tuple[int, int]], known: dict[int, int]) -> dict[int, int]:
    """`known` (pid -> creation identity) plus every process descending from it. A
    child created before its recorded parent is a reused PID, never a descendant."""
    found = dict(known)
    grew = True
    while grew:
        grew = False
        for pid, (parent, created) in table.items():
            if pid not in found and parent in found and created >= found[parent]:
                found[pid] = created
                grew = True
    return found


def _alive(pid: int, created: int) -> bool:
    return _process_alive(pid, str(created))


def _kill_tree(tree: dict[int, int]) -> None:
    for pid, created in tree.items():
        if _alive(pid, created):
            try:
                os.kill(pid, signal.SIGKILL if os.name == "posix" else signal.SIGTERM)
            except OSError:
                pass


def _tree_survivors(tree: dict[int, int]) -> dict[int, int]:
    """Members of a daemon's recorded process tree, and their descendants, still alive
    after the daemon stopped; they are killed once before they count as survivors."""
    deadline = time.monotonic() + 5
    while True:
        alive = {pid: created for pid, created in descendants(_process_table(), tree).items()
                 if _alive(pid, created)}
        if not alive:
            return {}
        if time.monotonic() > deadline:
            _kill_tree(alive)
            time.sleep(0.5)
            return {pid: created for pid, created in alive.items() if _alive(pid, created)}
        time.sleep(0.05)


def _group_members(pgid: int) -> list[int]:
    members = []
    for proc in Path("/proc").iterdir():
        if not proc.name.isdigit():
            continue
        try:
            stat = (proc / "stat").read_text(encoding="ascii")
        except OSError:
            continue
        fields = stat[stat.rfind(")") + 2:].split()
        if fields[0] != "Z" and int(fields[2]) == pgid:
            members.append(int(proc.name))
    return members


def _kill_group(pgid: int) -> None:
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def _group_gone(pgid: int) -> bool:
    """True once no live process remains in the daemon's Linux process group; the group
    is killed if a member ignored the polite signal. Windows has no process groups."""
    if not sys.platform.startswith("linux"):
        return True
    deadline = time.monotonic() + 5
    while _group_members(pgid):
        if time.monotonic() > deadline:
            _kill_group(pgid)
            time.sleep(0.5)
            return not _group_members(pgid)
        time.sleep(0.05)
    return True


def _write_private(path: Path, data: bytes) -> None:
    """Atomic 0600 write of a new private file; an existing file is never replaced."""
    temporary = path.with_name(f".{path.name}.{secrets.token_hex(4)}.tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        if path.exists():
            raise CaptureError(f"{path.name} appeared during the capture; refusing to replace it")
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()


def _serialize(value: Any) -> bytes:
    return (json.dumps(value, indent=1, ensure_ascii=False, sort_keys=False) + "\n").encode("utf-8")


def golden_capture(cfg: dict[str, Any], scenario: ScenarioSet, out: str, *,
                   rehearsal_receipt: str | None, timeout: float = 3600.0,
                   build: Callable[[dict[str, Any], str], dict[str, Any]] | None = None,
                   client: Client | None = None,
                   operator: Callable[[str, str], None] = stderr_operator) -> int:
    if "golden" not in cfg:
        raise CaptureError("the run configuration has no golden block")
    if not 0 < timeout < float("inf"):
        raise CaptureError("timeout must be finite and positive")
    if _elevated():
        raise CaptureError("golden-capture refuses an elevated or root process: run it with the "
                           "limited interactive token (verification.md, O0 pilot run)")
    out_path = Path(out).resolve()
    transcript_path = out_path.with_name(out_path.name + ".transcript.json")
    for path in (out_path, transcript_path):
        if path.exists():
            raise CaptureError(f"{path.name} exists; golden-capture never replaces evidence")
    if not out_path.parent.is_dir():
        raise CaptureError("the private capture directory does not exist")
    from parity_golden_rehearsal import check_receipt  # It builds on this module.
    receipt = check_receipt(rehearsal_receipt, cfg)
    if build is None:
        from parity_build import build_harness_artifacts
        build = build_harness_artifacts
    session = Session(cfg, scenario, out_path, timeout=timeout, build=build,
                      client=client or LiveClient(cfg), operator=operator)
    session.transcript["rehearsal_receipt"] = receipt
    capture: dict[str, Any] | None = None
    failure: BaseException | None = None
    try:
        capture = session.run()
    except BaseException as exc:  # Cleanup and the transcript run for every failure.
        failure = exc
    cleanup = session.cleanup()
    if cleanup["errors"] and failure is None:
        failure = CaptureError("; ".join(cleanup["errors"]))
    session.transcript["result"] = "complete" if failure is None else f"failed: {failure}"
    outputs = [(transcript_path, _serialize(session.transcript))]
    if failure is None and capture is not None:
        outputs.insert(0, (out_path, _serialize(capture)))
    capability = session.capability.encode("ascii")
    if any(capability in data for _path, data in outputs):
        # Raw evidence redacts the capability (raw-capability-redaction); a leaked value
        # makes the whole run unusable, so nothing is written.
        raise CaptureError("the adapter capture capability leaked into the recorded evidence; "
                           "nothing was written") from failure
    for path, data in outputs:
        _write_private(path, data)
    if failure is not None:
        if isinstance(failure, (CaptureError, ValueError, OSError)):
            raise CaptureError(f"{failure}; transcript: {transcript_path.name}") from failure
        raise failure
    assert capture is not None
    print(json.dumps({
        "capture": out_path.name, "transcript": transcript_path.name,
        "platform": capture["platform"], "scenario_set": scenario.id,
        "cases": len(capture["cases"]),
        "upstream_crashed": sum(case["attempts"][-1]["outcome"] == crash.OUTCOME
                                for case in capture["cases"]),
        "retried_cases": sum(len(case["attempts"]) > 1 for case in capture["cases"]),
        "findings": session.transcript["findings"],
        "vosu_sha256": capture["attestation"]["vosu"]["sha256"],
        "sha256": hashlib.sha256(outputs[0][1]).hexdigest(),
    }, sort_keys=True))
    return 0
