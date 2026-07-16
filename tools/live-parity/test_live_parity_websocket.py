from __future__ import annotations

import json
import base64
import hashlib
import importlib.util
import math
from pathlib import Path
import subprocess
import os
import shutil
import socket
import threading
import time
import sys
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from live_parity import (PINNED_SOURCE_REVISION, Sample, align_active, barrier_of,
                         compare_pair, dump_capture, monotonic_violations, validate_barriers)
from parity_domains import gamebase_path_family, infer_reference_capabilities
RUN_SPEC = importlib.util.spec_from_file_location("live_parity_run", HERE / "run.py")
assert RUN_SPEC and RUN_SPEC.loader
LIVE_RUN = importlib.util.module_from_spec(RUN_SPEC)
RUN_SPEC.loader.exec_module(LIVE_RUN)


def v2(daemon: str, timestamp: int, state: object, time_ms: int, *, paused: bool = False,
       static: str = "same", score: int = 0) -> Sample:
    return Sample(daemon, "v2", timestamp, {
        "state": state,
        "client": "lazer",
        "session": {"playTime": time_ms},
        "beatmap": {
            "checksum": "shared-map",
            "time": {"live": time_ms},
            "title": static,
        },
        "play": {"paused": paused, "score": score},
        "game": {"paused": paused},
    })
