from __future__ import annotations

from contextlib import contextmanager
import base64
import dataclasses
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock
from urllib.parse import unquote

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import parity_routes as pr
import route_matrix as rm
import route_security as rs

ROOT = HERE.parents[1]
FAST = pr.Limits(deadline_s=3.0, drain_s=0.05)
CONTEXT_VALUES = {"songs_file": "Probe Set/map.osu", "songs_dir": "Probe Set",
                  "beatmap_relative": "map.osu", "skin_file": "cursor.png",
                  "calc_relative": "Probe Set/map.osu"}


def read_request(conn: socket.socket) -> bytes:
    data = bytearray()
    while b"\r\n\r\n" not in data:
        chunk = conn.recv(65_536)
        if not chunk:
            return bytes(data)
        data.extend(chunk)
    head, _, rest = bytes(data).partition(b"\r\n\r\n")
    match = re.search(rb"(?im)^content-length:\s*(\d+)", head)
    length = int(match.group(1)) if match else 0
    body = bytearray(rest)
    while len(body) < length:
        body.extend(conn.recv(65_536))
    return head + b"\r\n\r\n" + bytes(body)


def recv_exact(conn: socket.socket, count: int) -> bytes:
    data = bytearray()
    while len(data) < count:
        chunk = conn.recv(count - len(data))
        if not chunk:
            raise OSError("closed")
        data.extend(chunk)
    return bytes(data)


def read_client_frame(conn: socket.socket) -> tuple[int, bytes, bytes, bytes]:
    """(opcode, mask, unmasked payload, raw frame bytes)."""
    header = recv_exact(conn, 2)
    length = header[1] & 0x7F
    if length == 126:
        header += recv_exact(conn, 2)
        length = int.from_bytes(header[2:], "big")
    elif length == 127:
        header += recv_exact(conn, 8)
        length = int.from_bytes(header[2:], "big")
    assert header[1] & 0x80, "client frames must be masked"
    mask = recv_exact(conn, 4)
    masked = recv_exact(conn, length)
    payload = bytes(value ^ mask[index % 4] for index, value in enumerate(masked))
    return header[0] & 0x0F, mask, payload, header + mask + masked


def server_frame(opcode: int, payload: bytes, fin: bool = True) -> bytes:
    length = len(payload)
    first = (0x80 if fin else 0) | opcode
    if length < 126:
        return bytes((first, length)) + payload
    return bytes((first, 126)) + length.to_bytes(2, "big") + payload


def handshake(conn: socket.socket, *, accept: str | None = None) -> bytes:
    request = read_request(conn)
    key = re.search(rb"(?im)^sec-websocket-key:\s*(\S+)", request).group(1).decode()
    conn.sendall((
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
        f"Sec-WebSocket-Accept: {accept or pr.ws_accept(key)}\r\n\r\n").encode())
    return request


class RawServer:
    def __init__(self, handler) -> None:
        self.handler = handler
        self.requests: list[bytes] = []
        self.connections = 0
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen()
        self.port = self.listener.getsockname()[1]
        self.threads: list[threading.Thread] = []
        self.errors: list[BaseException] = []
        self.accept = threading.Thread(target=self._accept, daemon=True)
        self.accept.start()

    def _accept(self) -> None:
        while True:
            try:
                conn, _ = self.listener.accept()
            except OSError:
                return
            self.connections += 1
            thread = threading.Thread(target=self._serve, args=(conn,), daemon=True)
            self.threads.append(thread)
            thread.start()

    def _serve(self, conn: socket.socket) -> None:
        with conn:
            try:
                self.handler(conn, self)
            except BaseException as exc:  # Surface handler failures in the test thread.
                self.errors.append(exc)

    def endpoint(self, daemon: str = "source") -> pr.RouteEndpoint:
        return pr.RouteEndpoint(daemon, self.port)

    def close(self) -> None:
        self.listener.close()
        for thread in self.threads:
            thread.join(5)
        if self.errors:
            raise self.errors[0]


@contextmanager
def serve(handler):
    server = RawServer(handler)
    try:
        yield server
    finally:
        server.close()


def responder(raw: bytes, *, hold: float = 0.0):
    def handle(conn: socket.socket, server: RawServer) -> None:
        server.requests.append(read_request(conn))
        conn.sendall(raw)
        if hold:
            time.sleep(hold)
    return handle


def rendered(case_id: str, context: rm.MatrixContext | None = None) -> rm.RenderedCase:
    return rm.render(rm.CASES[case_id], context or rm.MatrixContext("linux-lazer", CONTEXT_VALUES))


def body(record: dict, part: str = "body") -> bytes:
    return pr.decode_bytes(record["response"][part])


