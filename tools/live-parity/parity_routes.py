"""Raw HTTP/1.1 and WebSocket transcripts for the M4b route matrix (plan §5 P0).

Each record keeps the exact request bytes, the status line, ordered raw headers,
the exact wire body plus its transfer-decoded form, and for WebSocket every
frame's fin/rsv/opcode/mask/length/payload bytes in both directions. Nothing is
JSON-decoded, no redirect is followed, a request is never replayed implicitly,
and every read is bounded. These records never reuse `live_parity.Sample`.
"""
from __future__ import annotations

from dataclasses import dataclass
import base64
import hashlib
import socket
import time
from typing import Any, Callable, Iterable

import route_matrix as rm

RECORD_FORMAT = "vosu-route-record-v1"
MAX_HEAD_BYTES = 64 * 1024
MAX_BODY_BYTES = 16 * 1024 * 1024
MAX_EXCESS_BYTES = 64 * 1024
MAX_FRAME_BYTES = 4 * 1024 * 1024
MAX_TRANSCRIPT_BYTES = 32 * 1024 * 1024
MAX_FRAMES = 512
MAX_CHUNK_LINE = 4096
CASE_DEADLINE_S = 30.0
DRAIN_S = 0.25
WS_GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


@dataclass(frozen=True)
class RouteEndpoint:
    """One verified daemon on an exact loopback port; `owner` enables socket attestation."""
    daemon: str
    port: int
    owner: dict[str, Any] | None = None


@dataclass(frozen=True)
class Limits:
    head: int = MAX_HEAD_BYTES
    body: int = MAX_BODY_BYTES
    excess: int = MAX_EXCESS_BYTES
    frame: int = MAX_FRAME_BYTES
    transcript: int = MAX_TRANSCRIPT_BYTES
    frames: int = MAX_FRAMES
    deadline_s: float = CASE_DEADLINE_S
    drain_s: float = DRAIN_S


class ProbeFailure(Exception):
    def __init__(self, outcome: str, message: str) -> None:
        super().__init__(message)
        self.outcome = outcome


