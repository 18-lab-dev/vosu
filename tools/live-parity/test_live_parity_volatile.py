"""M4b O0 dry-run fixes: volatile ledger, stable barriers, copy-client mode, offset bundle
attestation, elevated refusal and run-owned worktree cleanup."""
from __future__ import annotations

import base64
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import parity_build
import parity_golden_capture as gc
import parity_golden_client as client_mode
from parity_golden_rules import dump_sandwich_difference
import parity_volatile as volatile


def v2(bass: str, live: str = "3300", extra: str = "1") -> bytes:
    return (f'{{"state":{{"number":0,"name":"menu"}},"settings":{{"bassDensity":{bass}}},'
            f'"beatmap":{{"time":{{"live":{live}}}}},"other":{extra}}}').encode()


def record(body: bytes, length: int | None = None) -> tuple:
    headers = [["Content-Type", "application/json"],
               ["Content-Length", str(len(body) if length is None else length)]]
    return volatile.record_view("/json/v2", "200", headers, body, None)


class LedgerTests(unittest.TestCase):
    def test_volatile_fields_compare_by_rule_not_value(self) -> None:
        first, used = record(v2("0.5190195025110165"))
        second, _ = record(v2("0.518", live="3310"))
        self.assertEqual(first, second)
        self.assertEqual(used, ["upstream-bass-density", "menu-time-v2-live"])

    def test_every_other_byte_is_still_exact(self) -> None:
        self.assertNotEqual(record(v2("0.5"))[0], record(v2("0.5", extra="2"))[0])
        self.assertNotEqual(record(v2("0.5"))[0], record(v2("0.5", extra="1.0"))[0],
                            "number spelling is never normalized")

    def test_rule_violations_and_absence_are_findings(self) -> None:
        with self.assertRaisesRegex(volatile.VolatileError, "nonnegative-number"):
            record(v2("-0.1"))
        with self.assertRaisesRegex(volatile.VolatileError, "nonnegative-number"):
            record(v2('"0.5"'))
        missing = b'{"settings":{},"beatmap":{"time":{"live":1}},"other":1}'
        self.assertNotEqual(record(v2("0.5"))[0], record(missing)[0])

    def test_content_length_must_still_describe_the_body(self) -> None:
        with self.assertRaisesRegex(volatile.VolatileError, "content-length"):
            record(v2("0.5"), length=3)

    def test_ws_frames_mask_only_text_payloads_of_their_route(self) -> None:
        frames = [("server", True, 1, False, v2("0.51")), ("client", True, 8, True, b"\x03\xe8")]
        other = [("server", True, 1, False, v2("0.52")), ("client", True, 8, True, b"\x03\xe8")]
        view = lambda f: volatile.record_view("/websocket/v2", "101", [], b"", f)[0]
        self.assertEqual(view(frames), view(other))
        self.assertNotEqual(volatile.record_view("/json/sc", "101", [], b"", frames)[0],
                            volatile.record_view("/json/sc", "101", [], b"", other)[0])

    def test_ledger_rows_name_reason_and_authority(self) -> None:
        rows = volatile.ledger()
        self.assertEqual({row["id"] for row in rows}, volatile.LEDGER_IDS)
        self.assertTrue(all(row["reason"] and row["authority"] for row in rows))


def entry(live: float, extra: int = 1) -> dict:
    value = (b"\xa2\x64time\xa1\x64live\xfb" + struct.pack(">d", live) + b"\x65extra"
             + bytes([extra]))
    return {"ns": "vosu.osu", "pointer": "/beatmap",
            "value_cbor_b64": base64.b64encode(value).decode(), "error": None}


def envelope(live: float, revision: int, extra: int = 1, bass: str = "0.5") -> dict:
    return {"format": "vosu-adapter-capture-v1", "schema": "v2", "provider_generation": 1,
            "session": {"ordinal": 0}, "focus_revision": 3,
            "roster": {"revision": 3, "sessions": []}, "bus_revision": revision,
            "namespace_revisions": [["vosu.osu", revision]], "entries": [entry(live, extra)],
            "derived": {"revision": 0, "status": "ready"},
            "http": {"kind": "ok", "status": 200, "body_utf8": v2(bass, str(int(live))).decode()},
            "digest": f"sha256:{revision:064d}"}


class DumpSandwichTests(unittest.TestCase):
    def test_live_clock_publication_is_volatile(self) -> None:
        self.assertIsNone(dump_sandwich_difference(envelope(3300.0, 23), envelope(3310.0, 32,
                                                                                    bass="0.4")))

    def test_other_entry_changes_and_backwards_revisions_still_fail(self) -> None:
        self.assertEqual(dump_sandwich_difference(envelope(1.0, 23), envelope(1.0, 24, extra=2)),
                         "entries")
        self.assertEqual(dump_sandwich_difference(envelope(1.0, 24), envelope(1.0, 23)),
                         "bus_revision")
        self.assertIn("volatile field", dump_sandwich_difference(envelope(1.0, 1),
                                                                 envelope(-1.0, 2)))


