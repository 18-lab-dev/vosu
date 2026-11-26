"""Deterministic M4b raw route and WebSocket probe matrix (GOLDEN_CONTRACT.md §6, plan §3.7).

Cases are data: a stable `<family>.<slug>` ID, a stable-window barrier, a golden
role, prerequisites, exact request bytes after rendering and the allowed
transport outcomes. Rendering is a pure function of the case and the run's
MatrixContext, so the same bytes reach both daemons and every retry
(RequestLedger). The only per-daemon substitution is a declared conditional
validator taken from the same daemon's earlier response in that case.
"""
from __future__ import annotations

from dataclasses import dataclass, field
import base64
import hashlib
import re
from string import Template
from typing import Iterable, Mapping
from urllib.parse import quote

from route_security import (ROUTE_PREFIXES, SPELLINGS, SecurityAbort, SecurityLayout,
                            encode_relative, inside_absolute, outside_chart, security_target)

MATRIX_FORMAT = "vosu-route-matrix-v1"
FAMILIES = ("state-http", "ws", "files", "path-security", "calculation", "origin-peer")
BARRIERS = ("no-client", "menu", "paused", "result")
PLATFORMS = {"windows-stable": ("windows", "stable"), "windows-lazer": ("windows", "lazer"),
             "linux-lazer": ("linux", "lazer")}
GOLDEN, NEGATIVE, DIAGNOSTIC = "golden", "negative", "diagnostic"
HTTP_OUTCOMES = frozenset({"response", "closed", "truncated", "timeout", "protocol-error",
                           "over-budget", "validator-missing"})
WS_OUTCOMES = frozenset({"upgraded", "upgraded-timeout", "server-closed", "rejected", "closed",
                         "truncated", "timeout", "protocol-error", "over-budget"})
CASE_ID = re.compile(r"^(?P<family>[a-z-]+)\.[a-z0-9]+(-[a-z0-9]+)*(\.[a-z0-9]+(-[a-z0-9]+)*)*$")
OP_CONTINUATION, OP_TEXT, OP_BINARY, OP_CLOSE, OP_PING, OP_PONG = 0, 1, 2, 8, 9, 10
HOST = "127.0.0.1"
VALIDATOR = "validator"

# A4b.7: compared exactly after normalization; every other header, the reason
# phrase and header order stay in the private raw record.
COMPARED_HEADERS = frozenset({
    "content-type", "content-length", "cache-control", "etag", "last-modified",
    "accept-ranges", "content-range", "location", "content-security-policy",
    "x-content-type-options"})
WS_COMPARED_HEADERS = frozenset({
    "upgrade", "connection", "sec-websocket-accept", "sec-websocket-protocol",
    "sec-websocket-extensions", "sec-websocket-version"})

# Fixture placeholders: path values are plain relative '/'-separated paths
# encoded per segment; query values are encoded as one form value.
PATH_VALUES = frozenset({"songs_file", "songs_dir", "beatmap_relative", "skin_file"})
QUERY_VALUES = frozenset({"calc_relative"})
LAYOUT_VALUES = frozenset({"calc_absolute_inside", "calc_absolute_outside"})


@dataclass(frozen=True)
class Recv:
    """Read until `messages` complete data messages arrive or the timeout elapses."""
    messages: int = 1
    timeout_s: float = 3.0


@dataclass(frozen=True)
class Send:
    payload: bytes
    opcode: int = OP_TEXT


@dataclass(frozen=True)
class Close:
    code: int = 1000
    reason: bytes = b""
    timeout_s: float = 2.0


@dataclass(frozen=True)
class Hold:
    """Slow-reader diagnostic: read nothing for `seconds`."""
    seconds: float


WsStep = Recv | Send | Close | Hold


@dataclass(frozen=True)
class Validator:
    """Conditional header whose value is taken from this daemon's earlier response in the case."""
    request_header: str
    response_header: str


@dataclass(frozen=True)
class RouteCase:
    id: str
    barrier: str
    role: str
    method: str
    target: str = ""
    headers: tuple[tuple[str, str], ...] = ()
    hosts: tuple[str, ...] = (HOST,)
    body: bytes = b""
    script: tuple[WsStep, ...] | None = None
    requires: frozenset[str] = frozenset()
    outcomes: frozenset[str] = frozenset({"response"})
    ledger: tuple[str, ...] = ()
    security: tuple[str, str] | None = None
    validator: Validator | None = None
    vosu_status: int | None = None

    @property
    def family(self) -> str:
        return self.id.split(".", 1)[0]

    @property
    def websocket(self) -> bool:
        return self.script is not None


@dataclass(frozen=True)
class MatrixContext:
    platform: str
    values: Mapping[str, str] = field(default_factory=dict)
    security: SecurityLayout | None = None

    @property
    def os_name(self) -> str:
        return PLATFORMS[self.platform][0]

    @property
    def client(self) -> str:
        return PLATFORMS[self.platform][1]