class WebSocketCaptureTests(unittest.TestCase):
    def test_records_a_valid_text_frame_with_monotonic_timestamp(self) -> None:
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        port = listener.getsockname()[1]

        def serve() -> None:
            connection, _ = listener.accept()
            with connection:
                request = bytearray()
                while not request.endswith(b"\r\n\r\n"):
                    request.extend(connection.recv(1))
                key_line = next(line for line in request.decode("ascii").split("\r\n")
                                if line.lower().startswith("sec-websocket-key:"))
                key = key_line.split(":", 1)[1].strip()
                accept = base64.b64encode(hashlib.sha1(
                    (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")).digest())
                connection.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                                   b"Connection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
                bodies = [
                    b'{"state":{"name":"menu"}}',
                    b'{"state":{"name":"playing"}}',
                ]
                for body in bodies:
                    connection.sendall(bytes([0x81, len(body)]) + body)
                time.sleep(0.1)
            listener.close()

        server = threading.Thread(target=serve)
        server.start()
        stop = threading.Event()
        sink = []
        failures: list[str] = []
        worker = threading.Thread(target=LIVE_RUN.websocket_capture,
                                  args=(f"http://127.0.0.1:{port}", "source", "v2",
                                        stop, sink, threading.Lock(), {}, failures))
        started = time.monotonic_ns()
        with mock.patch.object(LIVE_RUN, "_assert_one_owner") as one_owner, \
                mock.patch.object(LIVE_RUN, "_assert_connection_owner") as connection_owner:
            worker.start()
            deadline = time.monotonic() + 2
            while len(sink) < 2 and time.monotonic() < deadline:
                time.sleep(0.01)
            stop.set()
            worker.join(timeout=3)
            server.join(timeout=3)
        self.assertEqual([], failures)
        self.assertEqual(
            ["websocket/v2/connect-before", "websocket/v2/handshake-after"],
            [call.args[1] for call in one_owner.call_args_list])
        self.assertEqual(
            ["websocket/v2/handshake-after"],
            [call.args[2] for call in connection_owner.call_args_list])
        self.assertEqual(2, len(sink))
        self.assertTrue(all(sample.transport == "websocket" for sample in sink))
        self.assertEqual(
            [{"state": {"name": "menu"}}, {"state": {"name": "playing"}}],
            [sample.payload for sample in sink])
        self.assertTrue(all(sample.monotonic_ns >= started for sample in sink))

    def test_idle_websocket_timeout_is_transient(self) -> None:
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(2)
        port = listener.getsockname()[1]
        accepted = 0

        def serve() -> None:
            nonlocal accepted
            connection, _ = listener.accept()
            accepted += 1
            with connection:
                request = bytearray()
                while not request.endswith(b"\r\n\r\n"):
                    request.extend(connection.recv(1))
                key_line = next(line for line in request.decode("ascii").split("\r\n")
                                if line.lower().startswith("sec-websocket-key:"))
                key = key_line.split(":", 1)[1].strip()
                accept = base64.b64encode(hashlib.sha1(
                    (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")).digest())
                connection.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                                   b"Connection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
                time.sleep(1.2)
                body = b'{"state":{"name":"menu"}}'
                connection.sendall(bytes([0x81, len(body)]) + body)
                time.sleep(0.1)
            listener.close()

        server = threading.Thread(target=serve)
        server.start()
        stop = threading.Event()
        sink: list[Sample] = []
        failures: list[str] = []
        worker = threading.Thread(
            target=LIVE_RUN.websocket_capture,
            args=(f"http://127.0.0.1:{port}", "vosu", "sc", stop, sink,
                  threading.Lock(), None, failures))
        worker.start()
        deadline = time.monotonic() + 3
        while not sink and time.monotonic() < deadline:
            time.sleep(0.01)
        stop.set()
        worker.join(timeout=3)
        server.join(timeout=3)
        self.assertFalse(worker.is_alive())
        self.assertEqual([], failures)
        self.assertEqual(1, accepted)
        self.assertEqual([{"state": {"name": "menu"}}],
                         [sample.payload for sample in sink])

    def test_partial_frame_survives_socket_timeout(self) -> None:
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(2)
        port = listener.getsockname()[1]
        accepted = 0

        def serve() -> None:
            nonlocal accepted
            connection, _ = listener.accept()
            accepted += 1
            with connection:
                request = bytearray()
                while not request.endswith(b"\r\n\r\n"):
                    request.extend(connection.recv(1))
                key_line = next(line for line in request.decode("ascii").split("\r\n")
                                if line.lower().startswith("sec-websocket-key:"))
                key = key_line.split(":", 1)[1].strip()
                accept = base64.b64encode(hashlib.sha1(
                    (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")).digest())
                connection.sendall(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                                   b"Connection: Upgrade\r\nSec-WebSocket-Accept: " + accept + b"\r\n\r\n")
                body = b'{"state":{"name":"playing"}}'
                frame = bytes([0x81, len(body)]) + body
                connection.sendall(frame[:7])
                time.sleep(1.2)
                connection.sendall(frame[7:])
                time.sleep(0.1)
            listener.close()

        server = threading.Thread(target=serve)
        server.start()
        stop = threading.Event()
        sink: list[Sample] = []
        failures: list[str] = []
        worker = threading.Thread(
            target=LIVE_RUN.websocket_capture,
            args=(f"http://127.0.0.1:{port}", "source", "v2", stop, sink,
                  threading.Lock(), None, failures))
        worker.start()
        deadline = time.monotonic() + 3
        while not sink and time.monotonic() < deadline:
            time.sleep(0.01)
        stop.set()
        worker.join(timeout=3)
        server.join(timeout=3)
        self.assertFalse(worker.is_alive())
        self.assertEqual([], failures)
        self.assertEqual(1, accepted)
        self.assertEqual([{"state": {"name": "playing"}}],
                         [sample.payload for sample in sink])


    def test_rejects_reserved_and_rsv_websocket_frames(self) -> None:
        with self.assertRaisesRegex(ValueError, "reserved opcode"):
            LIVE_RUN._validate_ws_frame_header(0x83, 3, True, 0)
        with self.assertRaisesRegex(ValueError, "RSV bits"):
            LIVE_RUN._validate_ws_frame_header(0xC1, 1, True, 0)
        with self.assertRaisesRegex(ValueError, "fragmented or oversized"):
            LIVE_RUN._validate_ws_frame_header(0x09, 9, False, 1)

class WebSocketBarrierWindowTests(unittest.TestCase):
    def test_covers_two_publication_intervals(self) -> None:
        self.assertEqual(600_000_000, LIVE_RUN._websocket_barrier_window_ns(150))
        self.assertEqual(1_000_000_000, LIVE_RUN._websocket_barrier_window_ns(250))
