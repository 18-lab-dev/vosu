"""The client-less golden rehearsal and the receipt golden-capture requires (owner amendment
"upto3"), plus the pilot-2 operator findings, against the golden-capture fake daemons."""
from __future__ import annotations

import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import parity_config
import parity_golden_capture as gc
import parity_golden_client as golden_client
from parity_golden_config import validate_golden_config
import parity_golden_rehearsal as rehearsal
import route_matrix as rm
import test_live_parity_golden_capture as base

BUDGET = ("state-http.json-v2.budget.query-pairs-128", "state-http.json-v2.budget.query-pairs-129")
CRASHING = "state-http.json.near.malformed-percent"
FOREIGN = "path-security.songs.absolute-drive"
SUBSET = ("state-http.json-v2.get.menu", "state-http.json-sc.get.no-client",
          "state-http.json-v2.head.no-client", *BUDGET, "state-http.negative.api-v1-suffix",
          FOREIGN, CRASHING)


@unittest.skipUnless(sys.platform.startswith("linux"), "the rehearsal runs on Linux")
class RehearsalTests(base.CaptureTestCase):
    def rehearsal_config(self, **options: dict) -> dict:
        cfg = self.config(gc.SCENARIO_SETS["m4b-full-v1"], **options)
        del cfg["golden"]["authorization"], cfg["golden"]["assets"]
        cfg["_artifact_identity"]["client"] = None
        return cfg

    def rehearse(self, cfg: dict, cases=SUBSET) -> tuple[int | Exception, dict, dict]:
        # The fakes report no-client throughout; the phase runs in its PID namespace.
        base.FakeClient(self.state)  # Writes the fakes' no-client state.
        receipt = self.out.with_name("receipt.json")
        try:
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                result: int | Exception = rehearsal.rehearse(
                    cfg, str(receipt), timeout=120.0, build=self.stub_build(),
                    cases=[rm.CASES[case_id] for case_id in cases])
        except gc.CaptureError as exc:
            result = exc
        transcript = json.loads(receipt.with_name("receipt.json.transcript.json").read_text("utf-8"))
        self.assertEqual([p.name for p in self.out.parent.iterdir() if p.name.endswith(".state.json")],
                         [], "the isolated phase's state file is removed")
        return result, json.loads(receipt.read_text("utf-8")), transcript

    def test_the_phase_runs_only_isolated_and_refuses_a_visible_game(self) -> None:
        state = self.tmp / "state.json"
        with self.assertRaisesRegex(gc.CaptureError, "only as PID 1 of its private PID namespace"):
            rehearsal.isolated_phase(str(state))
        game = {4242: {"pid": 4242, "parent_pid": None, "start_identity": None, "executable": None}}
        with mock.patch.object(rehearsal.os, "getpid", return_value=1), \
                mock.patch.object(gc.daemon_mod, "_running_osu_processes", return_value=game), \
                self.assertRaisesRegex(gc.CaptureError, r"osu! client is visible \(PIDs \[4242\]\)"):
            rehearsal.isolated_phase(str(state))
        self.assertEqual(list(self.out.parent.iterdir()), [])

    def test_passing_rehearsal_writes_the_receipt_golden_capture_accepts(self) -> None:
        cfg = self.rehearsal_config(source_options={"crash_targets": ["/json%"]},
                                    vosu_options={"request_budget": True})
        result, receipt, transcript = self.rehearse(cfg)
        self.assertEqual((result, receipt["result"], receipt["failures"]), (0, "pass", []))
        self.assertEqual(receipt["isolation"]["pid"], 1)
        self.assertEqual((receipt["vosu_commit"], receipt["tooling_digest"]),
                         (self.commit, rehearsal.tooling_digest()))
        self.assertEqual(receipt["renderings"],
                         {"linux-lazer": len(SUBSET) - 1, "windows-lazer": 1, "windows-stable": 0})
        self.assertEqual(receipt["outcomes"], {"complete": 6, "negative": 1, "upstream_crashed": 1})
        self.assertEqual([entry["case"] for entry in transcript["rehearsal"]][-1], CRASHING)
        self.assertEqual(receipt["golden"]["cases"], 5)
        self.assertEqual([b["barrier"] for b in receipt["barriers"]], ["no-client"])
        self.assertEqual(receipt["cleanup"]["errors"], [])
        self.assertEqual(oct(self.out.with_name("receipt.json").stat().st_mode & 0o777), "0o600")
        scenario = gc.ScenarioSet("test-receipt", ("menu",), ("state-http.json-v2.get.menu",))
        self.capture(scenario, base.FakeClient(self.state), self.config(scenario),
                     receipt=self.out.with_name("receipt.json"))
        self.assertEqual(self.transcript()["rehearsal_receipt"]["vosu_commit"], self.commit)

    def test_failing_cases_are_all_reported_and_the_receipt_is_refused(self) -> None:
        # Without the request budget the over-budget route answers like the dump; an
        # upstream that dies on a case outside the crash ledger is relaunched and reported;
        # a feed with no state frame is client-dependent at a client barrier only.
        cfg = self.rehearsal_config(source_options={"crash_targets": ["/json%", "/json/v2/"],
                                                    "silent_ws": True})
        cases = (*SUBSET[:-1], "state-http.json-v2.near.trailing-slash", "ws.v2.upgrade.menu",
                 "ws.v2.upgrade.no-client", CRASHING)
        result, receipt, _transcript = self.rehearse(cfg, cases)
        self.assertIsInstance(result, gc.CaptureError)
        self.assertEqual(receipt["result"], "fail")
        failed = dict(receipt["failures"])
        self.assertRegex(failed[BUDGET[1]], "must answer 404")
        self.assertRegex(failed["state-http.json-v2.near.trailing-slash"],
                         "upstream exited, but the case is not under ledger")
        self.assertRegex(failed["ws.v2.upgrade.no-client"], "outcome 'upgraded-timeout' is not allowed")
        self.assertEqual([case for case, _reasons in receipt["client_dependent"]],
                         ["ws.v2.upgrade.menu"])
        self.assertNotIn("ws.v2.upgrade.menu", failed)
        self.assertNotIn(CRASHING, failed)
        self.assertEqual(receipt["upstream_restarts"], 2)
        scenario = gc.ScenarioSet("test-refused", ("menu",), ("state-http.json-v2.get.menu",))
        self.refuses("rehearsal did not pass", scenario, base.FakeClient(self.state),
                     self.config(scenario), receipt=self.out.with_name("receipt.json"))

    def test_golden_capture_refuses_without_a_matching_receipt(self) -> None:
        scenario = gc.ScenarioSet("test-gate", ("menu",), ("state-http.json-v2.get.menu",))
        calls: list = []
        for receipt, pattern in (
                (None, "needs a passing rehearsal receipt"),
                (self.tmp / "absent.json", "cannot read the rehearsal receipt"),
                (self.receipt(format="other"), "not a vosu-golden-rehearsal-receipt-v1"),
                (self.receipt(result="fail"), "did not pass"),
                (self.receipt(vosu_commit="0" * 40), "receipt is for vosu 0{40}"),
                (self.receipt(tooling_digest="sha256:" + "0" * 64), "other tooling"),
                (self.receipt(source_revision="0" * 40), "another upstream pin")):
            with self.assertRaisesRegex(gc.CaptureError, pattern):
                with contextlib.redirect_stdout(io.StringIO()):
                    gc.golden_capture(self.config(scenario), scenario, str(self.out),
                                      rehearsal_receipt=None if receipt is None else str(receipt),
                                      build=self.stub_build(calls), client=base.FakeClient(self.state))
        self.assertEqual(calls, [], "a refused pilot never builds")
        self.assertEqual(list(self.out.parent.iterdir()), [])

    def test_tooling_digest_covers_tooling_and_documents_not_tests(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "tools" / "live-parity").mkdir(parents=True)
            (root / "tests" / "fixtures" / "upstream").mkdir(parents=True)
            for name in rehearsal.TOOLING_DOCUMENTS:
                (root / name).write_text("ledger")
            module = root / "tools" / "live-parity" / "parity_x.py"
            module.write_text("a = 1\n")
            digest = rehearsal.tooling_digest(root)
            (root / "tools" / "live-parity" / "test_x.py").write_text("ignored")
            self.assertEqual(rehearsal.tooling_digest(root), digest)
            module.write_text("a = 1\r\n")
            self.assertNotEqual(rehearsal.tooling_digest(root), digest)
            module.write_text("a = 1\n")
            (root / rehearsal.TOOLING_DOCUMENTS[1]).write_text("ledger row")
            self.assertNotEqual(rehearsal.tooling_digest(root), digest)

    def test_rehearsal_configuration_has_no_client_or_authorization(self) -> None:
        block = {"capture_root": "/srv/capture", "capture_slot": "osu",
                 "route_roots": {"/Songs/": "/srv/capture/Songs"}, "other_roots": {},
                 "inside_files": {}, "fixture_values": {}}
        self.assertEqual(validate_golden_config(block, "linux", rehearsal=True), [])
        for key in ("authorization", "assets", "client_source"):
            self.assertIn(f"golden has unknown field {key!r}", validate_golden_config(
                {**block, key: {}}, "linux", rehearsal=True))
        self.assertIn("golden.authorization is required", validate_golden_config(block, "linux"))
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "run.json"
            path.write_text(json.dumps({"os": "linux", "client": "lazer", "poll_rate_ms": 150,
                                        "client_executable": "/x", "golden": block}))
            with self.assertRaisesRegex(ValueError, "client_executable is forbidden"):
                parity_config.load_config(str(path), rehearsal=True)