@dataclass(frozen=True)
class RenderedCase:
    case: RouteCase
    request: bytes
    frames: tuple[bytes, ...]
    digest: str
    lexical_only: bool = False

    def with_validator(self, value: str) -> bytes:
        """The conditional request with this daemon's validator substituted."""
        if self.case.validator is None:
            raise ValueError(f"{self.case.id} declares no validator")
        if any(ord(ch) < 32 or ord(ch) > 126 for ch in value):
            raise ValueError("validator contains a control or non-ASCII byte")
        return self.request.replace(b"${" + VALIDATOR.encode() + b"}", value.encode("ascii"), 1)

    def priming_request(self) -> bytes:
        """The case's request with the validator header line removed."""
        validator = self.case.validator
        if validator is None:
            raise ValueError(f"{self.case.id} declares no validator")
        line = f"{validator.request_header}: ${{{VALIDATOR}}}\r\n".encode("latin-1")
        return self.request.replace(line, b"", 1)


class CaseUnavailable(ValueError):
    """A prerequisite of the case is not met in this run; it is excluded, never faked."""


class RequestLedger:
    """Rejects a retry or second daemon whose request bytes differ for the same case ID."""

    def __init__(self) -> None:
        self.digests: dict[str, str] = {}

    def admit(self, rendered: RenderedCase) -> None:
        previous = self.digests.setdefault(rendered.case.id, rendered.digest)
        if previous != rendered.digest:
            raise ValueError(f"request bytes changed for case {rendered.case.id}")


def _derive(case_id: str, label: str, length: int) -> bytes:
    return hashlib.sha256(f"{MATRIX_FORMAT}\0{case_id}\0{label}".encode()).digest()[:length]


def ws_key(case_id: str) -> str:
    return base64.b64encode(_derive(case_id, "key", 16)).decode("ascii")


