from __future__ import annotations

from contextlib import contextmanager, ExitStack
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import socket
import sys
import threading
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import parity_probes as probes
from live_parity import SCHEMAS
from parity_capture import _http_samples_attested


@contextmanager
def http_server(daemon: str = "source", mode: str = "mixed"):
    class Server(ThreadingHTTPServer):
        def __init__(self):
            self.accepted_ports: list[int] = []
            self.requests: dict[str, int] = {}
            super().__init__(("127.0.0.1", 0), Handler)

        def get_request(self):
            connection, address = super().get_request()
            connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            self.accepted_ports.append(address[1])
            return connection, address

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_GET(self):
            sequence = self.server.requests.get(self.path, 0) + 1
            self.server.requests[self.path] = sequence
            not_ready = mode == "mixed" and sequence % 97 == 0
            body = json.dumps(
                {"error": "osu is not ready/running"} if not_ready else {
                    "daemon": daemon, "path": self.path, "sequence": sequence,
                    "client_port": self.client_address[1],
                }).encode()
            chunked = mode == "mixed" and sequence % 3 == 0
            self.send_response(500 if not_ready else 200)
            if mode == "close":
                self.send_header("Connection", "close")
            if chunked:
                self.send_header("Transfer-Encoding", "chunked")
            else:
                self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if mode == "truncate_once" and sequence == 1:
                self.wfile.write(body[:len(body) // 2])
                self.close_connection = True
            elif chunked:
                for chunk in (body[:7], body[7:]):
                    self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
                self.wfile.write(b"0\r\n\r\n")
            else:
                self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = Server()
    worker = threading.Thread(target=server.serve_forever)
    worker.start()
    try:
        yield server
    finally:
        server.shutdown()
        worker.join()
        server.server_close()


def owner_of(server):
    return {
        "pid": os.getpid(),
        "start_identity": probes._process_start_identity(os.getpid()),
        "port": server.server_port,
        "ownership_checks": [],
    }


class HttpReuseTests(unittest.TestCase):
    def test_eight_streams_exceed_port_exhaustion_onset_with_fresh_attestations(self):
        rounds = 3_000
        with http_server("source") as source, http_server("vosu") as vosu, ExitStack() as stack:
            servers = {"source": source, "vosu": vosu}
            owners = {daemon: owner_of(server) for daemon, server in servers.items()}
            streams = {
                (daemon, schema): stack.enter_context(probes.HttpProbe(
                    f"http://127.0.0.1:{server.server_port}", daemon, schema, owners[daemon]))
                for daemon, server in servers.items() for schema in SCHEMAS
            }
            timestamps = {key: [] for key in streams}
            for sequence in range(1, rounds + 1):
                for (daemon, schema), probe in streams.items():
                    sample = probe.fetch()
                    if sequence % 97 == 0:
                        self.assertEqual((500, "not_ready"), (sample.status, sample.error))
                        self.assertEqual({"error": "osu is not ready/running"}, sample.payload)
                    else:
                        self.assertEqual((200, None), (sample.status, sample.error))
                        self.assertEqual({
                            "daemon": daemon, "path": SCHEMAS[schema], "sequence": sequence,
                            "client_port": probe.connection.sock.getsockname()[1],
                        }, sample.payload)
                    check = owners[daemon]["ownership_checks"][-2]
                    self.assertEqual(f"http/{schema}/connection", check["phase"])
                    self.assertEqual(os.getpid(), check["connection_owner_pid"])
                    self.assertEqual(probe.connection.sock.getsockname()[1], check["client_port"])
                    self.assertEqual(servers[daemon].server_port, check["server_port"])
                    self.assertLessEqual(check["before_monotonic_ns"], check["after_monotonic_ns"])
                    self.assertLessEqual(check["after_monotonic_ns"], sample.monotonic_ns)
                    timestamps[daemon, schema].append(sample.monotonic_ns)
            for (daemon, schema), probe in streams.items():
                checks = [check for check in owners[daemon]["ownership_checks"]
                          if check["phase"] == f"http/{schema}/connection"]
                self.assertEqual(rounds, len(checks))
                self.assertEqual(rounds, probe.request_attempts)
                self.assertEqual(1, probe.connection_attempts)
                self.assertTrue(_http_samples_attested(checks, timestamps[daemon, schema]))
                self.assertFalse(_http_samples_attested(checks[:-1], timestamps[daemon, schema]))
                self.assertFalse(_http_samples_attested(
                    checks[:-1] + checks[-2:-1], timestamps[daemon, schema]))
            self.assertEqual([4, 4], [len(server.accepted_ports) for server in servers.values()])
            print(json.dumps({
                "http_endurance": {
                    "rounds": rounds, "streams": len(streams), "requests": rounds * len(streams),
                    "accepted_connections": sum(len(server.accepted_ports) for server in servers.values()),
                    "connection_attempts": sum(probe.connection_attempts for probe in streams.values()),
                    "valid_sample_bindings": rounds * len(streams),
                }
            }), flush=True)

    def test_reused_connection_rejects_changed_process_identity(self):
        with http_server() as server:
            owner = owner_of(server)
            with probes.HttpProbe(f"http://127.0.0.1:{server.server_port}", "source", "v1", owner) as probe:
                self.assertEqual(1, probe.fetch().payload["sequence"])
                self.assertEqual(2, probe.fetch().payload["sequence"])
                owner["start_identity"] = "not-the-attested-process"
                with self.assertRaisesRegex(ValueError, "lost socket ownership"):
                    probe.fetch()
                self.assertIsNone(probe.connection.sock)
                self.assertEqual(2, server.requests[SCHEMAS["v1"]])

    def test_peer_close_reconnects_but_connection_creation_is_bounded(self):
        with http_server(mode="close") as server:
            with probes.HttpProbe(f"http://127.0.0.1:{server.server_port}", "source", "v1") as probe:
                for sequence in range(1, probes.HTTP_CONNECTION_LIMIT + 1):
                    sample = probe.fetch()
                    self.assertEqual((200, sequence), (sample.status, sample.payload["sequence"]))
                with self.assertRaisesRegex(ValueError, "HTTP connection limit exceeded"):
                    probe.fetch()
                self.assertEqual(probes.HTTP_CONNECTION_LIMIT, probe.connection_attempts)
                self.assertEqual(probes.HTTP_CONNECTION_LIMIT, len(server.accepted_ports))

    def test_truncated_response_is_retained_without_retry_and_next_sample_reconnects(self):
        with http_server(mode="truncate_once") as server:
            with probes.HttpProbe(f"http://127.0.0.1:{server.server_port}", "source", "v1") as probe:
                failed = probe.fetch()
                self.assertEqual(0, failed.status)
                self.assertIn("IncompleteRead", failed.error)
                self.assertEqual(1, server.requests[SCHEMAS["v1"]])
                self.assertEqual(1, probe.connection_attempts)
                recovered = probe.fetch()
                self.assertEqual((200, 2), (recovered.status, recovered.payload["sequence"]))
                self.assertEqual(2, probe.connection_attempts)
                self.assertEqual(2, len(server.accepted_ports))


if __name__ == "__main__":
    unittest.main()
