from __future__ import annotations

import base64
import hashlib
import json
import math
import os
import secrets
import socket
import ssl
import struct
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, BinaryIO
from urllib.parse import urlsplit

from validation_error import ValidationError

MAX_MESSAGE_BYTES = 1024 * 1024
_WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

def atomic_json_write(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as stream:
            json.dump(value, stream, ensure_ascii=False, separators=(",", ":"), sort_keys=True, allow_nan=False)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


def _read_exact(stream: BinaryIO, length: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < length:
        chunk = stream.read(length - len(chunks))
        if not chunk:
            raise ValidationError("unexpected end of websocket stream")
        chunks.extend(chunk)
    return bytes(chunks)


@dataclass(frozen=True)
class Frame:
    fin: bool
    opcode: int
    payload: bytes


def _decode_frame_prefix(first: int, second: int) -> tuple[bool, int, int]:
    if first & 0x70:
        raise ValidationError("websocket RSV bits are not supported")
    if second & 0x80:
        raise ValidationError("server websocket frames must not be masked")
    return bool(first & 0x80), first & 0x0F, second & 0x7F


def _decode_frame_header(
    first: int,
    second: int,
    encoded_length: bytes,
    max_payload: int,
) -> tuple[bool, int, int]:
    fin, opcode, length = _decode_frame_prefix(first, second)
    if length == 126:
        length = struct.unpack("!H", encoded_length)[0]
        if length < 126:
            raise ValidationError("non-minimal websocket payload length")
    elif length == 127:
        if encoded_length[0] & 0x80:
            raise ValidationError("invalid websocket payload length")
        length = struct.unpack("!Q", encoded_length)[0]
        if length < 65536:
            raise ValidationError("non-minimal websocket payload length")
    if opcode >= 8 and (not fin or length > 125):
        raise ValidationError("invalid websocket control frame")
    if opcode not in (0, 1, 2, 8, 9, 10):
        raise ValidationError(f"unsupported websocket opcode {opcode}")
    if length > max_payload:
        raise ValidationError("websocket frame exceeds 1 MiB bound")
    return fin, opcode, length


def _decode_buffered_frame(
    buffer: bytearray,
    max_payload: int = MAX_MESSAGE_BYTES,
) -> tuple[Frame, int] | None:
    if len(buffer) < 2:
        return None
    first, second = buffer[0], buffer[1]
    _, _, length_marker = _decode_frame_prefix(first, second)
    extended_length_size = 2 if length_marker == 126 else 8 if length_marker == 127 else 0
    header_size = 2 + extended_length_size
    if len(buffer) < header_size:
        return None
    encoded_length = bytes(buffer[2:header_size])
    fin, opcode, payload_length = _decode_frame_header(
        first,
        second,
        encoded_length,
        max_payload,
    )
    frame_size = header_size + payload_length
    if len(buffer) < frame_size:
        return None
    return Frame(fin, opcode, bytes(buffer[header_size:frame_size])), frame_size


def decode_frame(stream: BinaryIO, max_payload: int = MAX_MESSAGE_BYTES) -> Frame:
    header = _read_exact(stream, 2)
    first, second = header
    _decode_frame_prefix(first, second)
    length_marker = second & 0x7F
    extended_length_size = 2 if length_marker == 126 else 8 if length_marker == 127 else 0
    encoded_length = _read_exact(stream, extended_length_size)
    fin, opcode, length = _decode_frame_header(first, second, encoded_length, max_payload)
    return Frame(fin, opcode, _read_exact(stream, length))


def encode_client_frame(opcode: int, payload: bytes = b"") -> bytes:
    if len(payload) > 125 and opcode >= 8:
        raise ValidationError("control payload is too large")
    first = 0x80 | opcode
    mask = secrets.token_bytes(4)
    length = len(payload)
    if length < 126:
        header = bytes((first, 0x80 | length))
    elif length < 65536:
        header = bytes((first, 0x80 | 126)) + struct.pack("!H", length)
    else:
        header = bytes((first, 0x80 | 127)) + struct.pack("!Q", length)
    masked = bytes(byte ^ mask[index & 3] for index, byte in enumerate(payload))
    return header + mask + masked


class WebSocket:
    def __init__(self, sock: socket.socket):
        self.sock = sock
        self.receive_buffer = bytearray()
        self.fragment_opcode: int | None = None
        self.fragments = bytearray()

    def send(self, opcode: int, payload: bytes = b"") -> None:
        self.sock.sendall(encode_client_frame(opcode, payload))

    def _receive_frame(self) -> Frame:
        while True:
            decoded = _decode_buffered_frame(self.receive_buffer)
            if decoded is not None:
                frame, frame_size = decoded
                del self.receive_buffer[:frame_size]
                return frame
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ValidationError("unexpected end of websocket stream")
            self.receive_buffer.extend(chunk)

    def receive_message(self) -> bytes | None:
        while True:
            frame = self._receive_frame()
            if frame.opcode == 9:
                self.send(10, frame.payload)
                continue
            if frame.opcode == 10:
                continue
            if frame.opcode == 8:
                if len(frame.payload) == 1:
                    raise ValidationError("invalid websocket close payload")
                self.send(8, frame.payload)
                return None
            if frame.opcode == 0:
                if self.fragment_opcode is None:
                    raise ValidationError("unexpected websocket continuation")
                if len(self.fragments) + len(frame.payload) > MAX_MESSAGE_BYTES:
                    raise ValidationError("fragmented websocket message exceeds 1 MiB bound")
                self.fragments.extend(frame.payload)
                if not frame.fin:
                    continue
                opcode, payload = self.fragment_opcode, bytes(self.fragments)
                self.fragment_opcode = None
                self.fragments.clear()
            else:
                if self.fragment_opcode is not None:
                    raise ValidationError("new data frame during fragmented message")
                opcode, payload = frame.opcode, frame.payload
                if len(frame.payload) > MAX_MESSAGE_BYTES:
                    raise ValidationError("websocket message exceeds 1 MiB bound")
                if not frame.fin:
                    self.fragment_opcode = opcode
                    self.fragments.extend(payload)
                    continue
            if opcode != 1:
                raise ValidationError("expected websocket text message")
            try:
                payload.decode("utf-8", errors="strict")
            except UnicodeDecodeError as exc:
                raise ValidationError("invalid UTF-8 websocket message") from exc
            return payload

    def close(self) -> None:
        self.sock.close()


def websocket_connect(
    base_url: str,
    timeout: float,
    endpoint: str = "/websocket/v2/precise",
) -> WebSocket:
    parsed = urlsplit(base_url)
    if parsed.scheme not in ("http", "https", "ws", "wss") or not parsed.hostname:
        raise ValidationError("base URL must use http, https, ws, or wss")
    secure = parsed.scheme in ("https", "wss")
    try:
        port = parsed.port or (443 if secure else 80)
    except ValueError as exc:
        raise ValidationError(f"invalid base URL port: {exc}") from exc
    base_path = parsed.path.rstrip("/")
    path = f"{base_path}{endpoint}" or endpoint
    if parsed.query or parsed.fragment:
        raise ValidationError("base URL must not contain a query or fragment")
    raw = socket.create_connection((parsed.hostname, port), timeout=timeout)
    try:
        sock = ssl.create_default_context().wrap_socket(raw, server_hostname=parsed.hostname) if secure else raw
        sock.settimeout(timeout)
        key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
        default_port = 443 if secure else 80
        hostname = f"[{parsed.hostname}]" if ":" in parsed.hostname else parsed.hostname
        host = hostname if port == default_port else f"{hostname}:{port}"
        request = (
            f"GET {path} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        ).encode("ascii")
        sock.sendall(request)
        response = bytearray()
        while not response.endswith(b"\r\n\r\n"):
            chunk = sock.recv(1)
            if not chunk:
                raise ValidationError("connection closed during websocket handshake")
            response.extend(chunk)
            if len(response) > 65536:
                raise ValidationError("websocket handshake headers are too large")
        head = bytes(response[:-4])
        try:
            lines = head.decode("ascii").split("\r\n")
        except UnicodeDecodeError as exc:
            raise ValidationError("non-ASCII websocket handshake") from exc
        status = lines[0].split(" ", 2)
        if len(status) < 2 or status[0] != "HTTP/1.1" or status[1] != "101":
            raise ValidationError(f"websocket handshake rejected: {lines[0]}")
        headers: dict[str, list[str]] = {}
        for line in lines[1:]:
            if ":" not in line:
                raise ValidationError("malformed websocket handshake header")
            name, value = line.split(":", 1)
            headers.setdefault(name.strip().lower(), []).append(value.strip())
        tokens = lambda name: {token.strip().lower() for value in headers.get(name, []) for token in value.split(",")}
        expected = base64.b64encode(hashlib.sha1((key + _WS_GUID).encode("ascii")).digest()).decode("ascii")
        if "websocket" not in tokens("upgrade") or "upgrade" not in tokens("connection"):
            raise ValidationError("invalid websocket upgrade headers")
        if headers.get("sec-websocket-accept") != [expected]:
            raise ValidationError("invalid Sec-WebSocket-Accept")
        if "sec-websocket-extensions" in headers or "sec-websocket-protocol" in headers:
            raise ValidationError("server selected an unrequested websocket extension or protocol")
        return WebSocket(sock)
    except BaseException:
        raw.close()
        raise


class CaptureAccounting:
    def __init__(self) -> None:
        self.events: list[dict[str, Any]] = []
        self.states: dict[str, dict[str, Any]] = {}
        self.completed_states: dict[str, dict[str, Any]] = {}
        self.active_mode: int | None = None
        self.segment = 1
        self.receive_times: list[int] = []
        self.failures: list[dict[str, Any]] = []
        self.frames = 0

    @staticmethod
    def _timestamp(value: Any, field: str) -> int | float | None:
        if value is None:
            return None
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
            raise ValidationError(f"keypress {field} must be a finite number or null")
        return value

    def _finish_segment(self) -> None:
        if not self.states:
            return
        self.completed_states.update(self.states)
        self.states.clear()
        self.segment += 1

    def _key_for_segment(self, name: str) -> str:
        return name if self.segment == 1 else f"{name}@{self.segment}"

    def process(self, payload: Any, receive_ns: int) -> None:
        if not isinstance(payload, dict):
            raise ValidationError("precise payload must be an object")
        keypresses = payload.get("keypresses")
        if not isinstance(keypresses, dict) or not isinstance(keypresses.get("keys"), list):
            raise ValidationError("precise payload has no keypresses.keys array")
        mode = keypresses.get("mode")
        mode_number = mode.get("number") if isinstance(mode, dict) else None
        if isinstance(mode_number, bool) or not isinstance(mode_number, int):
            raise ValidationError("precise payload has no integer keypresses.mode.number")
        keys = keypresses["keys"]
        self.receive_times.append(receive_ns)
        self.frames += 1
        if self.active_mode is not None and mode_number != self.active_mode:
            self._finish_segment()
        self.active_mode = mode_number
        if not keys:
            self._finish_segment()
            return
        probe_duplicates: dict[str, int] = {}
        for item in keys:
            if not isinstance(item, dict) or not isinstance(item.get("name"), str):
                raise ValidationError("keypress entry must have a string name")
            pressed = item.get("isPressed")
            count = item.get("count")
            if not isinstance(pressed, bool) or isinstance(count, bool) or not isinstance(count, int):
                raise ValidationError("keypress isPressed/count have invalid types")
            name = item["name"]
            occurrence = probe_duplicates.get(name, 0)
            probe_duplicates[name] = occurrence + 1
            base_key = name if occurrence == 0 else f"{name}#{occurrence + 1}"
            prior = self.states.get(self._key_for_segment(base_key))
            if prior is not None and count < prior["count"]:
                self._finish_segment()
                break
        duplicate_counts: dict[str, int] = {}
        for item in keys:
            if not isinstance(item, dict) or not isinstance(item.get("name"), str):
                raise ValidationError("keypress entry must have a string name")
            name = item["name"]
            occurrence = duplicate_counts.get(name, 0)
            duplicate_counts[name] = occurrence + 1
            base_key = name if occurrence == 0 else f"{name}#{occurrence + 1}"
            key = self._key_for_segment(base_key)
            pressed = item.get("isPressed")
            count = item.get("count")
            if not isinstance(pressed, bool) or isinstance(count, bool) or not isinstance(count, int):
                raise ValidationError("keypress isPressed/count have invalid types")
            pressed_at = self._timestamp(item.get("pressedAt"), "pressedAt")
            released_at = self._timestamp(item.get("releasedAt"), "releasedAt")
            current = {"pressed": pressed, "count": count, "pressedAt": pressed_at, "releasedAt": released_at}
            prior = self.states.get(key)
            if prior is None:
                current.update(firstCount=count, observedPresses=0, observedReleases=0, durations=[], pendingPress=None, mode=mode_number, segment=self.segment)
                self.states[key] = current
                continue
            edge = pressed != prior["pressed"]
            edge_name = "press" if pressed else "release"
            edge_field = "pressedAt" if pressed else "releasedAt"
            for field in ("pressedAt", "releasedAt"):
                old, new = prior[field], current[field]
                if old is not None and new is not None and new < old:
                    self.failures.append({"key": key, "type": "regressing", "field": field, "previous": old, "current": new, "receiveNs": receive_ns})
                expected_field = edge_field if edge else None
                if new != old and field != expected_field:
                    self.failures.append({"key": key, "type": "changedWithoutEdge", "field": field, "previous": old, "current": new, "receiveNs": receive_ns})
            if edge:
                timestamp = current[edge_field]
                if timestamp is None:
                    self.failures.append({"key": key, "type": "missing", "field": edge_field, "receiveNs": receive_ns})
                self.events.append({"key": key, "edge": edge_name, "apiGameplayMs": timestamp, "receiveNs": receive_ns, "count": count})
                if pressed:
                    prior["observedPresses"] += 1
                    prior["pendingPress"] = timestamp
                else:
                    prior["observedReleases"] += 1
                    if prior["pendingPress"] is not None and timestamp is not None:
                        prior["durations"].append(timestamp - prior["pendingPress"])
                    prior["pendingPress"] = None
            current.update(
                firstCount=prior["firstCount"], observedPresses=prior["observedPresses"],
                observedReleases=prior["observedReleases"], durations=prior["durations"],
                pendingPress=prior["pendingPress"],
                mode=prior["mode"], segment=prior["segment"],
            )
            self.states[key] = current

    def result(self) -> dict[str, Any]:
        summaries: dict[str, Any] = {}
        all_states = self.completed_states | self.states
        for key in sorted(all_states):
            state = all_states[key]
            delta = state["count"] - state["firstCount"]
            summaries[key] = {
                "mode": state["mode"], "segment": state["segment"],
                "firstCount": state["firstCount"], "lastCount": state["count"], "countDelta": delta,
                "observedPresses": state["observedPresses"], "observedReleases": state["observedReleases"],
                "estimatedMissedPresses": max(0, delta - state["observedPresses"]),
                "pressDurationsMs": state["durations"],
            }
        gaps = [later - earlier for earlier, later in zip(self.receive_times, self.receive_times[1:])]
        gap_stats = {
            "count": len(gaps), "min": min(gaps) if gaps else None, "max": max(gaps) if gaps else None,
            "mean": (sum(gaps) / len(gaps)) if gaps else None,
        }
        return {"keys": summaries, "receiveFrameGapsNs": gap_stats, "invariantFailures": self.failures, "frames": self.frames}


def capture(base_url: str, label: str, out: Path, timeout: float) -> dict[str, Any]:
    if not math.isfinite(timeout) or timeout <= 0:
        raise ValidationError("timeout must be a finite positive number")
    accounting = CaptureAccounting()
    started_wall = time.time_ns()
    ws = websocket_connect(base_url, timeout)
    deadline = time.monotonic() + timeout
    completed = False
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                completed = True
                break
            ws.sock.settimeout(remaining)
            try:
                message = ws.receive_message()
            except socket.timeout:
                completed = True
                break
            if message is None:
                completed = True
                break
            receive_ns = time.monotonic_ns()
            try:
                payload = json.loads(message)
            except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                raise ValidationError(f"invalid precise JSON: {exc}") from exc
            accounting.process(payload, receive_ns)
    finally:
        ws.close()
    result = {
        "metadata": {"label": label, "baseUrl": base_url, "endpoint": "/websocket/v2/precise", "startedUnixNs": started_wall},
        "events": accounting.events,
        "summary": accounting.result(),
    }
    if completed:
        atomic_json_write(out, result)
    if not accounting.states and not accounting.completed_states:
        raise ValidationError("capture completed without keypress data")
    return result