def mask_frame(opcode: int, payload: bytes, mask: bytes, fin: bool = True) -> bytes:
    """Encode one client frame (always masked, RFC 6455 §5.3)."""
    first = (0x80 if fin else 0) | opcode
    length = len(payload)
    if length < 126:
        header = bytes((first, 0x80 | length))
    elif length < 65_536:
        header = bytes((first, 0x80 | 126)) + length.to_bytes(2, "big")
    else:
        header = bytes((first, 0x80 | 127)) + length.to_bytes(8, "big")
    repeated = (mask * (length // 4 + 1))[:length]
    masked = (int.from_bytes(payload, "big") ^ int.from_bytes(repeated, "big")).to_bytes(
        length, "big") if length else b""
    return header + mask + masked


def client_frame(case_id: str, label: str, opcode: int, payload: bytes) -> bytes:
    return mask_frame(opcode, payload, _derive(case_id, label, 4))


def split_headers(headers: Iterable[tuple[str, str]], *, websocket: bool
                  ) -> tuple[list[tuple[str, str]], list[tuple[str, str]]]:
    """Split ordered raw headers into (compared, private-only), preserving order and case."""
    compared, private = [], []
    for name, value in headers:
        lower = name.lower()
        if lower in COMPARED_HEADERS or lower.startswith("access-control-") or (
                websocket and lower in WS_COMPARED_HEADERS):
            compared.append((name, value))
        else:
            private.append((name, value))
    return compared, private


def requirements_unmet(case: RouteCase, context: MatrixContext) -> list[str]:
    missing = []
    for requirement in sorted(case.requires):
        kind, _, value = requirement.partition(":")
        if kind == "os" and context.os_name != value:
            missing.append(f"requires {value}")
        elif kind == "client" and context.client != value:
            missing.append(f"requires {value} client")
        elif kind == "security" and context.security is None:
            missing.append("requires a run-owned security layout")
    for name in sorted(Template(case.target).get_identifiers()):
        if name in LAYOUT_VALUES:
            if context.security is None:
                missing.append(f"{name} requires a run-owned security layout")
            elif name == "calc_absolute_inside" and name not in context.values:
                missing.append(f"fixture value {name} not provided")
            elif name == "calc_absolute_outside" and context.security.sentinel_chart is None:
                missing.append("no run-owned sentinel chart outside the declared roots")
        elif name not in context.values:
            missing.append(f"fixture value {name} not provided")
    if case.security is not None and context.security is not None:
        prefix, spelling = case.security
        if prefix not in context.security.route_roots:
            missing.append(f"no declared root for {prefix} on {context.platform}")
        elif spelling == "symlink" and prefix not in context.security.links:
            reason = context.security.link_errors.get(prefix, "not created")
            missing.append(f"symlink fixture unavailable: {reason}")
    return missing


def _target(case: RouteCase, context: MatrixContext) -> tuple[str, bool]:
    if case.security is not None:
        assert context.security is not None
        prefix, spelling = case.security
        return security_target(context.security, prefix, spelling)
    values: dict[str, str] = {}
    lexical_only = False
    for name in Template(case.target).get_identifiers():
        if name in PATH_VALUES:
            values[name] = encode_relative(context.values[name])
        elif name in QUERY_VALUES:
            values[name] = quote(context.values[name], safe="")
        elif name == "calc_absolute_inside":
            assert context.security is not None
            path, lexical_only = inside_absolute(context.security, context.values[name])
            values[name] = quote(path, safe="")
        elif name == "calc_absolute_outside":
            assert context.security is not None
            path, lexical_only = outside_chart(context.security)
            values[name] = quote(path, safe="")
        else:
            raise CaseUnavailable(f"unknown fixture value {name}")
    return Template(case.target).substitute(values), lexical_only


def render(case: RouteCase, context: MatrixContext) -> RenderedCase:
    """Exact request bytes for this run. Raises CaseUnavailable or SecurityAbort before sending."""
    unmet = requirements_unmet(case, context)
    if unmet:
        raise CaseUnavailable(f"{case.id}: " + "; ".join(unmet))
    target, lexical_only = _target(case, context)
    if not target.startswith("/") or any(ord(ch) <= 32 or ord(ch) > 126 for ch in target):
        raise ValueError(f"{case.id}: request target is not raw printable ASCII")
    lines = [f"{case.method} {target} HTTP/1.1"]
    lines += [f"Host: {host}" if host else "Host:" for host in case.hosts]
    for name, value in case.headers:
        if name.lower() == "sec-websocket-key" and value == "$ws_key":
            value = ws_key(case.id)
        lines.append(f"{name}: {value}" if value else f"{name}:")
    if case.validator is not None:
        lines.append(f"{case.validator.request_header}: ${{{VALIDATOR}}}")
    if not case.websocket and not any(name.lower() == "connection" for name, _ in case.headers):
        lines.append("Connection: close")
    if case.body:
        lines.append(f"Content-Length: {len(case.body)}")
    request = ("\r\n".join(lines) + "\r\n\r\n").encode("latin-1") + case.body
    frames = []
    for index, step in enumerate(case.script or ()):
        if isinstance(step, Send):
            frames.append(client_frame(case.id, f"step-{index}", step.opcode, step.payload))
        elif isinstance(step, Close):
            frames.append(client_frame(case.id, f"step-{index}", OP_CLOSE,
                                       step.code.to_bytes(2, "big") + step.reason))
    digest = hashlib.sha256()
    for part in (request, *frames):
        digest.update(len(part).to_bytes(8, "big") + part)
    return RenderedCase(case, request, tuple(frames), "sha256:" + digest.hexdigest(), lexical_only)


def select(cases: Iterable[RouteCase], context: MatrixContext
           ) -> tuple[list[RenderedCase], list[tuple[str, str]]]:
    """Render every applicable case; every other case is listed with its exclusion reason."""
    rendered, excluded = [], []
    for case in cases:
        try:
            rendered.append(render(case, context))
        except CaseUnavailable as exc:
            excluded.append((case.id, str(exc)))
    return rendered, excluded


def manifest(cases: Iterable[RouteCase] | None = None) -> dict:
    return {"format": MATRIX_FORMAT, "cases": [{
        "id": case.id, "family": case.family, "barrier": case.barrier, "role": case.role,
        "method": case.method, "transport": "websocket" if case.websocket else "http",
        "target": case.target or None,
        "security": None if case.security is None else list(case.security),
        "requires": sorted(case.requires), "outcomes": sorted(case.outcomes),
        "ledger": list(case.ledger), "validator": None if case.validator is None else [
            case.validator.request_header, case.validator.response_header],
    } for case in (MATRIX if cases is None else cases)]}


# --------------------------------------------------------------------------- matrix data

STATE_ROUTES = (("json", "/json"), ("json-sc", "/json/sc"), ("json-v2", "/json/v2"),
                ("json-v2-precise", "/json/v2/precise"))
WS_ROUTES = (("ws", "/ws"), ("tokens", "/tokens"), ("v2", "/websocket/v2"),
             ("v2-precise", "/websocket/v2/precise"))
PAYLOAD_BARRIERS = ("menu", "paused", "result")
UPGRADE = (("Upgrade", "websocket"), ("Connection", "Upgrade"),
           ("Sec-WebSocket-Key", "$ws_key"), ("Sec-WebSocket-Version", "13"))
CLOSED_OK = frozenset({"response", "closed"})
REJECTED = frozenset({"rejected", "closed"})
BUDGET = ("adapter-surface-request-budget",)
LISTENER = ("adapter-listener-policy",)
FILE_SANDBOX = ("legacy-game-file-sandbox",)
CALC = ("legacy-calculation-engine",)
UPGRADED = frozenset({"upgraded"})
# With no client a feed may send no state frame inside its Recv window (the pinned
# upstream sends none, rehearsal 2026-11-25): a `no-client` upgrade case is a
# handshake-only golden either way (GOLDEN_CONTRACT §6), and golden-capture lists a
# daemon pair that disagrees on the frame as a finding (`handshake_only_finding`).
HANDSHAKE_ONLY = UPGRADED | {"upgraded-timeout"}
# The pinned upstream answers a request target with a malformed percent escape, then its
# response logger throws and the daemon exits (parity_golden_crash). These cases run after
# the no-client barrier and record the `upstream_crashed` outcome.
UPSTREAM_CRASH_LEDGER = "upstream-malformed-uri-crash"


def upstream_crash_ledger(case: RouteCase) -> str | None:
    """The ledger row recording that this case's request kills the pinned upstream daemon."""
    return UPSTREAM_CRASH_LEDGER if UPSTREAM_CRASH_LEDGER in case.ledger else None


# Upstream outcomes the route matrix declares per case (Main's decisions of 2026-11-26,
# each case named by ID, never a class rule), each under its reviewed ledger row.
# `timeout`: the pinned upstream sends no byte before the case deadline, on every attempt
# and in both sandwich records (rehearsal and the first M4b full pilot run): its `upgrade`
# handler answers only the exact WebSocket paths it owns and leaves every other Upgrade
# unanswered, and its HTTP router's `new URL` throws on a Host with a non-numeric port
# before any response. `validator-missing`: upstream's file routes never send `ETag` or
# `Last-Modified` (no such code in the pinned source; second full pilot run), so a 2xx
# priming response without the validator is the complete observation. vosu must answer.
# `private-path-echo` (pre-run-3 audit): on Windows upstream joins an absolute drive or
# device spelling under its songs folder and answers 5xx echoing the joined private path
# in its status line and body. The record is an ordinary response; golden checks the echo
# structurally, never stores it, and asserts that vosu refuses cleanly (3xx/4xx, no path).
NEAR_UPGRADE_LEDGER = "upstream-unanswered-near-upgrade"
NO_VALIDATOR_LEDGER = "upstream-no-file-validators"
PATH_ECHO_LEDGER = "upstream-private-path-echo"
PATH_ECHO_SPELLINGS = ("absolute-drive", "absolute-drive-slash", "unc-extended", "unc-device")
UPSTREAM_EXPECTED = {
    "ws.near.v2-trailing-slash": ("timeout", NEAR_UPGRADE_LEDGER),
    "ws.near.v2-precise-trailing-slash": ("timeout", NEAR_UPGRADE_LEDGER),
    "ws.near.ws-trailing-slash": ("timeout", NEAR_UPGRADE_LEDGER),
    "ws.near.tokens-trailing-slash": ("timeout", NEAR_UPGRADE_LEDGER),
    "ws.near.v2-subscribe": ("timeout", "adr0021-removed-selected-v2-feed"),
    "files.skin.index.upgrade": ("timeout", "adr0021-removed-legacy-skin-index"),
    "origin-peer.host.malformed-port": ("timeout", "adapter-listener-policy"),
    "files.conditional.if-none-match-etag": ("validator-missing", NO_VALIDATOR_LEDGER),
    "files.conditional.if-modified-since-last-modified": ("validator-missing", NO_VALIDATOR_LEDGER),
    "files.conditional.if-range-etag": ("validator-missing", NO_VALIDATOR_LEDGER),
    "files.conditional.if-range-last-modified": ("validator-missing", NO_VALIDATOR_LEDGER),
    "path-security.songs.absolute-drive": ("private-path-echo", PATH_ECHO_LEDGER),
    "path-security.songs.absolute-drive-slash": ("private-path-echo", PATH_ECHO_LEDGER),
    "path-security.songs.unc-extended": ("private-path-echo", PATH_ECHO_LEDGER),
    "path-security.songs.unc-device": ("private-path-echo", PATH_ECHO_LEDGER),
    "path-security.beatmap.absolute-drive": ("private-path-echo", PATH_ECHO_LEDGER),
    "path-security.beatmap.absolute-drive-slash": ("private-path-echo", PATH_ECHO_LEDGER),
    "path-security.beatmap.unc-extended": ("private-path-echo", PATH_ECHO_LEDGER),
    "path-security.beatmap.unc-device": ("private-path-echo", PATH_ECHO_LEDGER),
}
# The raw route-record outcome each declared outcome is observed as.
RAW_OUTCOME = {"timeout": "timeout", "validator-missing": "validator-missing",
               "private-path-echo": "response"}


def upstream_expected(case_id: str) -> tuple[str, str] | None:
    """(upstream's declared outcome, its ledger row) for this case, or None."""
    return UPSTREAM_EXPECTED.get(case_id)


def allowed_outcomes(case: RouteCase, daemon: str) -> frozenset[str]:
    """What `daemon` may record for `case`: upstream must record a declared outcome."""
    expected = UPSTREAM_EXPECTED.get(case.id)
    return frozenset({RAW_OUTCOME[expected[0]]}) if daemon == "source" and expected \
        else case.outcomes


def handshake_only_finding(case: RouteCase, source: str, vosu: str) -> dict | None:
    """A completed handshake-only golden (`HANDSHAKE_ONLY`) whose upstream and vosu
    outcomes disagree on a state frame inside the window. Both records stay in the capture
    as observed; the disagreement is a finding for review, never normalized away."""
    if case.role != GOLDEN or "upgraded-timeout" not in case.outcomes:
        return None
    frame = {"upgraded": "state frame", "upgraded-timeout": "no frame"}
    if frame[source] == frame[vosu]:
        return None
    return {"case": case.id, "rule": "handshake-only", "source": frame[source], "vosu": frame[vosu]}


FILTER_V2 = b'["menu",{"field":"play","keys":["playerName",{"field":"hits","keys":["0","100","300"]}]}]'
FILTER_PRECISE = (b'["currentTime",{"field":"keys","keys":["k1",{"field":"k2","keys":'
                  b'["isPressed","count"]}]}]')
FILTER_TOKENS = b'["artistRoman","titleRoman","mapStrains"]'
# The adapters admit one 1 MiB message; 512 KiB is "oversized" yet within that limit.
OVERSIZED_FIELD = b"a" * (512 * 1024)
FUTURE_DATE = "Fri, 01 Jan 2100 00:00:00 GMT"
PAST_DATE = "Thu, 01 Jan 1970 00:00:00 GMT"
STATIC_TAG = '"vosu-route-probe"'


def _validator_headers(count: int) -> tuple[tuple[str, str], ...]:
    """`count` forwarded conditional headers: the surface MAX_HEADERS budget is 32."""
    return tuple(("If-None-Match", f'"vosu-route-probe-{index}"') for index in range(count))


def _http(case_id: str, method: str, target: str, *, barrier: str = "menu", role: str = GOLDEN,
          **kwargs) -> RouteCase:
    return RouteCase(case_id, barrier, role, method, target, **kwargs)


def _ws(case_id: str, target: str, script: tuple[WsStep, ...], *, barrier: str = "menu",
        headers: tuple[tuple[str, str], ...] = UPGRADE, method: str = "GET",
        outcomes: frozenset[str] = UPGRADED, role: str = GOLDEN, **kwargs) -> RouteCase:
    return RouteCase(case_id, barrier, role, method, target, headers, script=script,
                     outcomes=outcomes, **kwargs)


def _state_cases() -> list[RouteCase]:
    cases = []
    for slug, path in STATE_ROUTES:
        base = f"state-http.{slug}"
        for barrier in ("no-client", *PAYLOAD_BARRIERS):
            cases.append(_http(f"{base}.get.{barrier}", "GET", path, barrier=barrier))
            cases.append(_http(f"{base}.head.{barrier}", "HEAD", path, barrier=barrier))
        cases.append(_http(f"{base}.post", "POST", path, body=b"{}",
                           headers=(("Content-Type", "application/json"),)))
        for method in ("PUT", "DELETE", "OPTIONS"):
            cases.append(_http(f"{base}.{method.lower()}", method, path))
        near = {
            "trailing-slash": path + "/", "wrong-case": path.upper(), "suffix": path + "x",
            "double-slash": "/" + path, "malformed-percent": path + "%",
            "malformed-percent-hex": path + "%zz", "query": path + "?vosuProbe=1",
        }
        if path.count("/") > 1:
            near["inner-double-slash"] = path.replace("/", "//", 2)[1:]
        for name, target in near.items():
            crash = {"barrier": "no-client", "ledger": (UPSTREAM_CRASH_LEDGER,)} \
                if name.startswith("malformed-percent") else {}
            cases.append(_http(f"{base}.near.{name}", "GET", target, outcomes=CLOSED_OK, **crash))
    pairs = lambda count: "/json/v2?" + "&".join(f"p{index}=0" for index in range(count))
    # MAX_METADATA_BYTES counts decoded key+value bytes: "q" plus the value.
    query_bytes = lambda total: "/json/v2?q=" + "x" * (total - 1)
    cases += [
        _http("state-http.json-v2.budget.query-pairs-128", "GET", pairs(128), ledger=BUDGET),
        _http("state-http.json-v2.budget.query-pairs-129", "GET", pairs(129), ledger=BUDGET,
              outcomes=CLOSED_OK),
        _http("state-http.json-v2.budget.query-bytes-16384", "GET", query_bytes(16_384),
              ledger=BUDGET, outcomes=CLOSED_OK),
        _http("state-http.json-v2.budget.query-bytes-16385", "GET", query_bytes(16_385),
              ledger=BUDGET, outcomes=CLOSED_OK),
        _http("state-http.json-v2.budget.headers-32", "GET", "/json/v2",
              headers=_validator_headers(32), ledger=BUDGET),
        _http("state-http.json-v2.budget.headers-33", "GET", "/json/v2",
              headers=_validator_headers(33), ledger=BUDGET, outcomes=CLOSED_OK),
    ]
    # Vosu-only surfaces: negative near misses only, never upstream goldens.
    for slug, method, target in (("api-v1-suffix", "GET", "/api/v1x"),
                                 ("dashboard-suffix", "GET", "/api/dashboardx"),
                                 ("tools-post", "POST", "/tools"),
                                 ("commands-suffix", "GET", "/websocket/commandsx")):
        cases.append(_http(f"state-http.negative.{slug}", method, target, role=NEGATIVE,
                           vosu_status=404, outcomes=CLOSED_OK))
    return cases


def _ws_cases() -> list[RouteCase]:
    cases = []
    first = (Recv(1), Close())
    for slug, path in WS_ROUTES:
        base = f"ws.{slug}"
        for barrier in ("no-client", *PAYLOAD_BARRIERS):
            cases.append(_ws(f"{base}.upgrade.{barrier}", path, first, barrier=barrier,
                             outcomes=HANDSHAKE_ONLY if barrier == "no-client" else UPGRADED))
        drop = lambda name: tuple(item for item in UPGRADE if item[0] != name)
        invalid = {
            "missing-key": drop("Sec-WebSocket-Key"),
            "wrong-version": drop("Sec-WebSocket-Version") + (("Sec-WebSocket-Version", "8"),),
            "missing-upgrade": drop("Upgrade"), "missing-connection": drop("Connection"),
        }
        for name, headers in invalid.items():
            cases.append(_ws(f"{base}.invalid.{name}", path, first, headers=headers,
                             outcomes=REJECTED | UPGRADED))
        cases.append(_ws(f"{base}.invalid.post-upgrade", path, first, method="POST",
                         outcomes=REJECTED | UPGRADED))
        cases.append(_http(f"{base}.invalid.get-plain", "GET", path, outcomes=CLOSED_OK))
    for name, path in (("v2-trailing-slash", "/websocket/v2/"),
                       ("v2-precise-trailing-slash", "/websocket/v2/precise/"),
                       ("ws-trailing-slash", "/ws/"), ("tokens-trailing-slash", "/tokens/")):
        cases.append(_ws(f"ws.near.{name}", path, first, outcomes=REJECTED | UPGRADED,
                         ledger=(NEAR_UPGRADE_LEDGER,)))
    subscribe = ("adr0021-removed-selected-v2-feed",)
    cases += [
        _ws("ws.near.v2-subscribe", "/websocket/v2/subscribe", first,
            outcomes=REJECTED | UPGRADED, ledger=subscribe),
        _http("ws.near.v2-subscribe-plain", "GET", "/websocket/v2/subscribe", ledger=subscribe,
              outcomes=CLOSED_OK),
    ]
    oversized = lambda prefix: prefix + b'["' + OVERSIZED_FIELD + b'"]'
    for slug, path, nested, envelope in (
            ("v2", "/websocket/v2", FILTER_V2, b"applyFilters:"),
            ("v2-precise", "/websocket/v2/precise", FILTER_PRECISE, b"applyFilters:"),
            ("tokens", "/tokens", FILTER_TOKENS, b"")):
        base = f"ws.{slug}.filter"
        message = envelope + nested
        duplicate = envelope + (b'["menu","menu"]' if envelope else b'["artistRoman","artistRoman"]')
        scripts = {
            "nested": (Recv(1), Send(message), Recv(1), Close()),
            "repeated-message": (Recv(1), Send(message), Send(message), Recv(1), Close()),
            "duplicate-keys": (Recv(1), Send(duplicate), Recv(1), Close()),
            "invalid-json": (Recv(1), Send(envelope + b"["), Recv(1), Close()),
            "oversized": (Recv(1), Send(oversized(envelope)), Recv(1), Close()),
        }
        for name, script in scripts.items():
            cases.append(_ws(f"{base}.{name}", path, script, barrier="paused",
                             outcomes=UPGRADED | {"server-closed"}))
    cases += [
        _ws("ws.v2.diagnostic.close-normal", "/websocket/v2", (Close(),), role=DIAGNOSTIC,
            outcomes=UPGRADED | {"server-closed"}),
        _ws("ws.v2.diagnostic.slow-reader", "/websocket/v2", (Hold(3.0), Recv(8), Close()),
            role=DIAGNOSTIC, outcomes=UPGRADED | {"upgraded-timeout", "server-closed"}),
        _ws("ws.v2.diagnostic.feed", "/websocket/v2", (Recv(16, 10.0), Close()),
            barrier="menu", role=DIAGNOSTIC, outcomes=UPGRADED | {"upgraded-timeout"}),
    ]
    return cases


def _file_cases() -> list[RouteCase]:
    cases = []
    for name in ("background", "audio", "file"):
        for method in ("GET", "HEAD"):
            cases.append(_http(f"files.beatmap.{name}.{method.lower()}", method,
                               f"/files/beatmap/{name}", ledger=FILE_SANDBOX))
    sandboxed = [
        ("files.beatmap.file.post", "POST", "/files/beatmap/file", {}),
        ("files.beatmap.relative.get", "GET", "/files/beatmap/${beatmap_relative}", {}),
        ("files.beatmap.relative.head", "HEAD", "/files/beatmap/${beatmap_relative}", {}),
        ("files.songs.file.get", "GET", "/Songs/${songs_file}", {}),
        ("files.songs.file.head", "HEAD", "/Songs/${songs_file}", {}),
        ("files.songs.dir.get", "GET", "/Songs/${songs_dir}/", {}),
        ("files.songs.dir-no-slash.get", "GET", "/Songs/${songs_dir}", {}),
        ("files.background-image.get", "GET", "/backgroundImage", {}),
        ("files.background-image.head", "HEAD", "/backgroundImage", {}),
        ("files.skin.file.get", "GET", "/files/skin/${skin_file}",
         {"requires": frozenset({"client:stable"})}),
        ("files.skin.file.head", "HEAD", "/files/skin/${skin_file}",
         {"requires": frozenset({"client:stable"})}),
        ("files.skin.lazer-file.get", "GET", "/files/skin/${skin_file}",
         {"requires": frozenset({"client:lazer"}),
          "ledger": FILE_SANDBOX + ("lazer-legacy-skin-404",)}),
        ("files.skin.lazer-file.head", "HEAD", "/files/skin/${skin_file}",
         {"requires": frozenset({"client:lazer"}),
          "ledger": FILE_SANDBOX + ("lazer-legacy-skin-404",)}),
    ]
    for case_id, method, target, extra in sandboxed:
        extra.setdefault("ledger", FILE_SANDBOX)
        cases.append(_http(case_id, method, target, **extra))
    index = ("adr0021-removed-legacy-skin-index",)
    cases += [
        _http("files.skin.index.get", "GET", "/files/skin/", ledger=index),
        _http("files.skin.index.head", "HEAD", "/files/skin/", ledger=index),
        _ws("files.skin.index.upgrade", "/files/skin/", (Recv(1), Close()), ledger=index,
            outcomes=REJECTED | UPGRADED),
    ]
    audio, file = "/files/beatmap/audio", "/files/beatmap/file"
    headed = [
        ("files.range.single", "GET", audio, (("Range", "bytes=0-15"),), None),
        ("files.range.single-head", "HEAD", audio, (("Range", "bytes=0-15"),), None),
        ("files.range.unsatisfiable", "GET", audio, (("Range", "bytes=1099511627776-"),), None),
        ("files.range.multiple", "GET", audio, (("Range", "bytes=0-1,4-5"),), None),
        ("files.conditional.if-none-match-star", "GET", file, (("If-None-Match", "*"),), None),
        ("files.conditional.if-none-match-other", "GET", file,
         (("If-None-Match", STATIC_TAG),), None),
        ("files.conditional.if-none-match-etag", "GET", file, (),
         Validator("If-None-Match", "etag")),
        ("files.conditional.if-modified-since-future", "GET", file,
         (("If-Modified-Since", FUTURE_DATE),), None),
        ("files.conditional.if-modified-since-past", "GET", file,
         (("If-Modified-Since", PAST_DATE),), None),
        ("files.conditional.if-modified-since-last-modified", "GET", file, (),
         Validator("If-Modified-Since", "last-modified")),
        ("files.conditional.if-range-other", "GET", audio,
         (("Range", "bytes=0-15"), ("If-Range", STATIC_TAG)), None),
        ("files.conditional.if-range-past", "GET", audio,
         (("Range", "bytes=0-15"), ("If-Range", PAST_DATE)), None),
        ("files.conditional.if-range-etag", "GET", audio, (("Range", "bytes=0-15"),),
         Validator("If-Range", "etag")),
        ("files.conditional.if-range-last-modified", "GET", audio, (("Range", "bytes=0-15"),),
         Validator("If-Range", "last-modified")),
    ]
    for case_id, method, target, headers, validator in headed:
        upstream = (NO_VALIDATOR_LEDGER,) if validator else ()
        cases.append(_http(case_id, method, target, headers=headers, validator=validator,
                           ledger=FILE_SANDBOX + upstream))
    cases += [
        _http("files.budget.headers-32", "GET", file, headers=_validator_headers(32),
              ledger=BUDGET),
        _http("files.budget.headers-33", "GET", file, headers=_validator_headers(33),
              ledger=BUDGET, outcomes=CLOSED_OK),
        _http("files.budget.range-control-byte", "GET", audio,
              headers=(("Range", "bytes=0-1\x7f"),), ledger=BUDGET, outcomes=CLOSED_OK),
    ]
    return cases


def _security_cases() -> list[RouteCase]:
    slugs = {"/Songs/": "songs", "/files/beatmap/": "beatmap", "/files/skin/": "skin"}
    cases = []
    for prefix in ROUTE_PREFIXES:
        for spelling in SPELLINGS:
            requires = {"security"}
            if len(spelling.oses) == 1:
                requires.add("os:" + next(iter(spelling.oses)))
            echo = (PATH_ECHO_LEDGER,) if prefix != "/files/skin/" \
                and spelling.name in PATH_ECHO_SPELLINGS else ()
            cases.append(_http(
                f"path-security.{slugs[prefix]}.{spelling.name}", "GET", "",
                requires=frozenset(requires), security=(prefix, spelling.name),
                ledger=("legacy-game-file-confinement", *echo), outcomes=CLOSED_OK))
    return cases


def _calculation_cases() -> list[RouteCase]:
    base = "/api/calculate/pp"
    mods_json = quote('[{"acronym":"DT","settings":{"speed_change":1.25}}]', safe="")
    contracts = {
        **dict.fromkeys(("mods-acronyms", "mods-json"), ("legacy-calculate-mod-inputs",)),
        **dict.fromkeys(
            ("invalid-accuracy", "invalid-clock-rate", "mods-invalid", "invalid-count", "invalid-mode"),
            ("legacy-calculate-client-errors",),
        ),
    }
    queries = {
        "no-query": "", "mods-bits": "?mods=72", "mods-acronyms": "?mods=HDDT",
        "mods-json": "?mods=" + mods_json, "mods-invalid": "?mods=H",
        "accuracy": "?acc=98.5", "combo": "?combo=100&nMisses=2",
        "statistics": "?n300=100&n100=5&n50=1&nMisses=0&nGeki=10&nKatu=2",
        "lazer-statistics": "?lazer=true&sliderEndHits=3&smallTickHits=2&largeTickHits=1",
        "attributes": "?mode=0&ar=9.5&cs=4&hp=6&od=8&clockRate=1.5&passedObjects=10",
        "invalid-accuracy": "?acc=abc", "invalid-count": "?combo=-1",
        "invalid-mode": "?mode=9", "invalid-clock-rate": "?clockRate=0",
        "relative-path": "?path=${calc_relative}",
        "absolute-inside-root": "?path=${calc_absolute_inside}",
    }
    cases = [_http(f"calculation.{name}", "GET", base + query, ledger=CALC + contracts.get(name, ()) + (
        ("legacy-calculate-declared-roots",) if "path=" in query else ()))
        for name, query in queries.items()]
    cases += [
        _http("calculation.absolute-outside-root", "GET",
              base + "?path=${calc_absolute_outside}", requires=frozenset({"security"}),
              ledger=CALC + ("legacy-calculate-declared-roots",)),
        _http("calculation.no-query.no-client", "GET", base, barrier="no-client", ledger=CALC),
        _http("calculation.post", "POST", base, ledger=CALC),
    ]
    return cases


def _origin_cases() -> list[RouteCase]:
    origins = {"none": None, "loopback-ip": "http://127.0.0.1", "localhost": "http://localhost",
               "foreign": "https://example.invalid", "null": "null"}
    cases = []
    for name, origin in origins.items():
        headers = () if origin is None else (("Origin", origin),)
        cases.append(_http(f"origin-peer.http.{name}", "GET", "/json/v2", headers=headers,
                           ledger=LISTENER))
        cases.append(_ws(f"origin-peer.ws.{name}", "/websocket/v2", (Recv(1), Close()),
                         headers=UPGRADE + headers, ledger=LISTENER,
                         outcomes=UPGRADED | REJECTED))
    cases += [
        _http("origin-peer.http.preflight", "OPTIONS", "/json/v2", ledger=LISTENER,
              headers=(("Origin", "https://example.invalid"),
                       ("Access-Control-Request-Method", "GET"))),
        _http("origin-peer.host.malformed-port", "GET", "/json/v2", hosts=("127.0.0.1:notaport",),
              ledger=LISTENER, outcomes=CLOSED_OK),
        _http("origin-peer.host.empty", "GET", "/json/v2", hosts=("",), ledger=LISTENER,
              outcomes=CLOSED_OK),
        _http("origin-peer.host.duplicate", "GET", "/json/v2", hosts=(HOST, "localhost"),
              ledger=LISTENER, outcomes=CLOSED_OK),
        _http("origin-peer.host.missing", "GET", "/json/v2", hosts=(), ledger=LISTENER,
              outcomes=CLOSED_OK),
        _http("origin-peer.host.foreign", "GET", "/json/v2", hosts=("example.invalid",),
              ledger=LISTENER, outcomes=CLOSED_OK),
    ]
    return cases


def _build() -> tuple[RouteCase, ...]:
    cases = (*_state_cases(), *_ws_cases(), *_file_cases(), *_security_cases(),
             *_calculation_cases(), *_origin_cases())
    seen: set[str] = set()
    for case in cases:
        match = CASE_ID.match(case.id)
        if not match or match["family"] not in FAMILIES or case.id in seen:
            raise ValueError(f"invalid or duplicate case ID {case.id}")
        if case.barrier not in BARRIERS or case.role not in (GOLDEN, NEGATIVE, DIAGNOSTIC):
            raise ValueError(f"invalid barrier or role for {case.id}")
        allowed = WS_OUTCOMES if case.websocket else HTTP_OUTCOMES
        if not case.outcomes or not case.outcomes <= allowed:
            raise ValueError(f"invalid outcomes for {case.id}")
        if "/api/ingame" in case.target.lower():
            raise ValueError("/api/ingame is excluded from M4b (GOLDEN_CONTRACT.md §8)")
        if any(name.lower() == "x-vosu-test-capture" for name, _ in case.headers):
            raise ValueError("route cases never carry the harness capability (§7)")
        if upstream_crash_ledger(case) and (case.websocket or case.validator or case.role != GOLDEN
                                            or case.barrier != "no-client"):
            raise ValueError(f"{case.id}: an upstream-crashing case is a golden plain HTTP "
                             "request run after the no-client barrier")
        expected = upstream_expected(case.id)
        if expected is not None and (
                expected[1] not in case.ledger or case.role != GOLDEN or upstream_crash_ledger(case)
                or (case.validator is not None) != (expected[0] == "validator-missing")):
            raise ValueError(f"{case.id}: a declared upstream {expected[0]} is a golden case under "
                             f"its ledger row {expected[1]}, with a validator only for "
                             "validator-missing")
        seen.add(case.id)
    if not seen >= set(UPSTREAM_EXPECTED):
        raise ValueError(f"unknown declared cases {sorted(set(UPSTREAM_EXPECTED) - seen)}")
    return cases


MATRIX = _build()
CASES = {case.id: case for case in MATRIX}


def golden_eligible(case: RouteCase) -> bool:
    """Only upstream records of golden-role cases may become expected values."""
    return case.role == GOLDEN


__all__ = [
    "BARRIERS", "CASES", "COMPARED_HEADERS", "CaseUnavailable", "Close", "FAMILIES", "Hold",
    "MATRIX", "MatrixContext", "PLATFORMS", "Recv", "RenderedCase", "RequestLedger", "RouteCase",
    "SecurityAbort", "Send", "UPSTREAM_CRASH_LEDGER", "UPSTREAM_EXPECTED", "Validator",
    "WS_COMPARED_HEADERS", "allowed_outcomes", "client_frame", "golden_eligible", "manifest",
    "mask_frame", "render", "requirements_unmet", "select", "split_headers",
    "upstream_crash_ledger", "upstream_expected", "ws_key",
]
