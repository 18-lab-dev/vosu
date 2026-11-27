"""Fake daemons for test_live_parity_golden_capture.py; never a source of goldens.

`upstream` stands in for the pinned upstream daemon. `vosu` stands in for a
test-harness vosu: its adapter capture endpoint serves real envelopes exported by
the C0 encoder (golden-capture-envelopes.json), in read order per barrier and
schema, and its state routes serve the envelope's adapter bytes. The test switches
the client barrier through the JSON file in GOLDEN_FAKE_STATE. GOLDEN_FAKE_OPTIONS
selects counterexample behavior.
"""
from __future__ import annotations

import base64
import email.utils
import hashlib
import json
import os
from pathlib import Path
import socket
import socketserver
import subprocess
import sys
import threading
import urllib.parse

MODE = sys.argv[1]
PORT = int(os.environ["SERVER_PORT"])
STATE = Path(os.environ["GOLDEN_FAKE_STATE"])
OPTIONS = json.loads(os.environ.get("GOLDEN_FAKE_OPTIONS", "{}"))
CAPABILITY = os.environ.get("VOSU_TEST_ADAPTER_CAPTURE")
ENDPOINT = "/__vosu_test/v1/adapter-capture"
NOT_READY = b'{"error":"osu is not ready/running"}'
STATE_ROUTES = {"/json": "v1", "/json/sc": "sc", "/json/v2": "v2", "/json/v2/precise": "v2-precise"}
WS_ROUTES = {"/ws": "v1", "/tokens": "sc", "/websocket/v2": "v2", "/websocket/v2/precise": "v2-precise"}
GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
ENVELOPES = (json.loads(Path(os.environ["GOLDEN_FAKE_ENVELOPES"]).read_text("utf-8"))["barriers"]
             if MODE == "vosu" else {})
LOCK = threading.Lock()
READS: dict[tuple[str, str], int] = {}
COUNTS: dict[str, int] = {}
ATTACHED = {"beatmap": {"checksum": "fixture-chart"}, "folders": {"game": "fixture-game"}}


def state() -> dict:
    return json.loads(STATE.read_text("utf-8"))


def barrier() -> str:
    data = state()
    if MODE == "vosu" and "vosu_barrier" in data:
        return data["vosu_barrier"]
    return data["barrier"]


def envelope(schema: str, advance: bool) -> dict:
    current = barrier()
    reads = ENVELOPES["menu" if current == "main-menu" else current][schema]
    with LOCK:
        index = READS.get((current, schema), 0)
        if advance:
            READS[current, schema] = index + 1
    value = dict(reads[index % len(reads)])
    if schema == "v2" and current != "no-client":
        http = value["http"]
        body = {**json.loads(http["body_utf8"]), **ATTACHED}
        if current == "main-menu":
            body.update(state={"number": 0, "name": "menu"}, settings={"bassDensity": 0.6})
        value["http"] = dict(http, body_utf8=json.dumps(body, separators=(",", ":")))
    if reads[0]["provider_generation"] != reads[-1]["provider_generation"]:
        # An advancing provider (sc) keeps advancing past the exported reads: only the
        # fields the digest leaves out move, exactly as between the exported reads.
        shift = index // len(reads) * len(reads)
        value["provider_generation"] += shift
        value["derived"] = dict(value["derived"], revision=value["derived"]["revision"] + shift)
    if advance and current in OPTIONS.get("churn_generation", ()):
        # A live clock in the captured entries starts a new provider snapshot per read.
        value["provider_generation"] += index
    flaky = OPTIONS.get("flaky_dump_reads", {}).get(schema)
    if advance and flaky is not None and index in flaky:
        # The provider moved on between two dumps of one attempt; a retry must recover.
        value = dict(value, provider_generation=value["provider_generation"] + 1)
    if OPTIONS.get("leak_capability") == schema and CAPABILITY:
        value["http"] = dict(value["http"], body_utf8=value["http"]["body_utf8"] + CAPABILITY)
    return value


def state_body(schema: str) -> tuple[int, bytes]:
    if MODE == "vosu":
        http = envelope(schema, False)["http"]
        return http["status"], http["body_utf8"].encode("utf-8")
    current = barrier()
    if current == "no-client":
        return 500, NOT_READY
    # The upstream state is the same shape the vosu fixture envelopes carry.
    body = ENVELOPES_UPSTREAM[schema, current]
    unstable = OPTIONS.get("unstable_upstream")
    if unstable and (unstable is True or current in unstable) and schema == "v2":
        with LOCK:
            COUNTS["v2"] = COUNTS.get("v2", 0) + 1
            body = body[:-1] + f',"tick":{COUNTS["v2"]}}}'.encode()
    return 200, body


ENVELOPES_UPSTREAM = {
    (schema, name): json.dumps({"state": {"number": number, "name": label}, "game": {"paused": paused},
                                **({"settings": {"bassDensity": 0.6}} if name == "main-menu" else {}),
                                **(ATTACHED if schema == "v2" else {})},
                               separators=(",", ":")).encode()
    for schema in STATE_ROUTES.values()
    for name, number, label, paused in (("main-menu", 0, "menu", False),
                                        ("menu", 0, "menu", False), ("play", 2, "play", False),
                                        ("paused", 2, "play", True),
                                        ("result", 7, "resultScreen", False))}