class BarrierTests(unittest.TestCase):
    def session(self, views: list) -> tuple[gc.Session, list]:
        client = mock.Mock()
        client.exited.return_value = False
        session = gc.Session({"golden": {}}, gc.SCENARIO_SETS["m4b-dry-run-v1"], Path("x"),
                             timeout=5, build=None, client=client, operator=lambda b, m: None)
        calls: list = []

        def observe(_source, _dump):
            calls.append(1)
            return {"source": "menu", "vosu": "menu"}, views[len(calls) - 1]
        session._observe = observe
        return session, calls

    def test_barrier_needs_consecutive_identical_views(self) -> None:
        session, calls = self.session(["a", "b", "b", None, "c", "c", "c", "d"])
        with mock.patch.object(gc, "POLL_S", 0):
            session._wait_barrier("menu", None, None, [])
        self.assertEqual(len(calls), 7)
        self.assertEqual(session.transcript["barriers"][0]["stable_observations"], 3)
        self.assertTrue(session.transcript["barriers"][0]["identical_views"])

    def test_caseless_barrier_needs_only_the_reported_state(self) -> None:
        # Active play: the clock and the provider generation change every observation.
        session, calls = self.session(["a", None, "b", "c"])
        with mock.patch.object(gc, "POLL_S", 0):
            session._wait_barrier("menu", None, None, [], identical_views=False)
        self.assertEqual(len(calls), 3)
        self.assertFalse(session.transcript["barriers"][0]["identical_views"])


class ClientRootTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name).resolve()
        self.install = self.tmp / "install" / "current"
        self.install.mkdir(parents=True)
        (self.install / "osu!.exe").write_bytes(b"MZ client")
        (self.install / "lib.dll").write_bytes(b"library")
        self.root = self.tmp / "capture"
        self.root.mkdir()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def cfg(self, executable: Path) -> dict:
        return {"client": "lazer", "client_executable": str(executable),
                "client_expected_sha256": parity_build.sha256_file(self.install / "osu!.exe"),
                "_artifact_identity": {"client": {"executable": str(executable)}},
                "golden": {"capture_root": str(self.root), "client_source": str(self.install)}}

    def test_client_outside_the_capture_root_is_refused(self) -> None:
        with self.assertRaisesRegex(client_mode.ClientRootError, "outside the capture root"):
            client_mode.check_client_root(self.cfg(self.install / "osu!.exe"))

    def test_copy_is_portable_verified_and_never_replaced(self) -> None:
        cfg = self.cfg(self.root / "client" / "osu!.exe")
        with mock.patch("builtins.print"):
            client_mode.golden_client_copy(cfg)
        self.assertTrue((self.root / "client" / "framework.ini").is_file())
        record = client_mode.check_client_root(cfg)
        self.assertEqual(record["copy"]["files"], 2)
        with self.assertRaisesRegex(client_mode.ClientRootError, "never replaced"):
            client_mode.golden_client_copy(cfg)
        (self.root / "client" / "lib.dll").write_bytes(b"changed")
        with self.assertRaisesRegex(client_mode.ClientRootError, "lib.dll"):
            client_mode.check_client_root(cfg)


class BuildAndCleanupTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name).resolve()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_offset_generator_bundle_is_required_and_attested(self) -> None:
        (self.tmp / "scripts").mkdir()
        (self.tmp / "scripts" / "prepare-offset-generator.py").write_text(
            "def fingerprint():\n    return 'f' * 64\n")
        rid = "win-x64" if sys.platform == "win32" else "linux-x64"
        base = self.tmp / "target" / "offset-generator" / rid
        with self.assertRaisesRegex(ValueError, "prepare-offset-generator.py --rid"):
            parity_build.offset_generator_bundle(self.tmp)
        base.mkdir(parents=True)
        archive = base / f"offset-generator-v1-{rid}.zip"
        archive.write_bytes(b"bundle")
        (base / f"offset-generator-v1-{rid}.zip.sha256").write_text(
            parity_build.sha256_file(archive) + "  bundle\n")
        (base / f"offset-generator-v1-{rid}.inputs.sha256").write_text("e" * 64 + "\n")
        with self.assertRaisesRegex(ValueError, "other generator sources"):
            parity_build.offset_generator_bundle(self.tmp)
        (base / f"offset-generator-v1-{rid}.inputs.sha256").write_text("f" * 64 + "\n")
        record = parity_build.offset_generator_bundle(self.tmp)
        self.assertEqual(record["sha256"], parity_build.sha256_file(archive))

    def test_elevated_runs_are_refused_before_anything_runs(self) -> None:
        build = mock.Mock()
        with mock.patch.object(gc, "_elevated", return_value=True), \
                self.assertRaisesRegex(gc.CaptureError, "elevated"):
            gc.golden_capture({"golden": {}}, gc.SCENARIO_SETS["m4b-dry-run-v1"],
                              str(self.tmp / "capture.json"), rehearsal_receipt=None, build=build)
        build.assert_not_called()

    def test_run_owned_worktree_and_registration_are_removed(self) -> None:
        repo, tree = self.tmp / "repo", self.tmp / "tree"
        git = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-C", str(repo)]
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        (repo / "a").write_text("a")
        subprocess.run([*git, "add", "a"], check=True)
        subprocess.run([*git, "commit", "-qm", "a"], check=True)
        subprocess.run([*git, "worktree", "add", "-q", "--detach", str(tree)], check=True)
        gc._remove_worktree(str(repo), str(tree))
        self.assertFalse(tree.exists())
        listed = subprocess.run([*git, "worktree", "list", "--porcelain"], check=True,
                                capture_output=True, text=True).stdout
        self.assertEqual(listed.count("worktree "), 1)


if __name__ == "__main__":
    unittest.main()
