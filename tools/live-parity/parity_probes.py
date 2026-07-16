from __future__ import annotations

import bisect
import base64
import argparse
import hashlib
import json
import os
import math
import queue
from pathlib import Path
import shutil
import secrets
import socket
import signal
import struct
import subprocess
import sys
import tempfile
import time
import threading
import http.client
import urllib.error
import urllib.request
import tomllib
from typing import Any
from urllib.parse import urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parent))
from live_parity import (SCHEMAS, PINNED_VOSU_VERSION, PINNED_SOURCE_REVISION, VOSU_BUILD_LABEL,
    SOURCE_BUILD_LABEL, SUPPORTED_MATRIX, Sample, align_active, barrier_of,
    beatmap_time, compare_pair, dump_capture, load_capture, monotonic_violations,
    paused_of, state_of)
from parity_domains import (
    cadence_evidence, confirmed_retry_boundaries, gamebase_path_family,
    infer_reference_capabilities, segment_at,
)
import parity_process as _parity_process
globals().update({name: value for name, value in vars(_parity_process).items() if not name.startswith("__")})
WS_PATHS = {"v1": "/ws", "v2": "/websocket/v2", "precise": "/websocket/v2/precise", "sc": "/tokens"}
class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req: Any, fp: Any, code: int, msg: str,
                         headers: Any, newurl: str) -> None:
        return None


HTTP_OPENER = urllib.request.build_opener(_NoRedirect)


def _assert_one_owner(record: dict[str, Any], phase: str, before: int) -> None:
    pid, port = record["pid"], record["port"]
    alive = _process_alive(pid, record["start_identity"])
    exclusive = record.get("ownership_mode") == "exclusive-port-hash"
    if sys.platform.startswith("linux") and not exclusive \
            and _linux_pid_owns_listening_socket(pid, port):
        observed = pid
    else:
        observed = _socket_owner(port)
    if not exclusive and observed != pid and alive:
        time.sleep(LINUX_INODE_OWNER_CACHE_NS / 1_000_000_000)
        observed = _socket_owner(port)
    if (not alive
            or (exclusive and (
                observed not in (None, pid) or _port_is_unused(port)))
            or (not exclusive and observed != pid)):
        raise ValueError(f"verified daemon lost socket ownership during {phase}")
    record["ownership_checks"].append({
        "before_monotonic_ns": before,
        "after_monotonic_ns": time.monotonic_ns(),
        "socket_owner_pid": observed,
        "port_exclusive": exclusive,
        "phase": phase,
    })


def _assert_connection_owner(
    record: dict[str, Any], connection: socket.socket, phase: str, before: int
) -> tuple[int, int]:
    pid = record["pid"]
    exclusive = record.get("ownership_mode") == "exclusive-port-hash"
    actual: int | None = None
    deadline = time.monotonic() + CONNECTION_OWNERSHIP_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if not _process_alive(pid, record["start_identity"]):
            break
        actual = _connection_owner(connection)
        if actual == pid or (exclusive and actual is None):
            break
        time.sleep(0.005)
    local = connection.getsockname()
    remote = connection.getpeername()
    if (local[0] != "127.0.0.1" or remote[0] != "127.0.0.1"
            or remote[1] != record["port"]
            or (exclusive and (
                actual not in (None, pid) or _port_is_unused(record["port"])))
            or (not exclusive and actual != pid)):
        raise ValueError(
            f"verified daemon does not own transport connection during {phase}; "
            f"observed owners: {sorted(_connection_owners(connection))}"
        )
    record["ownership_checks"].append({
        "before_monotonic_ns": before,
        "after_monotonic_ns": time.monotonic_ns(),
        "socket_owner_pid": _socket_owner(record["port"]),
        "connection_owner_pid": actual,
        "port_exclusive": exclusive,
        "client_port": local[1],
        "server_port": remote[1],
        "phase": phase,
    })
    return local[1], remote[1]




def fetch(base: str, daemon: str, schema: str,
          owner: dict[str, Any] | None = None) -> Sample:
    started = time.monotonic_ns()
    if owner is not None:
        _assert_one_owner(owner, f"http/{schema}/before", started)
    parsed = urlsplit(base)
    if parsed.hostname != "127.0.0.1" or parsed.port is None:
        raise ValueError("HTTP capture endpoint is not an exact loopback origin")
    connection = http.client.HTTPConnection(parsed.hostname, parsed.port, timeout=2)
    try:
        connection.connect()
        transport = connection.sock
        if transport is None:
            raise OSError("HTTP connection has no transport socket")
        request = (
            f"GET {SCHEMAS[schema]} HTTP/1.1\r\n"
            f"Host: {parsed.hostname}:{parsed.port}\r\n"
            "User-Agent: vosu-live-parity/1\r\n"
            "Connection: keep-alive\r\n"
            "\r\n"
        ).encode("ascii")
        transport.sendall(request)
        if owner is not None:
            _assert_connection_owner(
                owner, transport, f"http/{schema}/connection", started)
        response = http.client.HTTPResponse(transport, method="GET")
        response.begin()
        body = response.read()
        timestamp = time.monotonic_ns()
        try:
            payload = json.loads(body)
        except (ValueError, UnicodeDecodeError):
            payload = None
        exact_not_ready = (
            response.status == 500
            and payload == {"error": "osu is not ready/running"})
        sample = Sample(
            daemon, schema, timestamp, payload, response.status,
            "not_ready" if exact_not_ready else (
                None if response.status == 200 and isinstance(payload, dict)
                else "invalid_json" if response.status == 200
                else f"http_{response.status}"))
    except ValueError:
        raise
    except (OSError, http.client.HTTPException, json.JSONDecodeError,
            UnicodeDecodeError) as exc:
        timestamp = time.monotonic_ns()
        sample = Sample(
            daemon, schema, timestamp, None, 0, f"{type(exc).__name__}: {exc}")
    finally:
        connection.close()
    if owner is not None:
        _assert_one_owner(owner, f"http/{schema}/after", started)
    return sample