def encode_bytes(data: bytes) -> dict[str, Any]:
    """Valid UTF-8 stays reviewable text; anything else is base64. Length and digest always."""
    value: dict[str, Any] = {"length": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    try:
        value["text"] = data.decode("utf-8")
    except UnicodeDecodeError:
        value["base64"] = base64.b64encode(data).decode("ascii")
    return value


def decode_bytes(value: dict[str, Any]) -> bytes:
    data = value["text"].encode("utf-8") if "text" in value else base64.b64decode(value["base64"])
    if len(data) != value["length"] or hashlib.sha256(data).hexdigest() != value["sha256"]:
        raise ValueError("encoded bytes do not match their length or digest")
    return data


class _Wire:
    """Bounded buffered reader that keeps every received byte."""

    def __init__(self, sock: socket.socket, deadline: float, limit: int) -> None:
        self.sock, self.deadline, self.limit = sock, deadline, limit
        self.received = bytearray()
        self.offset = 0
        self.eof = False

    def _fill(self) -> bool:
        if self.eof:
            return False
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise ProbeFailure("timeout", "case deadline elapsed")
        self.sock.settimeout(remaining)
        try:
            chunk = self.sock.recv(65_536)
        except socket.timeout as exc:
            raise ProbeFailure("timeout", "case deadline elapsed") from exc
        except ConnectionResetError:
            chunk = b""
        if not chunk:
            self.eof = True
            return False
        if len(self.received) + len(chunk) > self.limit:
            raise ProbeFailure("over-budget", "transcript exceeds its byte budget")
        self.received.extend(chunk)
        return True

    def available(self) -> int:
        return len(self.received) - self.offset

    def take(self, count: int, eof: str = "truncated") -> bytes:
        while self.available() < count:
            if not self._fill():
                raise ProbeFailure(eof, f"peer closed with {self.available()} of {count} bytes")
        value = bytes(self.received[self.offset:self.offset + count])
        self.offset += count
        return value

    def take_until(self, delimiter: bytes, limit: int, outcome: str) -> bytes:
        start = self.offset
        while True:
            found = self.received.find(delimiter, start, start + limit + len(delimiter))
            if found >= 0:
                end = found + len(delimiter)
                value = bytes(self.received[self.offset:end])
                self.offset = end
                return value
            if self.available() > limit:
                raise ProbeFailure(outcome, f"no {delimiter!r} within {limit} bytes")
            if not self._fill():
                raise ProbeFailure("closed" if self.available() == 0 and start == 0
                                   else "truncated", "peer closed mid-message")

    def take_to_eof(self, limit: int) -> bytes:
        while self._fill():
            if self.available() > limit:
                raise ProbeFailure("over-budget", f"body exceeds {limit} bytes")
        value = bytes(self.received[self.offset:])
        self.offset = len(self.received)
        return value

    def drain(self, seconds: float, limit: int) -> tuple[bytes, bool]:
        """Bytes after the framed message, and whether the peer closed within `seconds`."""
        end = min(self.deadline, time.monotonic() + seconds)
        try:
            while not self.eof and self.available() <= limit:
                remaining = end - time.monotonic()
                if remaining <= 0:
                    break
                self.sock.settimeout(remaining)
                try:
                    chunk = self.sock.recv(65_536)
                except socket.timeout:
                    break
                except ConnectionResetError:
                    chunk = b""
                if not chunk:
                    self.eof = True
                    break
                self.received.extend(chunk)
        finally:
            value = bytes(self.received[self.offset:self.offset + limit])
            self.offset = len(self.received)
        return value, self.eof


def _parse_head(head: bytes) -> dict[str, Any]:
    lines = head[:-4].split(b"\r\n")
    status_line = lines[0]
    parts = status_line.split(b" ", 2)
    if len(parts) < 2 or parts[0] not in (b"HTTP/1.1", b"HTTP/1.0") \
            or len(parts[1]) != 3 or not parts[1].isdigit():
        raise ProbeFailure("protocol-error", "malformed status line")
    headers = []
    for line in lines[1:]:
        if line[:1] in (b" ", b"\t"):
            raise ProbeFailure("protocol-error", "obsolete header line folding")
        name, colon, value = line.partition(b":")
        if not colon or not name or name != name.strip() or b"\n" in line:
            raise ProbeFailure("protocol-error", "malformed header line")
        headers.append([name.decode("latin-1"), value.strip(b" \t").decode("latin-1")])
    return {"head": encode_bytes(head), "status_line": status_line.decode("latin-1"),
            "version": parts[0].decode("ascii"), "status": int(parts[1]),
            "reason": parts[2].decode("latin-1") if len(parts) > 2 else "",
            "headers": headers}


def header_values(headers: Iterable[list[str]], name: str) -> list[str]:
    lower = name.lower()
    return [value for key, value in headers if key.lower() == lower]


def _read_chunked(wire: _Wire, limit: int) -> tuple[bytes, list[list[str]]]:
    body = bytearray()
    while True:
        line = wire.take_until(b"\r\n", MAX_CHUNK_LINE, "protocol-error")
        size_text = line[:-2].split(b";", 1)[0]
        if not size_text or any(ch not in b"0123456789abcdefABCDEF" for ch in size_text):
            raise ProbeFailure("protocol-error", "malformed chunk size")
        size = int(size_text, 16)
        if size == 0:
            trailers = []
            while (line := wire.take_until(b"\r\n", MAX_CHUNK_LINE, "protocol-error")) != b"\r\n":
                name, colon, value = line[:-2].partition(b":")
                if not colon:
                    raise ProbeFailure("protocol-error", "malformed trailer")
                trailers.append([name.decode("latin-1"), value.strip(b" \t").decode("latin-1")])
            return bytes(body), trailers
        if len(body) + size > limit:
            raise ProbeFailure("over-budget", f"chunked body exceeds {limit} bytes")
        body.extend(wire.take(size))
        if wire.take(2) != b"\r\n":
            raise ProbeFailure("protocol-error", "chunk data is not followed by CRLF")


def _read_response(wire: _Wire, method: str, limits: Limits, record: dict[str, Any],
                   *, upgrade: bool = False) -> dict[str, Any]:
    """Read one response; the partial response stays in `record` when reading fails."""
    interim = []
    while True:
        head = wire.take_until(b"\r\n\r\n", limits.head, "over-budget")
        response = _parse_head(head)
        if 100 <= response["status"] < 200 and response["status"] != 101:
            interim.append(response)
            continue
        break
    response["interim"] = interim
    record["response"] = response
    status, headers = response["status"], response["headers"]
    body_start = wire.offset
    framing, decoded, trailers = "none", b"", []
    if not (method == "HEAD" or status in (204, 304) or status < 200
            or (upgrade and status == 101)):
        encodings = [item.strip().lower() for value in header_values(headers, "transfer-encoding")
                     for item in value.split(",") if item.strip()]
        lengths = {item.strip() for value in header_values(headers, "content-length")
                   for item in value.split(",")}
        if encodings:
            response["framing_conflict"] = bool(lengths)
            if encodings[-1] == "chunked":
                framing = "chunked"
                decoded, trailers = _read_chunked(wire, limits.body)
            else:
                framing, decoded = "eof", wire.take_to_eof(limits.body)
        elif lengths:
            if len(lengths) != 1 or not next(iter(lengths)).isdigit():
                raise ProbeFailure("protocol-error", "invalid or conflicting content-length")
            length = int(next(iter(lengths)))
            if length > limits.body:
                raise ProbeFailure("over-budget", f"content-length {length} exceeds budget")
            framing, decoded = "content-length", wire.take(length)
        else:
            framing, decoded = "eof", wire.take_to_eof(limits.body)
    response.update(framing=framing, body_wire=encode_bytes(
        bytes(wire.received[body_start:wire.offset])), body=encode_bytes(decoded),
        trailers=trailers)
    return response


def _assert_owner(endpoint: RouteEndpoint, phase: str, started: int,
                  connection: socket.socket | None = None) -> None:
    if endpoint.owner is None:
        return
    import parity_probes  # Socket-ownership attestation of the verified daemon.
    if connection is None:
        parity_probes._assert_one_owner(endpoint.owner, phase, started)
    else:
        parity_probes._assert_connection_owner(endpoint.owner, connection, phase, started)


def _connect(endpoint: RouteEndpoint, deadline: float) -> socket.socket:
    if not isinstance(endpoint.port, int) or not 0 < endpoint.port < 65_536:
        raise ValueError("route endpoint is not an exact loopback port")
    sock = socket.create_connection((rm.HOST, endpoint.port),
                                    timeout=max(0.1, deadline - time.monotonic()))
    sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    if sock.getpeername()[0] != rm.HOST:
        sock.close()
        raise ValueError("route endpoint is not loopback")
    return sock


def _exchange(endpoint: RouteEndpoint, request: bytes, rendered: rm.RenderedCase,
              limits: Limits, phase: str) -> dict[str, Any]:
    case = rendered.case
    started = time.monotonic_ns()
    deadline = time.monotonic() + limits.deadline_s
    record: dict[str, Any] = {"request": encode_bytes(request), "outcome": None, "error": None}
    _assert_owner(endpoint, f"route/{case.id}/{phase}/before", started)
    sock = _connect(endpoint, deadline)
    wire = _Wire(sock, deadline, limits.transcript)
    try:
        try:
            sock.sendall(request)
            _assert_owner(endpoint, f"route/{case.id}/{phase}/connection", started, sock)
            response = _read_response(wire, case.method, limits, record, upgrade=case.websocket)
            if case.websocket and response["status"] == 101:
                record["outcome"] = "upgraded"
                record["websocket"] = _run_script(sock, wire, rendered, response, limits, record)
            else:
                record["outcome"] = "rejected" if case.websocket else "response"
        except ProbeFailure as exc:
            record["outcome"], record["error"] = exc.outcome, str(exc)
        except OSError as exc:
            record["outcome"], record["error"] = "closed", f"{type(exc).__name__}: {exc}"
        if record["outcome"] in ("response", "rejected", "upgraded", "upgraded-timeout",
                                 "server-closed"):
            excess, closed = wire.drain(limits.drain_s, limits.excess)
        else:
            excess = bytes(wire.received[wire.offset:])
            closed = wire.eof
        record["excess"] = encode_bytes(excess)
        record["peer_closed"] = closed
        record["received"] = {"length": len(wire.received),
                              "sha256": hashlib.sha256(wire.received).hexdigest()}
        if record["outcome"] not in ("response", "rejected", "upgraded"):
            record["received_raw"] = encode_bytes(bytes(wire.received))
    finally:
        sock.close()
    _assert_owner(endpoint, f"route/{case.id}/{phase}/after", started)
    record["elapsed_ns"] = time.monotonic_ns() - started
    return record


def run_case(rendered: rm.RenderedCase, endpoint: RouteEndpoint, *,
             limits: Limits = Limits()) -> dict[str, Any]:
    """Send one rendered case to one daemon and return its raw record."""
    if rendered.lexical_only:
        raise rm.SecurityAbort(f"{rendered.case.id} was validated lexically only; "
                               "path probes run only on their native platform")
    case = rendered.case
    record: dict[str, Any] = {
        "format": RECORD_FORMAT, "case": case.id, "daemon": endpoint.daemon,
        "transport": "websocket" if case.websocket else "http", "request_digest": rendered.digest,
    }
    if case.validator is None:
        record.update(_exchange(endpoint, rendered.request, rendered, limits, "request"))
    else:
        prime = _exchange(endpoint, rendered.priming_request(), rendered, limits, "prime")
        record["prime"] = prime
        headers = (prime.get("response") or {}).get("headers", [])
        values = header_values(headers, case.validator.response_header)
        if prime["outcome"] != "response" or len(values) != 1:
            record.update(outcome="validator-missing", error=(
                f"priming response has {len(values)} {case.validator.response_header} values"))
        else:
            record["validator"] = {"request_header": case.validator.request_header,
                                   "response_header": case.validator.response_header,
                                   "value": values[0]}
            record.update(_exchange(endpoint, rendered.with_validator(values[0]), rendered,
                                    limits, "request"))
    record["outcome_allowed"] = record["outcome"] in case.outcomes
    return record


def run_identical(rendered: rm.RenderedCase, endpoints: Iterable[RouteEndpoint], *,
                  ledger: rm.RequestLedger | None = None,
                  limits: Limits = Limits()) -> list[dict[str, Any]]:
    """Run one case's identical bytes against each daemon in order (e.g. source, vosu)."""
    (ledger or rm.RequestLedger()).admit(rendered)
    records = [run_case(rendered, endpoint, limits=limits) for endpoint in endpoints]
    for record in records:
        if record["request_digest"] != rendered.digest:
            raise ValueError(f"{rendered.case.id}: request digest drifted")
        sent = [frame["bytes"]["sha256"] for frame in record.get("websocket", {}).get("frames", [])
                if frame.get("origin") == "script"]
        expected = [hashlib.sha256(frame).hexdigest() for frame in rendered.frames]
        if sent != expected[:len(sent)]:
            raise ValueError(f"{rendered.case.id}: scripted frames differ from rendered bytes")
    return records


# --------------------------------------------------------------------------- WebSocket


def ws_accept(key: str) -> str:
    return base64.b64encode(hashlib.sha1(key.encode("ascii") + WS_GUID).digest()).decode("ascii")


def _request_key(request: bytes) -> str | None:
    for line in request.split(b"\r\n\r\n", 1)[0].split(b"\r\n")[1:]:
        name, _, value = line.partition(b":")
        if name.strip().lower() == b"sec-websocket-key":
            return value.strip().decode("latin-1")
    return None


def _client_record(frame: bytes, origin: str, step: int | None) -> dict[str, Any]:
    length = frame[1] & 0x7F
    offset = 2 + (2 if length == 126 else 8 if length == 127 else 0)
    return {"direction": "client", "origin": origin, "step": step, "fin": bool(frame[0] & 0x80),
            "rsv": (frame[0] >> 4) & 7, "opcode": frame[0] & 0x0F, "masked": True,
            "mask": frame[offset:offset + 4].hex(), "length": len(frame) - offset - 4,
            "bytes": encode_bytes(frame)}


class PartialFrame(ProbeFailure):
    def __init__(self, partial: bytes, cause: str) -> None:
        super().__init__("protocol-error", f"partial frame of {len(partial)} bytes: {cause}")
        self.partial = partial


def _read_frame(wire: _Wire, limits: Limits, frames: list[dict[str, Any]],
                step: int | None) -> tuple[dict[str, Any], bytes]:
    """Read one complete server frame. A partial frame is never accepted: on a step
    timeout the bytes stay buffered for the next read; at EOF it is a protocol error."""
    start = wire.offset
    if len(frames) >= limits.frames:
        raise ProbeFailure("over-budget", f"more than {limits.frames} frames")
    try:
        header = wire.take(2, eof="closed")
        first, second = header
        length = second & 0x7F
        if length == 126:
            header += wire.take(2)
            length = int.from_bytes(header[2:], "big")
        elif length == 127:
            header += wire.take(8)
            length = int.from_bytes(header[2:], "big")
        masked = bool(second & 0x80)
        mask = wire.take(4) if masked else b""
        frame = {"direction": "server", "step": step, "fin": bool(first & 0x80),
                 "rsv": (first >> 4) & 7, "opcode": first & 0x0F, "masked": masked,
                 "mask": mask.hex() if masked else None, "length": length,
                 "header": encode_bytes(header + mask)}
        invalid = (
            "server sent a masked frame" if masked
            else "server frame sets unnegotiated RSV bits" if frame["rsv"]
            else "server frame uses a reserved opcode"
            if frame["opcode"] in (3, 4, 5, 6, 7, 11, 12, 13, 14, 15)
            else "server control frame is fragmented or oversized"
            if frame["opcode"] >= 8 and (not frame["fin"] or length > 125) else None)
        if invalid is not None:
            frames.append(frame)
            raise ProbeFailure("protocol-error", invalid)
        if length > limits.frame:
            frames.append(frame)
            raise ProbeFailure("over-budget", f"server frame exceeds {limits.frame} bytes")
        payload = wire.take(length)
    except ProbeFailure as exc:
        if exc.outcome == "timeout":
            wire.offset = start
        elif exc.outcome in ("truncated", "closed") and len(wire.received) > start:
            raise PartialFrame(bytes(wire.received[start:]), str(exc)) from exc
        raise
    frame["payload"] = encode_bytes(payload)
    frames.append(frame)
    return frame, payload


def _run_script(sock: socket.socket, wire: _Wire, rendered: rm.RenderedCase,
                response: dict[str, Any], limits: Limits, record: dict[str, Any]
                ) -> dict[str, Any]:
    case = rendered.case
    key = _request_key(rendered.request)
    accept = header_values(response["headers"], "sec-websocket-accept")
    expected = ws_accept(key) if key else None
    transcript: dict[str, Any] = {"accept_expected": expected,
                                  "accept_valid": expected is not None and accept == [expected],
                                  "frames": [], "steps": [], "messages": []}
    record["websocket"] = transcript
    frames, steps = transcript["frames"], transcript["steps"]
    if not transcript["accept_valid"]:
        raise ProbeFailure("protocol-error", "Sec-WebSocket-Accept does not match the key")
    fragment, fragment_opcode = bytearray(), 0
    state = {"server_closed": False, "client_closed": False, "auto": 0}
    scripted = iter(rendered.frames)

    def send(frame: bytes, origin: str, step: int | None) -> None:
        sock.sendall(frame)
        frames.append(_client_record(frame, origin, step))

    def receive(step: int | None) -> bool:
        """Read one frame; True when it completed a data message."""
        nonlocal fragment_opcode
        try:
            frame, payload = _read_frame(wire, limits, frames, step)
        except PartialFrame as exc:
            transcript["partial_frame"] = encode_bytes(exc.partial)
            raise
        opcode = frame["opcode"]
        if opcode == rm.OP_CLOSE:
            if len(payload) == 1:
                raise ProbeFailure("protocol-error", "close frame payload has one byte")
            state["server_closed"] = True
            if not state["client_closed"]:
                state["client_closed"] = True
                state["auto"] += 1
                send(rm.client_frame(case.id, f"auto-{state['auto']}", rm.OP_CLOSE, payload[:2]),
                     "auto", step)
            return False
        if opcode == rm.OP_PING:
            if not state["client_closed"]:
                state["auto"] += 1
                send(rm.client_frame(case.id, f"auto-{state['auto']}", rm.OP_PONG, payload),
                     "auto", step)
            return False
        if opcode == rm.OP_PONG:
            return False
        if opcode in (rm.OP_TEXT, rm.OP_BINARY):
            if fragment_opcode:
                raise ProbeFailure("protocol-error", "data frame interrupted a fragmented message")
            fragment_opcode = opcode
        elif not fragment_opcode:
            raise ProbeFailure("protocol-error", "continuation frame without a message")
        if len(fragment) + len(payload) > limits.frame:
            raise ProbeFailure("over-budget", f"message exceeds {limits.frame} bytes")
        fragment.extend(payload)
        if not frame["fin"]:
            return False
        message = bytes(fragment)
        if fragment_opcode == rm.OP_TEXT:
            try:
                message.decode("utf-8")
            except UnicodeDecodeError as exc:
                raise ProbeFailure("protocol-error", "text message is not UTF-8") from exc
        transcript["messages"].append({"step": step, "opcode": fragment_opcode,
                                       "payload": encode_bytes(message)})
        fragment.clear()
        fragment_opcode = 0
        return True

    def read_until(step: int, deadline: float, messages: int | None) -> str:
        received = 0
        saved = wire.deadline
        wire.deadline = min(saved, deadline)
        try:
            while not state["server_closed"]:
                if messages is not None and received >= messages:
                    return "complete"
                try:
                    if receive(step):
                        received += 1
                except ProbeFailure as exc:
                    if exc.outcome == "timeout" and time.monotonic() < saved:
                        return "timeout"
                    if exc.outcome == "closed":
                        state["server_closed"] = True
                        return "closed"
                    raise
            return "complete" if messages is not None and received >= messages else "closed"
        finally:
            wire.deadline = saved

    timed_out = False
    for index, step in enumerate(case.script or ()):
        if isinstance(step, rm.Recv):
            outcome = read_until(index, time.monotonic() + step.timeout_s, step.messages)
            timed_out |= outcome == "timeout"
        elif isinstance(step, rm.Send):
            frame = next(scripted)
            if state["client_closed"]:
                outcome = "skipped"
            else:
                send(frame, "script", index)
                outcome = "sent"
        elif isinstance(step, rm.Close):
            frame = next(scripted)
            if state["client_closed"]:
                outcome = "skipped"
            else:
                state["client_closed"] = True
                send(frame, "script", index)
                outcome = read_until(index, time.monotonic() + step.timeout_s, None)
                timed_out |= outcome == "timeout"
        else:
            time.sleep(step.seconds)
            outcome = "held"
        steps.append({"step": index, "kind": type(step).__name__.lower(), "outcome": outcome})
        if state["server_closed"] and not isinstance(step, rm.Close):
            # The server ended the session before the script did; later steps never run.
            record["outcome"] = "server-closed"
            return transcript
    record["outcome"] = "upgraded-timeout" if timed_out else "upgraded"
    return transcript


def local_expectation_failures(case: rm.RouteCase, record: dict[str, Any]) -> list[str]:
    """Local vosu-only checks for negative cases; never an upstream golden."""
    failures = []
    if record["daemon"] != "vosu" or case.vosu_status is None:
        return failures
    status = (record.get("response") or {}).get("status")
    if status != case.vosu_status:
        failures.append(f"{case.id}: vosu status {status}, expected {case.vosu_status}")
    if record.get("websocket"):
        failures.append(f"{case.id}: vosu upgraded a negative route")
    return failures


def run_matrix(rendered: Iterable[rm.RenderedCase], endpoints: list[RouteEndpoint], *,
               ledger: rm.RequestLedger | None = None, limits: Limits = Limits(),
               progress: Callable[[str], None] | None = None) -> list[dict[str, Any]]:
    """Run each rendered case once per daemon, in matrix order; P1 owns sandwiches/retries."""
    ledger = ledger or rm.RequestLedger()
    results = []
    for item in rendered:
        if progress is not None:
            progress(item.case.id)
        results.append({"case": item.case.id, "request_digest": item.digest,
                        "records": run_identical(item, endpoints, ledger=ledger, limits=limits)})
    return results