class RawHttpTests(unittest.TestCase):
    def test_preserves_status_line_header_case_order_and_duplicates(self) -> None:
        raw = (b"HTTP/1.1 200 Fine Reason\r\nX-B: 1\r\nset-Cookie: a\r\n"
               b"Content-Type: text/plain\r\nX-B:  2 \r\nContent-Length: 2\r\n\r\nok")
        with serve(responder(raw)) as server:
            record = pr.run_case(rendered("state-http.json.get.menu"), server.endpoint(), limits=FAST)
        self.assertEqual("response", record["outcome"])
        self.assertTrue(record["outcome_allowed"])
        response = record["response"]
        self.assertEqual("HTTP/1.1 200 Fine Reason", response["status_line"])
        self.assertEqual([["X-B", "1"], ["set-Cookie", "a"], ["Content-Type", "text/plain"],
                          ["X-B", "2"], ["Content-Length", "2"]], response["headers"])
        self.assertEqual(raw[:-2], pr.decode_bytes(response["head"]))
        compared, private = rm.split_headers(response["headers"], websocket=False)
        self.assertEqual([["Content-Type", "text/plain"], ["Content-Length", "2"]],
                         [list(item) for item in compared])
        self.assertEqual(["X-B", "set-Cookie", "X-B"], [name for name, _ in private])

    def test_chunked_wire_bytes_trailers_and_decoded_body_are_both_exact(self) -> None:
        wire = b"4;ext=1\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\nX-Trailer: t\r\n\r\n"
        raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n" + wire
        with serve(responder(raw)) as server:
            record = pr.run_case(rendered("state-http.json-v2.get.paused"), server.endpoint(),
                                 limits=FAST)
        self.assertEqual("chunked", record["response"]["framing"])
        self.assertEqual(wire, body(record, "body_wire"))
        self.assertEqual(b'{"a":1}', body(record))
        self.assertEqual([["X-Trailer", "t"]], record["response"]["trailers"])

    def test_binary_content_length_body_is_exact_and_base64_only_when_not_utf8(self) -> None:
        payload = bytes(range(256))
        raw = b"HTTP/1.1 206 Partial\r\nContent-Length: 256\r\n\r\n" + payload
        with serve(responder(raw)) as server:
            record = pr.run_case(rendered("files.range.single"), server.endpoint(), limits=FAST)
        encoded = record["response"]["body"]
        self.assertIn("base64", encoded)
        self.assertEqual(payload, body(record))
        self.assertEqual({"length": 5, "sha256": hashlib.sha256(b"r\xc3\xa9su").hexdigest(),
                          "text": "résu"}, pr.encode_bytes(b"r\xc3\xa9su"))
        with self.assertRaises(ValueError):
            pr.decode_bytes(dict(encoded, length=255))

    def test_head_does_not_consume_the_declared_body(self) -> None:
        head = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n"
        with serve(responder(head, hold=0.5)) as server:
            record = pr.run_case(rendered("state-http.json.head.menu"), server.endpoint(),
                                 limits=FAST)
        # The server holds the connection open for 0.5 s; the probe stops after the drain.
        self.assertLess(record["elapsed_ns"], 400_000_000)
        self.assertFalse(record["peer_closed"])
        self.assertEqual(("response", "none", b""),
                         (record["outcome"], record["response"]["framing"], body(record)))
        with serve(responder(head + b"0123456789")) as server:
            record = pr.run_case(rendered("state-http.json.head.menu"), server.endpoint(),
                                 limits=FAST)
        self.assertEqual(b"", body(record))
        self.assertEqual(b"0123456789", pr.decode_bytes(record["excess"]))

    def test_redirect_is_recorded_and_never_followed(self) -> None:
        raw = b"HTTP/1.1 302 Found\r\nLocation: /json\r\nContent-Length: 0\r\n\r\n"
        with serve(responder(raw)) as server:
            record = pr.run_case(rendered("state-http.json.near.trailing-slash"),
                                 server.endpoint(), limits=FAST)
            time.sleep(0.1)
        self.assertEqual(1, server.connections)
        self.assertEqual(302, record["response"]["status"])
        self.assertEqual(["/json"], pr.header_values(record["response"]["headers"], "location"))

    def test_framing_failures_are_named_and_never_allowed(self) -> None:
        cases = {
            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort": "truncated",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nok": "protocol-error",
            b"HTTP/1.1 200 OK\r\nX: a\r\n folded\r\nContent-Length: 0\r\n\r\n": "protocol-error",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n": "protocol-error",
            b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\n\r\n": "over-budget",
            b"": "closed",
        }
        small = dataclasses.replace(FAST, body=1024)
        for raw, outcome in cases.items():
            with self.subTest(outcome=outcome, raw=raw[:40]):
                with serve(responder(raw)) as server:
                    record = pr.run_case(rendered("state-http.json.get.menu"), server.endpoint(),
                                         limits=small)
                self.assertEqual(outcome, record["outcome"])
                self.assertFalse(record["outcome_allowed"])
                self.assertEqual(raw, pr.decode_bytes(record["received_raw"])[:len(raw)])

    def test_silent_server_times_out_within_the_case_deadline(self) -> None:
        def silent(conn: socket.socket, server: RawServer) -> None:
            read_request(conn)
            time.sleep(0.6)
        with serve(silent) as server:
            record = pr.run_case(rendered("state-http.json.get.menu"), server.endpoint(),
                                 limits=dataclasses.replace(FAST, deadline_s=0.3))
        self.assertEqual("timeout", record["outcome"])

    def test_identical_request_bytes_reach_both_daemons(self) -> None:
        raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nNot Found"
        ids = ("state-http.json-v2.post", "state-http.json-v2.budget.query-pairs-129",
               "origin-peer.host.missing", "origin-peer.host.duplicate",
               "calculation.relative-path", "files.budget.headers-33", "files.songs.dir.get")
        with serve(responder(raw)) as source, serve(responder(raw)) as vosu:
            for case_id in ids:
                item = rendered(case_id)
                records = pr.run_identical(item, [source.endpoint(), vosu.endpoint("vosu")],
                                           limits=FAST)
                self.assertEqual(["source", "vosu"], [record["daemon"] for record in records])
                self.assertEqual({item.digest}, {record["request_digest"] for record in records})
            self.assertEqual(source.requests, vosu.requests)
        self.assertEqual([rendered(case_id).request for case_id in ids], source.requests)
        self.assertNotIn(b"Host:", rendered("origin-peer.host.missing").request)
        self.assertEqual(2, rendered("origin-peer.host.duplicate").request.count(b"\r\nHost: "))

    def test_validator_comes_from_the_same_daemon_and_is_declared(self) -> None:
        def etag_server(tag: str | None):
            def handle(conn: socket.socket, server: RawServer) -> None:
                server.requests.append(read_request(conn))
                tag_line = f"ETag: {tag}\r\n" if tag else ""
                conn.sendall(f"HTTP/1.1 200 OK\r\n{tag_line}Content-Length: 0\r\n\r\n".encode())
            return handle
        item = rendered("files.conditional.if-none-match-etag")
        self.assertIn(b"If-None-Match: ${validator}\r\n", item.request)
        with serve(etag_server('"a"')) as source, serve(etag_server('W/"b"')) as vosu:
            records = pr.run_identical(item, [source.endpoint(), vosu.endpoint("vosu")],
                                       limits=FAST)
        self.assertNotIn(b"If-None-Match", source.requests[0])
        self.assertIn(b'If-None-Match: "a"\r\n', source.requests[1])
        self.assertIn(b'If-None-Match: W/"b"\r\n', vosu.requests[1])
        self.assertEqual(['"a"', 'W/"b"'], [record["validator"]["value"] for record in records])
        self.assertEqual(source.requests[1].replace(b'"a"', b"${validator}"), item.request)
        with serve(etag_server(None)) as server:
            record = pr.run_case(item, server.endpoint(), limits=FAST)
        self.assertEqual("validator-missing", record["outcome"])
        self.assertFalse(record["outcome_allowed"])
        self.assertEqual(1, len(server.requests))
        with self.assertRaises(ValueError):
            item.with_validator('"a"\r\nX-Injected: 1')

    def test_retry_or_second_daemon_cannot_change_request_bytes(self) -> None:
        ledger = rm.RequestLedger()
        item = rendered("state-http.json.get.menu")
        ledger.admit(item)
        ledger.admit(rendered("state-http.json.get.menu"))
        changed = dataclasses.replace(rm.CASES["state-http.json.get.menu"],
                                      headers=(("User-Agent", "retry"),))
        with self.assertRaisesRegex(ValueError, "request bytes changed"):
            ledger.admit(rm.render(changed, rm.MatrixContext("linux-lazer")))

    def test_owner_attestation_reuses_the_verified_daemon_checks(self) -> None:
        raw = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
        import parity_probes
        with serve(responder(raw)) as server, \
                mock.patch.object(parity_probes, "_assert_one_owner") as one, \
                mock.patch.object(parity_probes, "_assert_connection_owner") as connection:
            owner = {"pid": 1, "port": server.port}
            pr.run_case(rendered("state-http.json.get.menu"),
                        pr.RouteEndpoint("vosu", server.port, owner), limits=FAST)
        self.assertEqual(["route/state-http.json.get.menu/request/before",
                          "route/state-http.json.get.menu/request/after"],
                         [call.args[1] for call in one.call_args_list])
        self.assertEqual(1, connection.call_count)