def _validate_ws_frame_header(first_byte: int, opcode: int,
                              final: bool, length: int) -> None:
    if first_byte & 0x70:
        raise ValueError("websocket frame has unnegotiated RSV bits")
    if opcode in (3, 4, 5, 6, 7, 11, 12, 13, 14, 15):
        raise ValueError("websocket frame uses reserved opcode")
    if opcode >= 8 and (not final or length > 125):
        raise ValueError("websocket control frame is fragmented or oversized")



def websocket_capture(base: str, daemon: str, schema: str, stop: threading.Event,
                      sink: list[Sample], lock: threading.Lock,
                      owner: dict[str, Any] | None = None,
                      failures: list[str] | None = None) -> None:
    parsed = urlsplit(base)
    host, port = parsed.hostname or "127.0.0.1", parsed.port
    if port is None:
        return
    path = WS_PATHS[schema]
    def verify(
        phase: str, before: int, connection: socket.socket | None = None
    ) -> bool:
        if owner is None:
            return True
        try:
            _assert_one_owner(owner, f"websocket/{schema}/{phase}", before)
            if connection is not None:
                _assert_connection_owner(
                    owner, connection, f"websocket/{schema}/{phase}", before)
            return True
        except ValueError as exc:
            if failures is not None:
                failures.append(str(exc))
            stop.set()
            return False
    while not stop.is_set():
        sock: socket.socket | None = None
        try:
            connect_started = time.monotonic_ns()
            if not verify("connect-before", connect_started):
                return
            sock = socket.create_connection((host, port), timeout=2)
            sock.settimeout(1)
            def recv_exact(length: int) -> bytes:
                value = bytearray()
                while len(value) < length:
                    try:
                        chunk = sock.recv(length - len(value))
                    except socket.timeout:
                        if stop.is_set():
                            raise
                        continue
                    if not chunk:
                        raise OSError("websocket closed")
                    value.extend(chunk)
                return bytes(value)
            key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
            request = (f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n"
                       f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n")
            sock.sendall(request.encode("ascii"))
            header = bytearray()
            while not header.endswith(b"\r\n\r\n") and len(header) < 16_384:
                header.extend(recv_exact(1))
            if not header.startswith(b"HTTP/1.1 101"):
                raise OSError("websocket upgrade rejected")
            expected_accept = base64.b64encode(hashlib.sha1(
                (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")).digest()).decode("ascii")
            headers = header.decode("iso-8859-1").split("\r\n")
            accept = next((line.split(":", 1)[1].strip() for line in headers
                           if line.lower().startswith("sec-websocket-accept:")), "")
            if accept != expected_accept:
                raise OSError("invalid websocket accept")
            if not verify("handshake-after", connect_started, sock):
                return
            fragment = bytearray()
            opcode = 0
            while not stop.is_set():
                first = recv_exact(2)
                final = bool(first[0] & 0x80)
                frame_opcode = first[0] & 0x0f
                length = first[1] & 0x7f
                if first[1] & 0x80:
                    raise OSError("server sent masked frame")
                if length == 126:
                    length = struct.unpack("!H", recv_exact(2))[0]
                elif length == 127:
                    length = struct.unpack("!Q", recv_exact(8))[0]
                if length > 1_048_576:
                    raise ValueError("websocket frame exceeds 1 MiB")
                _validate_ws_frame_header(
                    first[0], frame_opcode, final, length)
                body = bytearray(recv_exact(length))
                if frame_opcode == 8:
                    break
                if frame_opcode == 9:
                    mask = secrets.token_bytes(4)
                    masked = bytes(value ^ mask[index % 4]
                                   for index, value in enumerate(body))
                    sock.sendall(
                        bytes([0x8a, 0x80 | len(body)]) + mask + masked)
                    continue
                if frame_opcode == 10:
                    continue
                if frame_opcode in (1, 2):
                    if fragment:
                        raise ValueError(
                            "websocket data frame interrupted fragmented message")
                    opcode = frame_opcode
                elif frame_opcode == 0:
                    if opcode == 0:
                        raise ValueError("websocket continuation has no message")
                else:
                    raise ValueError("websocket frame opcode is invalid")
                if len(fragment) + len(body) > 1_048_576:
                    raise ValueError("websocket assembled message exceeds 1 MiB")
                fragment.extend(body)
                if not final:
                    continue
                if opcode != 1:
                    raise ValueError("websocket state message is not text")
                timestamp = time.monotonic_ns()
                payload = json.loads(fragment.decode("utf-8"))
                if not isinstance(payload, dict):
                    raise ValueError("websocket state message is not an object")
                with lock:
                    sink.append(Sample(
                        daemon, schema, timestamp, payload, transport="websocket"))
                fragment.clear()
                opcode = 0
        except TimeoutError:
            if not stop.is_set():
                time.sleep(0.025)
        except (OSError, ValueError, json.JSONDecodeError) as exc:
            if not stop.is_set():
                if failures is not None:
                    failures.append(
                        f"{daemon}/{schema} websocket protocol failure: "
                        f"{type(exc).__name__}: {exc}")
                stop.set()
        finally:
            if sock is not None:
                sock.close()