class SharedLogReleaseTests(unittest.TestCase):
    """Pilot 2: the client still held its `.nauth.log` open (WinError 32) at removal."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.logs = Path(self._tmp.name)
        self.before = golden_client.log_names(self.logs)
        (self.logs / "1795636944.nauth.log").write_text("this run")
        self.addCleanup(self._tmp.cleanup)

    def unlink_failing(self, times: int):
        real = Path.unlink
        calls = []

        def unlink(path: Path, *args, **kwargs) -> None:
            calls.append(path.name)
            if len(calls) <= times:
                raise PermissionError(32, "The process cannot access the file")
            real(path, *args, **kwargs)
        return mock.patch.object(Path, "unlink", unlink), calls

    def test_removal_waits_for_the_exit_and_retries_a_held_log(self) -> None:
        exits = iter([False, False, True])
        patch, calls = self.unlink_failing(2)
        with patch, mock.patch.object(golden_client.time, "sleep"):
            record = golden_client.remove_new_shared_logs(self.logs, self.before,
                                                          exited=lambda: next(exits))
        self.assertEqual([entry["name"] for entry in record["removed"]], ["1795636944.nauth.log"])
        self.assertEqual((record["locked"], len(calls)), ([], 3))
        self.assertEqual(golden_client.log_names(self.logs), set())

    def test_a_log_still_held_after_the_bound_is_an_error(self) -> None:
        patch, _calls = self.unlink_failing(10 ** 6)
        with patch, mock.patch.object(golden_client, "RELEASE_TIMEOUT_S", 0.2), \
                mock.patch.object(golden_client, "RELEASE_POLL_S", 0.01):
            record = golden_client.remove_new_shared_logs(self.logs, self.before)
        self.assertEqual(record["removed"], [])
        self.assertEqual([entry["name"] for entry in record["locked"]], ["1795636944.nauth.log"])
        client = gc.LiveClient.__new__(gc.LiveClient)
        client.artifact, client.process = {"pid": 1}, None
        client.shared_logs, client.shared_before = self.logs, self.before
        with patch, mock.patch.object(golden_client, "RELEASE_TIMEOUT_S", 0.2), \
                mock.patch.multiple(gc.daemon_mod, _close_windows_client_job=mock.DEFAULT,
                                    _terminate_client_descendants=mock.DEFAULT,
                                    _wait_for_client_descendants=mock.Mock(return_value=True),
                                    _assert_client_population=mock.Mock(return_value=False)):
            errors = client.stop()
        self.assertEqual(len(errors), 1)
        self.assertRegex(errors[0], "1795636944.nauth.log is still open after the client exited")


if __name__ == "__main__":
    unittest.main()