class WebSocketTests(unittest.TestCase):
    def run_ws(self, handler, case_id: str = "ws.v2.upgrade.menu", **limits) -> tuple[dict, RawServer]:
        with serve(handler) as server:
            record = pr.run_case(rendered(case_id), server.endpoint(),
                                 limits=dataclasses.replace(FAST, **limits))
        return record, server

    def test_records_handshake_frames_masks_and_close(self) -> None:
        received = []

        def handle(conn: socket.socket, server: RawServer) -> None:
            server.requests.append(handshake(conn))
            conn.sendall(server_frame(rm.OP_TEXT, b'{"state":1}'))
            received.append(read_client_frame(conn))
            conn.sendall(server_frame(rm.OP_CLOSE, b"\x03\xe8"))
        record, _ = self.run_ws(handle)
        item = rendered("ws.v2.upgrade.menu")
        self.assertEqual(("upgraded", True), (record["outcome"], record["outcome_allowed"]))
        transcript = record["websocket"]
        self.assertTrue(transcript["accept_valid"])
        first, client_close, server_close = transcript["frames"]
        self.assertEqual(("server", True, 0, rm.OP_TEXT, False, None, 11),
                         (first["direction"], first["fin"], first["rsv"], first["opcode"],
                          first["masked"], first["mask"], first["length"]))
        self.assertEqual(b'{"state":1}', pr.decode_bytes(first["payload"]))
        self.assertEqual(("client", "script", rm.OP_CLOSE, True),
                         (client_close["direction"], client_close["origin"],
                          client_close["opcode"], client_close["masked"]))
        self.assertEqual(item.frames[0], received[0][3])
        self.assertEqual(received[0][1].hex(), client_close["mask"])
        self.assertEqual(b"\x03\xe8", received[0][2])
        self.assertEqual(rm.OP_CLOSE, server_close["opcode"])
        self.assertEqual(["complete", "closed"], [step["outcome"] for step in transcript["steps"]])
        self.assertEqual([b'{"state":1}'], [pr.decode_bytes(message["payload"])
                                            for message in transcript["messages"]])

    def test_partial_frame_is_never_accepted(self) -> None:
        def handle(conn: socket.socket, server: RawServer) -> None:
            handshake(conn)
            conn.sendall(bytes((0x81, 10)) + b"abcd")
        record, _ = self.run_ws(handle)
        self.assertEqual("protocol-error", record["outcome"])
        self.assertIn("partial frame", record["error"])
        self.assertEqual([], record["websocket"]["frames"])
        self.assertEqual(bytes((0x81, 10)) + b"abcd",
                         pr.decode_bytes(record["websocket"]["partial_frame"]))

    def test_frame_split_across_reads_is_reassembled_not_dropped(self) -> None:
        def handle(conn: socket.socket, server: RawServer) -> None:
            handshake(conn)
            conn.sendall(bytes((0x81, 5)) + b"he")
            time.sleep(0.2)
            conn.sendall(b"llo")
            read_client_frame(conn)
            conn.sendall(server_frame(rm.OP_CLOSE, b"\x03\xe8"))
        record, _ = self.run_ws(handle)
        self.assertEqual("upgraded", record["outcome"])
        self.assertEqual(b"hello", pr.decode_bytes(record["websocket"]["messages"][0]["payload"]))

    def test_step_timeout_keeps_a_partial_frame_buffered(self) -> None:
        item = rm.render(dataclasses.replace(
            rm.CASES["ws.v2.upgrade.menu"], script=(rm.Recv(1, 0.15), rm.Recv(1, 2.0), rm.Close())),
            rm.MatrixContext("linux-lazer"))

        def handle(conn: socket.socket, server: RawServer) -> None:
            handshake(conn)
            conn.sendall(bytes((0x81, 5)) + b"he")
            time.sleep(0.4)
            conn.sendall(b"llo")
            read_client_frame(conn)
            conn.sendall(server_frame(rm.OP_CLOSE, b""))
        with serve(handle) as server:
            record = pr.run_case(item, server.endpoint(), limits=FAST)
        self.assertEqual(["timeout", "complete", "closed"],
                         [step["outcome"] for step in record["websocket"]["steps"]])
        self.assertEqual("upgraded-timeout", record["outcome"])
        self.assertEqual(b"hello", pr.decode_bytes(record["websocket"]["messages"][0]["payload"]))

    def test_masked_or_reserved_server_frames_are_recorded_errors(self) -> None:
        frames = {
            "masked": bytes((0x81, 0x82)) + b"\x01\x02\x03\x04" + b"ab",
            "RSV": bytes((0xC1, 1)) + b"a",
            "reserved opcode": bytes((0x83, 1)) + b"a",
            "control frame": bytes((0x09, 1)) + b"a",
        }
        for reason, raw in frames.items():
            with self.subTest(reason=reason):
                def handle(conn: socket.socket, server: RawServer, raw: bytes = raw) -> None:
                    handshake(conn)
                    conn.sendall(raw)
                    time.sleep(0.2)
                record, _ = self.run_ws(handle)
                self.assertEqual("protocol-error", record["outcome"])
                self.assertIn(reason, record["error"])
                self.assertEqual(1, len(record["websocket"]["frames"]))

    def test_fragmented_message_with_interleaved_ping_gets_recorded_auto_pong(self) -> None:
        pongs = []

        def handle(conn: socket.socket, server: RawServer) -> None:
            handshake(conn)
            conn.sendall(server_frame(rm.OP_TEXT, b'{"a"', fin=False)
                         + server_frame(rm.OP_PING, b"p")
                         + server_frame(rm.OP_CONTINUATION, b":1}"))
            pongs.append(read_client_frame(conn))
            read_client_frame(conn)
            conn.sendall(server_frame(rm.OP_CLOSE, b"\x03\xe8"))
        record, _ = self.run_ws(handle)
        self.assertEqual("upgraded", record["outcome"])
        transcript = record["websocket"]
        self.assertEqual([b'{"a":1}'], [pr.decode_bytes(item["payload"])
                                        for item in transcript["messages"]])
        auto = [frame for frame in transcript["frames"] if frame.get("origin") == "auto"]
        self.assertEqual([(rm.OP_PONG, b"p")], [(opcode, payload) for opcode, _, payload, _ in pongs])
        self.assertEqual(rm.OP_PONG, auto[0]["opcode"])

    def test_invalid_accept_and_rejected_upgrade(self) -> None:
        def bad_accept(conn: socket.socket, server: RawServer) -> None:
            handshake(conn, accept="AAAA")
        record, _ = self.run_ws(bad_accept)
        self.assertEqual("protocol-error", record["outcome"])
        self.assertFalse(record["websocket"]["accept_valid"])

        raw = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 11\r\n\r\nBad Request"
        record, _ = self.run_ws(responder(raw), "ws.v2.invalid.missing-key")
        self.assertEqual(("rejected", True), (record["outcome"], record["outcome_allowed"]))
        self.assertEqual(b"Bad Request", body(record))
        self.assertNotIn(b"Sec-WebSocket-Key", rendered("ws.v2.invalid.missing-key").request)

    def test_filter_messages_reach_the_server_exactly(self) -> None:
        seen = []

        def handle(conn: socket.socket, server: RawServer) -> None:
            handshake(conn)
            conn.sendall(server_frame(rm.OP_TEXT, b"{}"))
            seen.append(read_client_frame(conn))
            conn.sendall(server_frame(rm.OP_TEXT, b'{"menu":{}}'))
            seen.append(read_client_frame(conn))
            conn.sendall(server_frame(rm.OP_CLOSE, b"\x03\xe8"))
        record, _ = self.run_ws(handle, "ws.v2.filter.nested")
        self.assertEqual("upgraded", record["outcome"])
        self.assertEqual((rm.OP_TEXT, b"applyFilters:" + rm.FILTER_V2), seen[0][::2][:2])
        item = rendered("ws.v2.filter.nested")
        self.assertEqual(list(item.frames), [frame for *_, frame in seen])
        self.assertEqual([b"{}", b'{"menu":{}}'], [pr.decode_bytes(message["payload"])
                                                   for message in record["websocket"]["messages"]])

    def test_server_close_before_the_expected_frame_is_not_success(self) -> None:
        def handle(conn: socket.socket, server: RawServer) -> None:
            handshake(conn)
            conn.sendall(server_frame(rm.OP_CLOSE, b"\x03\xf0"))
            read_client_frame(conn)
        record, _ = self.run_ws(handle)
        self.assertEqual("server-closed", record["outcome"])
        self.assertFalse(record["outcome_allowed"])
        echo = [frame for frame in record["websocket"]["frames"] if frame.get("origin") == "auto"]
        self.assertEqual(rm.OP_CLOSE, echo[0]["opcode"])