def response(status: int, body: bytes, *, head: bool = False, extra: list[str] = ()) -> bytes:
    reason = {200: "OK", 404: "Not Found", 405: "Method Not Allowed", 500: "Internal Server Error"}
    lines = [f"HTTP/1.1 {status} {reason.get(status, 'Status')}",
             f"Date: {email.utils.formatdate(usegmt=True)}",
             "Content-Type: application/json", f"Content-Length: {len(body)}", *extra,
             "Connection: close"]
    return ("\r\n".join(lines) + "\r\n\r\n").encode() + (b"" if head else body)


def frame(opcode: int, payload: bytes) -> bytes:
    length = len(payload)
    if length < 126:
        return bytes((0x80 | opcode, length)) + payload
    return bytes((0x80 | opcode, 126)) + length.to_bytes(2, "big") + payload


class Handler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        try:
            if self.respond():
                return  # Closed without a response byte.
            # Like a real daemon, keep the connection until the client closes it, so
            # the runner can attest which process owns it.
            self.request.settimeout(2)
            while self.request.recv(65536):
                pass
        except OSError:
            pass

    def respond(self) -> bool | None:
        """Answer one request; True closes the connection without a response."""
        sock: socket.socket = self.request
        sock.settimeout(10)
        data = b""
        while b"\r\n\r\n" not in data:
            chunk = sock.recv(65536)
            if not chunk:
                return
            data += chunk
        head = data.split(b"\r\n\r\n", 1)[0].decode("latin-1")
        line, *header_lines = head.split("\r\n")
        method, target, _version = line.split(" ", 2)
        headers = [h.split(":", 1) for h in header_lines if ":" in h]
        values = lambda name: [v.strip() for n, v in headers if n.strip().lower() == name]
        path, _, query = target.partition("?")
        if path in OPTIONS.get("close_paths", ()):
            return True
        if path in OPTIONS.get("silent_paths", ()):
            # The pinned upstream's `upgrade` handler leaves a path it does not own
            # unanswered (parity_golden_expected): hold the connection until the client goes.
            sock.settimeout(None)
            while sock.recv(65536):
                pass
            return True
        if path in OPTIONS.get("file_paths", ()):
            # The pinned upstream sends no file validators; vosu adds them and honours them.
            tag, stamp = '"fake-etag"', "Thu, 01 Jan 2026 00:00:00 GMT"
            extra = [f"ETag: {tag}", f"Last-Modified: {stamp}"] if MODE == "vosu" else []
            if extra and (values("if-none-match") == [tag] or values("if-modified-since") == [stamp]):
                sock.sendall(response(304, b"", head=True, extra=extra))
            else:
                sock.sendall(response(200, b"chart", head=method == "HEAD", extra=extra))
            return
        if MODE == "vosu" and OPTIONS.get("request_budget") and len(
                urllib.parse.parse_qsl(query, keep_blank_values=True)) > 128:
            # adapter-surface-request-budget: the request never reaches a provider.
            sock.sendall(response(404, b"Not Found", head=method == "HEAD"))
            return
        if MODE == "vosu" and path == ENDPOINT and not OPTIONS.get("no_harness"):
            if values("x-vosu-test-capture") == [CAPABILITY]:
                if method != "GET":
                    sock.sendall(response(405, b'{"error":"method"}'))
                    return
                body = json.dumps(envelope(query.removeprefix("schema="), True),
                                  separators=(",", ":")).encode()
                sock.sendall(response(200, body, extra=["Cache-Control: no-store"]))
                return
        if path in STATE_ROUTES and method in ("GET", "HEAD") and not values("upgrade"):
            status, body = state_body(STATE_ROUTES[path])
            sock.sendall(response(status, body, head=method == "HEAD"))
            return
        key = values("sec-websocket-key")
        if path in WS_ROUTES and method == "GET" and len(key) == 1 and values("upgrade"):
            accept = base64.b64encode(hashlib.sha1(key[0].encode() + GUID).digest()).decode()
            sock.sendall(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                          f"Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").encode())
            _status, body = state_body(WS_ROUTES[path])
            if not OPTIONS.get("silent_ws"):  # silent_ws: no game data, so no state frame.
                sock.sendall(frame(1, body))
            try:
                while sock.recv(65536):
                    sock.sendall(frame(8, (1000).to_bytes(2, "big")))
                    break
            except OSError:
                pass
            return
        sock.sendall(response(404, b'{"error":"not found"}', head=method == "HEAD"))
        if target in OPTIONS.get("crash_targets", ()):
            # The pinned upstream answers, then its response logger throws and the
            # process exits (parity_golden_crash).
            sock.close()
            os._exit(1)


class Server(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


if OPTIONS.get("orphan_pid_file"):
    # A same-group child that ignores the polite signal: cleanup must still remove it.
    # With orphan_setsid it also leaves the daemon's process group, so only the
    # recorded process tree still ties it to the run.
    child = subprocess.Popen([sys.executable, "-c",
                              "import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                              "time.sleep(600)"],
                             start_new_session=bool(OPTIONS.get("orphan_setsid")))
    Path(OPTIONS["orphan_pid_file"]).write_text(str(child.pid))

with Server(("127.0.0.1", PORT), Handler) as server:
    server.serve_forever()
