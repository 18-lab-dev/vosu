from __future__ import annotations

import gc
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
class ProcessOwnershipTests(unittest.TestCase):
    def _free_port(self) -> int:
        probe = socket.socket()
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
        probe.close()
        return port

    def test_capture_launch_failure_restores_callers_gc_state(self) -> None:
        original = gc.isenabled()

        def fail_launch(_cfg):
            self.assertFalse(gc.isenabled())
            raise ValueError("client launch failed")

        try:
            for enabled in (True, False):
                with self.subTest(enabled=enabled):
                    gc.enable() if enabled else gc.disable()
                    with mock.patch.object(LIVE_RUN, "build_release_artifacts"), \
                            mock.patch.object(LIVE_RUN, "launch", return_value=({}, {})), \
                            mock.patch.object(LIVE_RUN, "_launch_client", side_effect=fail_launch):
                        with self.assertRaisesRegex(ValueError, "client launch failed"):
                            LIVE_RUN.capture(
                                {"_artifact_identity": {"client": {}}},
                                "unused.json", 1, 25)
                    self.assertEqual(gc.isenabled(), enabled)
        finally:
            gc.enable() if original else gc.disable()

    def test_windows_runtime_directory_avoids_temp_archive_path(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                    mock.patch.dict(
                        LIVE_RUN.os.environ, {"LOCALAPPDATA": tmp}):
                runtime = LIVE_RUN._private_runtime_dir("source-")
            try:
                self.assertEqual(
                    Path(tmp) / "vosu-live-parity", runtime.parent)
            finally:
                shutil.rmtree(runtime)

    def test_http_attestation_allows_completed_shutdown_request(self) -> None:
        checks = [
            {"before_monotonic_ns": 0, "after_monotonic_ns": 10},
            {"before_monotonic_ns": 16, "after_monotonic_ns": 20},
            {"before_monotonic_ns": 26, "after_monotonic_ns": 30},
        ]
        self.assertTrue(LIVE_RUN._http_samples_attested(checks, [15, 25]))
        self.assertFalse(LIVE_RUN._http_samples_attested(checks[:1], [15, 25]))
        self.assertFalse(LIVE_RUN._http_samples_attested(checks, [5, 25]))
        self.assertFalse(LIVE_RUN._http_samples_attested(checks, [15, 35]))
        self.assertFalse(LIVE_RUN._http_samples_attested(
            [checks[0], checks[0]], [15, 25]))

    @unittest.skipUnless(os.name == "posix", "POSIX process groups required")
    def test_signal_process_tree_terminates_descendants(self) -> None:
        process = subprocess.Popen(
            [
                sys.executable,
                "-c",
                "import subprocess,sys,time; "
                "child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)']); "
                "print(child.pid,flush=True); time.sleep(60)",
            ],
            stdout=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        assert process.stdout is not None
        child_pid = int(process.stdout.readline())
        child_start = LIVE_RUN._process_start_identity(child_pid)
        try:
            LIVE_RUN._signal_process_tree(process)
            process.wait(timeout=5)
            deadline = time.monotonic() + 5
            while (LIVE_RUN._process_alive(child_pid, child_start)
                   and time.monotonic() < deadline):
                time.sleep(0.01)
            self.assertFalse(LIVE_RUN._process_alive(child_pid, child_start))
        finally:
            LIVE_RUN._signal_process_tree(process, hard=True)
            process.stdout.close()


    def test_windows_queryable_terminated_process_is_not_alive(self) -> None:
        import ctypes

        kernel32 = mock.Mock()
        kernel32.OpenProcess.return_value = 99

        def set_exit_code(_process: int, exit_code: object) -> int:
            exit_code._obj.value = 0
            return 1

        kernel32.GetExitCodeProcess.side_effect = set_exit_code
        with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                mock.patch.object(
                    ctypes, "windll",
                    mock.Mock(kernel32=kernel32), create=True):
            self.assertFalse(LIVE_RUN._process_alive(1234, "5678"))
        kernel32.GetProcessTimes.assert_not_called()
        kernel32.CloseHandle.assert_called_once_with(99)


    def test_connection_owner_attests_despite_delayed_accept(self) -> None:
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        accepted: list[socket.socket] = []

        def accept_later() -> None:
            time.sleep(1.1)
            connection, _ = listener.accept()
            accepted.append(connection)

        thread = threading.Thread(target=accept_later)
        thread.start()
        connection = socket.create_connection(listener.getsockname())
        record = {
            "pid": os.getpid(),
            "start_identity": LIVE_RUN._process_start_identity(os.getpid()),
            "port": listener.getsockname()[1],
            "ownership_checks": [],
        }
        try:
            LIVE_RUN._assert_connection_owner(
                record, connection, "http/v1/connection", time.monotonic_ns())
            self.assertEqual(
                os.getpid(),
                record["ownership_checks"][-1]["connection_owner_pid"])
        finally:
            connection.close()
            thread.join()
            for accepted_connection in accepted:
                accepted_connection.close()
            listener.close()

    def test_http_response_uses_connection_bound_to_exact_owner(self) -> None:
        from http.server import BaseHTTPRequestHandler, HTTPServer
        request_seen = threading.Event()

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_GET(self) -> None:
                request_seen.set()
                body = b'{"state":"menu"}'
                self.send_response(200)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                time.sleep(0.05)
                self.wfile.write(body)

            def log_message(self, *_: object) -> None:
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        owner = {
            "pid": os.getpid(),
            "start_identity": LIVE_RUN._process_start_identity(os.getpid()),
            "port": server.server_port,
            "ownership_checks": [],
        }
        original_assert = LIVE_RUN._assert_connection_owner

        def assert_after_request(*args: object, **kwargs: object) -> tuple[int, int]:
            self.assertTrue(request_seen.wait(1))
            return original_assert(*args, **kwargs)

        try:
            started = time.monotonic_ns()
            with LIVE_RUN.HttpProbe(
                    f"http://127.0.0.1:{server.server_port}",
                    "source", "v1", owner) as probe, mock.patch(
                        "parity_probes._assert_connection_owner", assert_after_request):
                sample = probe.fetch()
            self.assertEqual(200, sample.status)
            connection_checks = [
                check for check in owner["ownership_checks"]
                if "connection_owner_pid" in check
            ]
            self.assertEqual(
                {"http/v1/connection"},
                {check["phase"] for check in connection_checks})
            self.assertTrue(all(
                check["connection_owner_pid"] == os.getpid()
                for check in connection_checks))
            self.assertGreaterEqual(
                sample.monotonic_ns - started, 40_000_000)
        finally:
            server.shutdown()
            thread.join()
            server.server_close()

    def test_preexisting_fake_listener_blocks_launch(self) -> None:
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        occupied = listener.getsockname()[1]
        cfg = {
            "poll_rate_ms": 150,
            "daemons": {
                "source": {
                    "base_url": f"http://127.0.0.1:{occupied}",
                    "command": [sys.executable, "-c", "import time; time.sleep(10)"],
                },
                "vosu": {
                    "base_url": f"http://127.0.0.1:{self._free_port()}",
                    "command": [sys.executable, "-c", "import time; time.sleep(10)"],
                },
            },
        }
        try:
            with self.assertRaisesRegex(ValueError, "already occupied"):
                LIVE_RUN.launch(cfg)
        finally:
            listener.close()

    def test_immediately_exiting_child_fails_capture_preflight(self) -> None:
        cfg = {
            "poll_rate_ms": 150,
            "daemons": {
                "source": {
                    "base_url": f"http://127.0.0.1:{self._free_port()}",
                    "command": [sys.executable, "-c", "raise SystemExit(0)"],
                },
                "vosu": {
                    "base_url": f"http://127.0.0.1:{self._free_port()}",
                    "command": [sys.executable, "-c", "import time; time.sleep(10)"],
                },
            },
        }
        with self.assertRaisesRegex(ValueError, "exited before owning port"):
            LIVE_RUN.launch(cfg)

    def test_runtime_ownership_evidence_tracks_exact_pid(self) -> None:
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        port = listener.getsockname()[1]
        evidence = {
            "source": {
                "pid": os.getpid(), "start_identity": LIVE_RUN._process_start_identity(os.getpid()),
                "port": port, "ownership_checks": [],
            },
        }
        try:
            LIVE_RUN._assert_runtime_ownership(evidence)
            self.assertEqual(1, len(evidence["source"]["ownership_checks"]))
        finally:
            listener.close()

    def test_process_exit_barrier_is_bound_to_exact_client_identity(self) -> None:
        client = {
            "pid": os.getpid(),
            "start_identity": LIVE_RUN._process_start_identity(os.getpid()),
        }
        self.assertFalse(LIVE_RUN._barrier_is_authorized("process-exit", client))
        self.assertTrue(LIVE_RUN._barrier_is_authorized("result", client))
        replaced = dict(client, start_identity="not-this-process")
        self.assertTrue(LIVE_RUN._barrier_is_authorized("process-exit", replaced))

    def test_linux_zombie_is_not_alive(self) -> None:
        if not sys.platform.startswith("linux"):
            self.skipTest("Linux proc lifecycle assertion")
        read_fd, write_fd = os.pipe()
        pid = os.fork()
        if pid == 0:
            os.close(write_fd)
            os.read(read_fd, 1)
            os._exit(0)
        os.close(read_fd)
        try:
            identity = LIVE_RUN._process_start_identity(pid)
            os.write(write_fd, b"x")
            os.close(write_fd)
            write_fd = -1
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                stat = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
                if stat[stat.rfind(")") + 2:].split()[0] == "Z":
                    break
                time.sleep(0.01)
            self.assertFalse(LIVE_RUN._process_alive(pid, identity))
        finally:
            if write_fd >= 0:
                os.close(write_fd)
            os.waitpid(pid, 0)

    def test_unlinked_linux_osu_executable_is_still_detected(self) -> None:
        if not sys.platform.startswith("linux"):
            self.skipTest("Linux proc identity assertion")
        with tempfile.TemporaryDirectory() as tmp:
            proc = Path(tmp)
            (proc / "comm").write_text("unknown\n", encoding="utf-8")
            (proc / "cmdline").write_bytes(b"/games/osu!\0--test\0")
            (proc / "exe").symlink_to("/games/osu! (deleted)")
            self.assertTrue(LIVE_RUN._linux_osu_process(proc))

    def test_linux_zombie_is_not_counted_as_running_osu(self) -> None:
        if not sys.platform.startswith("linux"):
            self.skipTest("Linux proc lifecycle assertion")
        with tempfile.TemporaryDirectory() as tmp:
            proc = Path(tmp)
            (proc / "stat").write_text("123 (osu!) Z\n", encoding="ascii")
            (proc / "comm").write_text("osu!\n", encoding="utf-8")
            self.assertFalse(LIVE_RUN._linux_osu_process(proc))

    def test_preexisting_osu_client_blocks_configured_launch(self) -> None:
        cfg = {
            "_artifact_identity": {
                "client": {"executable": sys.executable, "sha256": "a" * 64,
                           "version": "test"},
            },
            "client_args": [],
        }
        with mock.patch.object(LIVE_RUN, "_running_osu_pids", return_value={1234}):
            with self.assertRaisesRegex(ValueError, "pre-existing osu client"):
                LIVE_RUN._launch_client(cfg)

    def test_exact_client_exit_before_results_is_rejected(self) -> None:
        client = {"pid": 1234, "start_identity": "original"}
        with (
            mock.patch.object(LIVE_RUN, "_process_alive", return_value=False),
            mock.patch.object(LIVE_RUN, "_running_osu_processes", return_value={}),
        ):
            with self.assertRaisesRegex(ValueError, "before both result barriers"):
                LIVE_RUN._assert_client_population(client, False)

    def test_exited_approved_windows_process_is_not_a_new_client(self) -> None:
        for pid in (100, 200):
            for results_ready in (False, True):
                with self.subTest(pid=pid, results_ready=results_ready):
                    client = {
                        "pid": 100, "start_identity": "1000",
                        "approved_processes": [
                            {"pid": 100, "start_identity": "1000"},
                            {"pid": 200, "start_identity": "2000"},
                        ],
                    }
                    snapshot = {
                        pid: {
                            "pid": pid, "parent_pid": 999,
                            "start_identity": str(pid * 10),
                            "identity_source": "kernel-process-snapshot",
                        },
                    }
                    with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                            mock.patch.object(
                                LIVE_RUN, "_running_osu_processes",
                                return_value=snapshot), \
                            mock.patch.object(
                                LIVE_RUN, "_process_alive", return_value=False):
                        if results_ready:
                            self.assertFalse(
                                LIVE_RUN._assert_client_population(client, True))
                        else:
                            with self.assertRaisesRegex(
                                    ValueError, "before both result barriers"):
                                LIVE_RUN._assert_client_population(client, False)

    def test_reused_approved_windows_pid_is_rejected(self) -> None:
        client = {"pid": 100, "start_identity": "1000"}
        snapshot = {
            100: {
                "pid": 100, "parent_pid": 999, "start_identity": "2000",
                "identity_source": "kernel-process-snapshot",
            },
        }
        with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                mock.patch.object(
                    LIVE_RUN, "_running_osu_processes", return_value=snapshot), \
                mock.patch.object(LIVE_RUN, "_process_alive", return_value=False):
            with self.assertRaisesRegex(ValueError, "unapproved osu client"):
                LIVE_RUN._assert_client_population(client, True)

    def test_exact_windows_client_descendant_is_adopted(self) -> None:
        executable = Path(sys.executable).resolve()
        client = {
            "pid": 100,
            "start_identity": "1000",
            "executable": str(executable),
            "sha256": "a" * 64,
        }
        snapshot = {
            100: {"pid": 100},
            200: {
                "pid": 200,
                "parent_pid": 100,
                "start_identity": "2000",
                "executable": str(executable),
            },
        }
        with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                mock.patch.object(
                    LIVE_RUN, "_running_osu_processes",
                    return_value=snapshot), \
                mock.patch.object(
                    LIVE_RUN, "sha256_file", return_value="a" * 64), \
                mock.patch.object(LIVE_RUN, "_process_alive", return_value=True):
            self.assertTrue(LIVE_RUN._assert_client_population(client, False))
        self.assertEqual(
            [(100, None), (200, 100)],
            [(record["pid"], record["parent_pid"])
             for record in client["approved_processes"]],
        )

    def test_windows_client_descendants_are_adopted_in_ancestry_order(self) -> None:
        executable = Path(sys.executable).resolve()
        client = {
            "pid": 100,
            "start_identity": "1000",
            "executable": str(executable),
            "sha256": "a" * 64,
        }
        snapshot = {
            100: {"pid": 100},
            500: {
                "pid": 500, "parent_pid": 900, "start_identity": "3000",
                "executable": str(executable),
            },
            900: {
                "pid": 900, "parent_pid": 100, "start_identity": "2000",
                "executable": str(executable),
            },
        }
        with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                mock.patch.object(
                    LIVE_RUN, "_running_osu_processes",
                    return_value=snapshot), \
                mock.patch.object(
                    LIVE_RUN, "sha256_file", return_value="a" * 64), \
                mock.patch.object(LIVE_RUN, "_process_alive", return_value=True):
            self.assertTrue(LIVE_RUN._assert_client_population(client, False))
        self.assertEqual(
            [100, 900, 500],
            [record["pid"] for record in client["approved_processes"]],
        )

    def test_exited_transient_windows_descendant_is_still_attested(self) -> None:
        executable = Path(sys.executable).resolve()
        client = {
            "pid": 100, "start_identity": "1000",
            "executable": str(executable), "sha256": "a" * 64,
        }
        snapshot = {
            200: {
                "pid": 200, "parent_pid": 100, "start_identity": "2000",
                "executable": str(executable),
            },
        }
        with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                mock.patch.object(
                    LIVE_RUN, "_running_osu_processes",
                    return_value=snapshot), \
                mock.patch.object(
                    LIVE_RUN, "sha256_file", return_value="a" * 64), \
                mock.patch.object(
                    LIVE_RUN, "_process_alive",
                    side_effect=lambda pid, _identity: pid == 100):
            self.assertTrue(LIVE_RUN._assert_client_population(client, False))
        self.assertEqual(
            [100, 200],
            [record["pid"] for record in client["approved_processes"]],
        )

    def test_exited_windows_descendant_keeps_atomic_snapshot_identity(self) -> None:
        executable = Path(sys.executable).resolve()
        client = {
            "pid": 100,
            "start_identity": "1000",
            "executable": str(executable),
            "sha256": "a" * 64,
        }
        snapshot = {
            200: {
                "pid": 200,
                "parent_pid": 100,
                "start_identity": "2000",
                "image_name": executable.name,
                "executable": None,
                "sha256": None,
                "identity_source": "kernel-process-snapshot",
            },
        }
        with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                mock.patch.object(
                    LIVE_RUN, "_running_osu_processes",
                    return_value=snapshot), \
                mock.patch.object(
                    LIVE_RUN, "_process_alive",
                    side_effect=lambda pid, _identity: pid == 100):
            self.assertTrue(LIVE_RUN._assert_client_population(client, False))
        record = client["approved_processes"][1]
        self.assertEqual("kernel-process-snapshot", record["identity_source"])
        self.assertEqual(executable.name, record["image_name"])
        self.assertEqual("2000", record["start_identity"])
        self.assertTrue(LIVE_RUN._valid_client_process_group(
            client["approved_processes"], client))

    def test_windows_image_query_race_keeps_atomic_snapshot_identity(self) -> None:
        import ctypes

        snapshot = {
            200: {
                "pid": 200,
                "parent_pid": 100,
                "start_identity": "2000",
                "image_name": "osu!.exe",
            },
        }
        kernel32 = mock.Mock()
        kernel32.OpenProcess.return_value = 1
        kernel32.QueryFullProcessImageNameW.return_value = False

        with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                mock.patch.object(
                    LIVE_RUN, "windows_osu_process_snapshot",
                    return_value=snapshot), \
                mock.patch.object(
                    ctypes, "WinDLL", create=True, return_value=kernel32):
            processes = LIVE_RUN._running_osu_processes()

        self.assertEqual({
            **snapshot[200],
            "executable": None,
            "sha256": None,
            "identity_source": "kernel-process-snapshot",
        }, processes[200])
        kernel32.CloseHandle.assert_called_once_with(1)

    def test_exited_job_descendant_uses_kernel_membership_evidence(self) -> None:
        client = {
            "pid": 100, "start_identity": "1000",
            "executable": str(Path(sys.executable).resolve()),
            "sha256": "a" * 64,
        }
        observed_ns = 123_456_789
        snapshot = {
            200: {
                "pid": 200,
                "parent_pid": 100,
                "start_identity": f"job:200:{observed_ns}",
                "executable": None,
                "sha256": None,
                "identity_source": "private-job-membership",
                "observed_monotonic_ns": observed_ns,
            },
        }
        job = mock.Mock()
        job.observed.return_value = {
            "pid": 200,
            "observed_monotonic_ns": observed_ns,
            "exited": True,
        }
        with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                mock.patch.object(
                    LIVE_RUN, "_running_osu_processes",
                    return_value=snapshot), \
                mock.patch.object(LIVE_RUN, "_client_job", return_value=job), \
                mock.patch.object(
                    LIVE_RUN, "_process_alive",
                    side_effect=lambda pid, _identity: pid == 100):
            self.assertTrue(LIVE_RUN._assert_client_population(client, False))
        self.assertEqual(
            "private-job-membership",
            client["approved_processes"][1]["identity_source"],
        )
        self.assertIsNone(client["approved_processes"][1]["executable"])

    def test_unrelated_windows_client_is_rejected(self) -> None:
        client = {
            "pid": 100,
            "start_identity": "1000",
            "executable": str(Path(sys.executable).resolve()),
            "sha256": "a" * 64,
        }
        snapshot = {
            200: {
                "pid": 200, "parent_pid": 999, "start_identity": "2000",
                "executable": client["executable"],
            },
        }
        with mock.patch.object(LIVE_RUN.sys, "platform", "win32"), \
                mock.patch.object(
                    LIVE_RUN, "_running_osu_processes",
                    return_value=snapshot), \
                mock.patch.object(LIVE_RUN, "_process_alive", return_value=True):
            with self.assertRaisesRegex(ValueError, "unapproved osu client"):
                LIVE_RUN._assert_client_population(client, False)

    def test_client_descendant_cleanup_checks_exact_identity(self) -> None:
        client = {
            "pid": 100,
            "start_identity": "root",
            "approved_processes": [
                {"pid": 100, "start_identity": "root"},
                {"pid": 200, "start_identity": "child"},
            ],
        }
        with mock.patch.object(
                LIVE_RUN, "_process_alive", return_value=True), \
                mock.patch.object(LIVE_RUN.os, "kill") as kill:
            LIVE_RUN._terminate_client_descendants(client)
        kill.assert_called_once_with(200, LIVE_RUN.signal.SIGTERM)

    def test_socket_owner_replacement_is_rejected_for_frame(self) -> None:
        record = {
            "pid": os.getpid(),
            "start_identity": LIVE_RUN._process_start_identity(os.getpid()),
            "port": 24050,
            "ownership_checks": [],
        }
        with mock.patch.object(LIVE_RUN, "_socket_owner", return_value=os.getpid() + 1):
            with self.assertRaisesRegex(ValueError, "lost socket ownership"):
                LIVE_RUN._assert_one_owner(
                    record, "websocket/v2/frame", time.monotonic_ns())

    def test_shared_socket_ownership_is_rejected(self) -> None:
        if not sys.platform.startswith("linux"):
            self.skipTest("Linux socket inode ownership assertion")
        with mock.patch.object(
                LIVE_RUN, "_linux_socket_owners", return_value={11, 12}):
            self.assertIsNone(LIVE_RUN._socket_owner(24050))