class MatrixTests(unittest.TestCase):
    def test_ids_follow_the_frozen_naming_and_cover_every_family(self) -> None:
        slug = re.compile(r"^[a-z0-9]+(-[a-z0-9]+)*(\.[a-z0-9]+(-[a-z0-9]+)*)*$")
        self.assertEqual(len(rm.MATRIX), len(rm.CASES))
        for case in rm.MATRIX:
            family, _, rest = case.id.partition(".")
            self.assertIn(family, rm.FAMILIES, case.id)
            self.assertRegex(rest, slug)
            self.assertIn(case.barrier, rm.BARRIERS)
        self.assertEqual(set(rm.FAMILIES), {case.family for case in rm.MATRIX})

    def test_required_cases_of_each_family_exist(self) -> None:
        ids = set(rm.CASES)
        for slug in ("json", "json-sc", "json-v2", "json-v2-precise"):
            for suffix in ("get.no-client", "get.menu", "get.paused", "get.result", "head.menu",
                           "post", "put", "delete", "options", "near.trailing-slash",
                           "near.wrong-case", "near.suffix", "near.double-slash",
                           "near.malformed-percent", "near.query"):
                self.assertIn(f"state-http.{slug}.{suffix}", ids)
        for slug in ("ws", "tokens", "v2", "v2-precise"):
            for suffix in ("upgrade.menu", "invalid.missing-key", "invalid.wrong-version",
                           "invalid.missing-upgrade", "invalid.missing-connection",
                           "invalid.post-upgrade", "invalid.get-plain"):
                self.assertIn(f"ws.{slug}.{suffix}", ids)
        for slug in ("v2", "v2-precise", "tokens"):
            for name in ("nested", "repeated-message", "invalid-json", "oversized"):
                self.assertIn(f"ws.{slug}.filter.{name}", ids)
        for suffix in ("v2-trailing-slash", "v2-precise-trailing-slash", "v2-subscribe"):
            self.assertIn(f"ws.near.{suffix}", ids)
        for name in ("files.beatmap.background.get", "files.beatmap.audio.head",
                     "files.beatmap.relative.get", "files.songs.file.get", "files.songs.dir.get",
                     "files.background-image.get", "files.skin.file.get",
                     "files.skin.lazer-file.get", "files.skin.index.get", "files.range.single",
                     "files.range.unsatisfiable", "files.range.multiple",
                     "files.conditional.if-none-match-etag",
                     "files.conditional.if-modified-since-last-modified",
                     "files.conditional.if-range-etag", "calculation.no-query",
                     "calculation.mods-acronyms", "calculation.accuracy", "calculation.combo",
                     "calculation.statistics", "calculation.invalid-accuracy",
                     "calculation.relative-path", "calculation.absolute-inside-root",
                     "calculation.absolute-outside-root", "origin-peer.http.none",
                     "origin-peer.http.loopback-ip", "origin-peer.http.foreign",
                     "origin-peer.host.malformed-port", "origin-peer.ws.foreign"):
            self.assertIn(name, ids)
        spellings = {case.security[1] for case in rm.MATRIX if case.security}
        self.assertEqual({spelling.name for spelling in rs.SPELLINGS}, spellings)

    def test_ingame_is_never_probed_and_vosu_only_routes_are_negative_only(self) -> None:
        values = dict(CONTEXT_VALUES)
        selected, _ = rm.select(rm.MATRIX, rm.MatrixContext("windows-stable", values))
        selected += rm.select(rm.MATRIX, rm.MatrixContext("linux-lazer", values))[0]
        for item in selected:
            self.assertNotIn(b"/api/ingame", item.request.lower())
        vosu_only = re.compile(r"^/(api/(v1|dashboard|counters|tools|keys)|tools|websocket/commands)")
        for case in rm.MATRIX:
            if vosu_only.match(case.target):
                self.assertEqual(rm.NEGATIVE, case.role, case.id)
                self.assertFalse(rm.golden_eligible(case))
                self.assertEqual(404, case.vosu_status)
        forbidden = {
            "api/ingame": rm._http("origin-peer.ingame", "GET", "/api/ingame"),
            "capability": rm._http("origin-peer.capability", "GET", "/json",
                                   headers=(("X-Vosu-Test-Capture", "00"),)),
        }
        for message, case in forbidden.items():
            with self.subTest(message=message), \
                    mock.patch.object(rm, "_origin_cases", lambda case=case: [case]), \
                    self.assertRaisesRegex(ValueError, message):
                rm._build()

    def test_negative_case_local_expectation(self) -> None:
        case = rm.CASES["state-http.negative.api-v1-suffix"]
        ok = {"daemon": "vosu", "response": {"status": 404}}
        self.assertEqual([], pr.local_expectation_failures(case, ok))
        self.assertTrue(pr.local_expectation_failures(case, dict(ok, response={"status": 200})))
        self.assertEqual([], pr.local_expectation_failures(
            case, {"daemon": "source", "response": {"status": 200}}))

    def test_upstream_crash_inventory_is_every_target_upstream_cannot_decode(self) -> None:
        # The pinned upstream logs each response through decodeURIComponent(req.url), which
        # throws on a `%` not followed by two hex digits or on escapes that are not UTF-8,
        # and the uncaught URIError ends the daemon. Every route-matrix request was sent to a
        # client-less pinned upstream (M4b pilot 2 diagnostic): exactly these eight killed it.
        inventory = {f"state-http.{slug}.near.{name}"
                     for slug in ("json", "json-sc", "json-v2", "json-v2-precise")
                     for name in ("malformed-percent", "malformed-percent-hex")}
        crashing = {case.id for case in rm.MATRIX if rm.upstream_crash_ledger(case)}
        self.assertEqual(inventory, crashing)

        def decodable(target: str) -> bool:
            data, index = bytearray(), 0
            while index < len(target):
                escape = target[index + 1:index + 3]
                if target[index] != "%":
                    data += target[index].encode()
                    index += 1
                elif len(escape) == 2 and all(ch in "0123456789abcdefABCDEF" for ch in escape):
                    data.append(int(escape, 16))
                    index += 3
                else:
                    return False
            try:
                data.decode("utf-8")
            except UnicodeDecodeError:
                return False
            return True

        for platform in rm.PLATFORMS:
            for item in rm.select(rm.MATRIX, rm.MatrixContext(platform, CONTEXT_VALUES))[0]:
                target = item.request.split(b" ", 2)[1].decode("ascii")
                self.assertEqual(item.case.id in crashing, not decodable(target), item.case.id)
        for case_id in crashing:
            case = rm.CASES[case_id]
            self.assertEqual((case.role, case.barrier, case.method, case.websocket, case.validator),
                             (rm.GOLDEN, "no-client", "GET", False, None), case_id)
        bad = dataclasses.replace(rm.CASES["state-http.json.near.malformed-percent"],
                                  id="origin-peer.crash-probe", barrier="menu")
        with mock.patch.object(rm, "_origin_cases", lambda: [bad]), \
                self.assertRaisesRegex(ValueError, "after the no-client barrier"):
            rm._build()

    def test_every_ledger_reference_is_an_existing_non_temporary_row(self) -> None:
        ledger = (ROOT / "tests/fixtures/upstream/DIVERGENCES.md").read_text(encoding="utf-8")
        rows = dict(re.findall(r"^\| ([a-zA-Z0-9.-]+) \| (security|contract|licensing|temporary) \|",
                               ledger, re.M))
        referenced = {ledger_id for case in rm.MATRIX for ledger_id in case.ledger}
        self.assertTrue(referenced)
        for ledger_id in referenced:
            self.assertIn(rows.get(ledger_id), ("security", "contract", "licensing"), ledger_id)

    def test_select_partitions_every_case_with_a_reason(self) -> None:
        for platform in rm.PLATFORMS:
            selected, excluded = rm.select(rm.MATRIX, rm.MatrixContext(platform, CONTEXT_VALUES))
            self.assertEqual(sorted(rm.CASES), sorted(
                [item.case.id for item in selected] + [case_id for case_id, _ in excluded]))
            self.assertTrue(all(reason for _, reason in excluded))
            self.assertTrue(selected)
        lazer = {item.case.id for item in rm.select(
            rm.MATRIX, rm.MatrixContext("linux-lazer", CONTEXT_VALUES))[0]}
        self.assertIn("files.skin.lazer-file.get", lazer)
        self.assertNotIn("files.skin.file.get", lazer)

    def test_compared_header_set_is_exactly_a4b7(self) -> None:
        headers = [("Date", "x"), ("ETag", "1"), ("Access-Control-Allow-Origin", "*"),
                   ("Upgrade", "websocket"), ("Sec-WebSocket-Accept", "a"), ("Server", "s"),
                   ("Content-Security-Policy", "sandbox"), ("X-Content-Type-Options", "nosniff")]
        http, private = rm.split_headers(headers, websocket=False)
        self.assertEqual(["ETag", "Access-Control-Allow-Origin", "Content-Security-Policy",
                          "X-Content-Type-Options"], [name for name, _ in http])
        self.assertEqual(["Date", "Upgrade", "Sec-WebSocket-Accept", "Server"],
                         [name for name, _ in private])
        ws, _ = rm.split_headers(headers, websocket=True)
        self.assertEqual(["ETag", "Access-Control-Allow-Origin", "Upgrade", "Sec-WebSocket-Accept",
                          "Content-Security-Policy", "X-Content-Type-Options"],
                         [name for name, _ in ws])

    def test_budget_cases_sit_exactly_at_and_above_the_limits(self) -> None:
        def query(case_id: str) -> str:
            line = rendered(case_id).request.split(b"\r\n", 1)[0].decode()
            return line.split(" ")[1].split("?", 1)[1]
        self.assertEqual(128, len(query("state-http.json-v2.budget.query-pairs-128").split("&")))
        self.assertEqual(129, len(query("state-http.json-v2.budget.query-pairs-129").split("&")))
        for case_id, total in (("state-http.json-v2.budget.query-bytes-16384", 16_384),
                               ("state-http.json-v2.budget.query-bytes-16385", 16_385)):
            key, _, value = query(case_id).partition("=")
            self.assertEqual(total, len(key) + len(value))
        for case_id, count in (("files.budget.headers-32", 32), ("files.budget.headers-33", 33)):
            self.assertEqual(count, rendered(case_id).request.count(b"\r\nIf-None-Match: "))
        self.assertIn(b"Range: bytes=0-1\x7f\r\n", rendered("files.budget.range-control-byte").request)

    def test_client_frames_are_deterministic_masked_and_length_encoded(self) -> None:
        for size in (0, 125, 126, 65_535, 65_536):
            payload = os.urandom(size)
            frame = rm.client_frame("ws.v2.filter.oversized", "step-1", rm.OP_TEXT, payload)
            self.assertEqual(frame, rm.client_frame("ws.v2.filter.oversized", "step-1",
                                                    rm.OP_TEXT, payload))
            header = 2 + (2 if 126 <= size < 65_536 else 8 if size >= 65_536 else 0)
            mask, masked = frame[header:header + 4], frame[header + 4:]
            self.assertEqual(payload, bytes(v ^ mask[i % 4] for i, v in enumerate(masked)))
            self.assertTrue(frame[1] & 0x80)
        oversized = rendered("ws.v2.filter.oversized").frames[0]
        self.assertLess(len(oversized), 1024 * 1024)
        self.assertGreater(len(oversized), 512 * 1024)
        self.assertEqual(rendered("ws.v2.upgrade.menu").request, rendered("ws.v2.upgrade.menu").request)
        self.assertNotEqual(rm.ws_key("ws.v2.upgrade.menu"), rm.ws_key("ws.v2.upgrade.paused"))

    def test_manifest_is_plain_json(self) -> None:
        manifest = json.loads(json.dumps(rm.manifest()))
        self.assertEqual(rm.MATRIX_FORMAT, manifest["format"])
        self.assertEqual(len(rm.MATRIX), len(manifest["cases"]))


class SecurityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name, "capture")
        self.songs = self.root / "game" / "Songs"
        self.skins = self.root / "game" / "Skins" / "Probe"
        (self.songs / "Probe Set").mkdir(parents=True)
        self.skins.mkdir(parents=True)
        (self.songs / "Probe Set" / "map.osu").write_text("osu file format v14\n")
        (self.skins / "cursor.png").write_bytes(b"png")
        self.layout = rs.prepare_security_layout(
            str(self.root), {"/Songs/": str(self.songs), "/files/beatmap/": str(self.songs),
                             "/files/skin/": str(self.skins)},
            {"/Songs/": "Probe Set/map.osu", "/files/beatmap/": "Probe Set/map.osu",
             "/files/skin/": "cursor.png"},
            chart_source=str(self.songs / "Probe Set" / "map.osu"))
        values = dict(CONTEXT_VALUES, calc_absolute_inside=str(self.songs / "Probe Set" / "map.osu"))
        self.context = rm.MatrixContext(f"{rs.host_os()}-lazer", values, self.layout)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def test_every_native_spelling_names_only_the_run_owned_sentinel(self) -> None:
        if rs.host_os() != "linux":
            self.skipTest("POSIX spellings")
        selected, excluded = rm.select(
            [case for case in rm.MATRIX if case.family == "path-security"], self.context)
        names = {item.case.security[1] for item in selected}
        self.assertTrue({"encoded-parent", "double-encoded-parent", "backslash-encoded",
                         "absolute-encoded", "absolute-double-slash"} <= names)
        if self.layout.links:
            self.assertIn("symlink", names)
        sentinel = os.path.realpath(self.layout.sentinel)
        for item in selected:
            self.assertFalse(item.lexical_only)
            target = item.request.split(b" ")[1].decode()
            prefix = item.case.security[0]
            spelled = target[len(prefix):]
            decoded = unquote(unquote(spelled)).replace("\\", "/")
            path = decoded if decoded.startswith("/") else os.path.join(
                self.layout.route_roots[prefix], decoded)
            self.assertEqual(sentinel, os.path.realpath(path), item.case.id)
        self.assertTrue(all("requires windows" in reason for _, reason in excluded
                            if "symlink" not in reason))

    def test_aborts_before_sending_for_any_target_outside_the_capture_root(self) -> None:
        sent = []
        home_file = str(Path.home() / ".profile")
        bad = {
            "home sentinel": dataclasses.replace(self.layout, sentinel=home_file),
            "home capture root": dataclasses.replace(self.layout, capture_root=str(Path.home())),
            "root outside capture": dataclasses.replace(
                self.layout, route_roots={"/Songs/": str(Path(self.temp.name, "elsewhere"))}),
            "foreign marker": dataclasses.replace(self.layout, token="0" * 32),
        }
        case = rm.CASES["path-security.songs.encoded-parent"]
        for reason, layout in bad.items():
            with self.subTest(reason=reason):
                context = dataclasses.replace(self.context, security=layout)
                with self.assertRaises(rs.SecurityAbort):
                    item = rm.render(case, context)
                    sent.append(item)
        Path(self.layout.sentinel).write_text("not ours\n")
        with self.assertRaises(rs.SecurityAbort):
            rm.render(case, self.context)
        self.assertEqual([], sent)

    def test_retargeted_symlink_and_outside_calculation_paths_abort(self) -> None:
        if "/Songs/" not in self.layout.links:
            self.skipTest("symlinks unavailable")
        link = self.songs / rs.LINK_NAME
        other = self.root / "other.txt"
        other.write_text("x")
        link.unlink()
        link.symlink_to(other)
        with self.assertRaises(rs.SecurityAbort):
            rm.render(rm.CASES["path-security.songs.symlink"], self.context)
        outside = dataclasses.replace(self.layout, sentinel_chart=str(Path.home() / "x.osu"))
        with self.assertRaises(rs.SecurityAbort):
            rm.render(rm.CASES["calculation.absolute-outside-root"],
                      dataclasses.replace(self.context, security=outside))
        values = dict(self.context.values, calc_absolute_inside=self.layout.sentinel)
        with self.assertRaises(rs.SecurityAbort):
            rm.render(rm.CASES["calculation.absolute-inside-root"],
                      dataclasses.replace(self.context, values=values))
        Path(self.layout.sentinel_chart).write_text("changed")
        with self.assertRaises(rs.SecurityAbort):
            rm.render(rm.CASES["calculation.absolute-outside-root"], self.context)

    def test_non_route_declared_root_admits_inside_paths_and_bounds_sentinels(self) -> None:
        files = self.root / "lazer" / "files"
        files.mkdir(parents=True)
        chart = files / "ab" / "abcd"
        chart.parent.mkdir()
        chart.write_text("osu file format v14\n")
        layout = dataclasses.replace(self.layout, other_roots={"files": str(files)})
        context = rm.MatrixContext(self.context.platform,
                                   dict(self.context.values, calc_absolute_inside=str(chart)),
                                   layout)
        rm.render(rm.CASES["calculation.absolute-inside-root"], context)
        with self.assertRaises(rs.SecurityAbort):
            rm.render(rm.CASES["calculation.absolute-inside-root"], dataclasses.replace(
                context, values=dict(context.values, calc_absolute_inside=str(Path.home()))))
        swallowed = dataclasses.replace(
            layout, other_roots={"files": str(self.root / "vosu-route-probe")})
        with self.assertRaisesRegex(rs.SecurityAbort, "inside a declared root"):
            rm.render(rm.CASES["calculation.absolute-outside-root"],
                      dataclasses.replace(context, security=swallowed))

    def test_calculation_paths_are_validated_and_query_encoded(self) -> None:
        outside = rm.render(rm.CASES["calculation.absolute-outside-root"], self.context)
        inside = rm.render(rm.CASES["calculation.absolute-inside-root"], self.context)
        for item, path in ((outside, self.layout.sentinel_chart),
                           (inside, self.context.values["calc_absolute_inside"])):
            target = item.request.split(b" ")[1].decode()
            self.assertEqual(path, unquote(target.split("?path=", 1)[1]))
            self.assertNotIn(" ", target)

    def test_windows_spellings_are_lexical_only_and_never_sent_from_linux(self) -> None:
        if rs.host_os() == "windows":
            self.skipTest("foreign-platform check")
        layout = rs.SecurityLayout(
            "windows", r"C:\vosu-capture\run1", "t" * 32,
            {"/Songs/": r"C:\vosu-capture\run1\osu!\Songs"},
            {"/Songs/": "Probe Set/map.osu"}, r"C:\vosu-capture\run1\vosu-route-probe\sentinel.txt")
        context = rm.MatrixContext("windows-stable", CONTEXT_VALUES, layout)
        spelled = {}
        for spelling in ("encoded-parent", "absolute-drive", "absolute-drive-slash",
                         "unc-extended", "unc-device", "ads", "trailing-dot", "trailing-space",
                         "backslash-raw"):
            item = rm.render(rm.CASES[f"path-security.songs.{spelling}"], context)
            self.assertTrue(item.lexical_only)
            spelled[spelling] = item.request.split(b" ")[1].decode()
            with self.assertRaises(rs.SecurityAbort):
                pr.run_case(item, pr.RouteEndpoint("source", 1))
        self.assertEqual("/Songs/..%2F..%2Fvosu-route-probe%2Fsentinel.txt",
                         spelled["encoded-parent"])
        self.assertTrue(spelled["ads"].endswith("map.osu::$DATA"))
        self.assertTrue(spelled["trailing-dot"].endswith("map.osu."))
        self.assertTrue(spelled["trailing-space"].endswith("map.osu%20"))
        self.assertTrue(spelled["unc-extended"].startswith("/Songs/%5C%5C%3F%5CC%3A"))
        self.assertTrue(spelled["absolute-drive-slash"].startswith("/Songs/C:/vosu-capture/"))
        for bad in (dataclasses.replace(layout, sentinel=r"C:\Users\vosu\secret.txt"),
                    dataclasses.replace(layout, capture_root=r"C:\Users\vosu"),
                    dataclasses.replace(layout, capture_root="C:\\")):
            with self.assertRaises(rs.SecurityAbort):
                rm.render(rm.CASES["path-security.songs.absolute-drive"],
                          dataclasses.replace(context, security=bad))

    def test_prepare_never_adopts_existing_files_and_cleanup_is_exact(self) -> None:
        with self.assertRaises(FileExistsError):
            rs.prepare_security_layout(str(self.root), {"/Songs/": str(self.songs)},
                                       {"/Songs/": "Probe Set/map.osu"})
        unrelated = self.songs / "Probe Set" / "map.osu"
        self.assertEqual(self.layout.upstream_static, str(self.root / rs.UPSTREAM_STATIC_DIR))
        self.assertTrue(Path(self.layout.upstream_static).is_dir())
        Path(self.layout.upstream_static, "written-by-upstream").write_text("x")
        rs.remove_security_layout(self.layout)
        self.assertFalse(Path(self.layout.sentinel).exists())
        self.assertFalse((self.root / rs.MARKER_NAME).exists())
        self.assertFalse((self.songs / rs.LINK_NAME).is_symlink())
        self.assertFalse(Path(self.layout.upstream_static).exists())
        self.assertTrue(unrelated.is_file())

    def test_sentinel_exposure_detection(self) -> None:
        self.assertTrue(rs.sentinel_exposed(
            rs.sentinel_content(self.layout.token).encode(), self.layout))
        self.assertFalse(rs.sentinel_exposed(b"Not Found", self.layout))
        self.assertEqual(self.layout.token, Path(self.root, rs.MARKER_NAME).read_text())


if __name__ == "__main__":
    unittest.main()
