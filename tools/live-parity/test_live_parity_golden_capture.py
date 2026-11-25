"""golden-capture orchestration (plan §5 P1) against local fakes and a real harness daemon.

No live game is used. The fake vosu serves real C0 envelopes (testdata/
golden-capture-envelopes.json, exported by the C0 encoder) and covers barrier
traversal and the streamCompanion generation advancing on every read. The real
debug test-harness daemon covers build attestation, the per-launch capability,
endpoint authorization, lifecycle and cleanup and not_ready no-client dumps.
"""
from __future__ import annotations

import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
sys.path.insert(0, str(HERE))
from live_parity import PINNED_SOURCE_REVISION, PINNED_VOSU_VERSION
import parity_build
import parity_golden
import parity_golden_capture as gc
from parity_golden_config import validate_golden_config
from parity_golden_rules import NON_GOLDEN_SEGMENTS
import route_matrix as rm
import route_security as rs

FAKE = HERE / "testdata" / "golden_capture_fake_daemon.py"
ENVELOPES = HERE / "testdata" / "golden-capture-envelopes.json"
PYTHON = str(Path(sys.executable).resolve())
CLIENT_VERSION = "2026.1118.0"
TRAVERSAL_CASES = (
    "state-http.json-v2.get.menu", "state-http.json-sc.get.menu", "state-http.json-sc.head.menu",
    "state-http.json-v2-precise.get.menu",
    "state-http.json-v2.get.paused", "state-http.json-sc.get.paused", "ws.v2.upgrade.paused",
    "state-http.json-v2.get.result", "state-http.json-sc.get.result",
    "state-http.json-v2.get.no-client", "state-http.json-sc.get.no-client",
)
NO_CLIENT_CASES = tuple(
    f"state-http.{route}.{method}.no-client"
    for route in ("json", "json-sc", "json-v2", "json-v2-precise") for method in ("get", "head")
) + ("calculation.no-query.no-client",)


def sha256(path: str) -> str:
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def head_commit() -> str:
    return subprocess.run(["git", "-C", str(REPO), "rev-parse", "HEAD"], check=True,
                          text=True, capture_output=True).stdout.strip()


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


HARNESS_BUILD_ENV = "VOSU_GOLDEN_HARNESS_BUILD"


def head_harness_binary() -> tuple[Path, str, list[str]]:
    """HEAD's debug test-harness vosu, built here from the clean committed tree.

    A prebuilt executable on the machine is never used: nothing attests that it was built
    from HEAD, and a stale one fails attestation for an unrelated reason. Returns the
    executable, the commit it was built from and its features; skips with the reason when
    the build is not requested or cannot be attributed to HEAD."""
    if os.environ.get(HARNESS_BUILD_ENV) != "1":
        raise unittest.SkipTest(
            f"set {HARNESS_BUILD_ENV}=1 to build HEAD's debug test-harness vosu "
            "(cargo build --locked --features test-harness) for this test; a prebuilt "
            "executable is never used because nothing attests it is HEAD's")
    cargo = shutil.which("cargo")
    if cargo is None:
        raise unittest.SkipTest("cargo is not on PATH, so HEAD's test-harness vosu cannot be built")
    try:
        commit = parity_build._clean_commit(REPO)
    except ValueError as error:
        raise unittest.SkipTest(f"{error}, so the built daemon would not be HEAD's") from None
    command, features = parity_build.harness_build_command(cargo)
    build = subprocess.run([*command, "--message-format=json-render-diagnostics"], cwd=REPO,
                           check=True, text=True, stdout=subprocess.PIPE)
    executables = [
        message["executable"] for message in map(json.loads, build.stdout.splitlines())
        if message.get("reason") == "compiler-artifact" and message.get("executable")
        and message["target"]["name"] == "vosu" and "bin" in message["target"]["kind"]]
    if len(executables) != 1:
        raise AssertionError(f"the harness build reported {len(executables)} vosu executables")
    if parity_build._clean_commit(REPO) != commit:
        raise AssertionError("the tree changed while HEAD's harness daemon was built")
    return Path(executables[0]).resolve(strict=True), commit, features


class FakeClient:
    """The scripted operator and client: each requested barrier becomes the fake state."""

    def __init__(self, state: Path, script: dict[str, str] | None = None,
                 exits: bool = True) -> None:
        self.state, self.script, self.exits = state, script or {}, exits
        self.gone = False
        self.requests: list[str] = []
        self.write("no-client")

    def write(self, barrier: str) -> None:
        temporary = self.state.with_suffix(".tmp")
        temporary.write_text(json.dumps({"barrier": barrier}))
        os.replace(temporary, self.state)

    def operator(self, barrier: str, message: str) -> None:
        if not message.startswith("waiting"):
            return
        self.requests.append(barrier)
        self.write(self.script.get(barrier, barrier))
        if barrier == "no-client" and self.exits:
            self.gone = True

    def start(self) -> dict:
        return {"mode": "fake"}

    def exited(self) -> bool:
        return self.gone

    def stop(self) -> list[str]:
        return []


class CaptureTestCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name).resolve()
        self.capture_root = self.tmp / "capture"
        for directory in ("osu/Songs", "lazer/files"):
            (self.capture_root / directory).mkdir(parents=True)
        self.state = self.tmp / "state.json"
        self.out = self.tmp / "evidence" / "capture.json"
        self.out.parent.mkdir()
        self.commit = head_commit()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def config(self, scenario: gc.ScenarioSet, *, vosu_command: list[str] | None = None,
               source_options: dict | None = None, vosu_options: dict | None = None,
               client_version: str = CLIENT_VERSION) -> dict:
        def fake(mode: str, options: dict | None) -> dict:
            return {"base_url": f"http://127.0.0.1:{free_port()}",
                    "command": [PYTHON, str(FAKE), mode],
                    "env": {"GOLDEN_FAKE_STATE": str(self.state),
                            "GOLDEN_FAKE_ENVELOPES": str(ENVELOPES),
                            "GOLDEN_FAKE_OPTIONS": json.dumps(options or {})}}
        vosu = fake("vosu", vosu_options)
        executable = PYTHON
        if vosu_command is not None:
            vosu = {"base_url": f"http://127.0.0.1:{free_port()}", "command": vosu_command}
            executable = vosu_command[0]
        return {
            "os": "linux", "client": "lazer", "poll_rate_ms": 150,
            "source_mode": "native-reference",
            "daemons": {"source": fake("upstream", source_options), "vosu": vosu},
            "_artifact_identity": {
                "source": {"mode": "native-reference", "executable": PYTHON,
                           "executable_sha256": sha256(PYTHON), "sha256": sha256(PYTHON)},
                "vosu": {"executable": executable, "sha256": sha256(executable)},
                "client": {"version": CLIENT_VERSION, "sha256": "c" * 64},
            },
            "golden": {
                "authorization": {"reference": "test-authorization", "platform": "linux-lazer",
                                  "source_revision": PINNED_SOURCE_REVISION,
                                  "vosu_commit": self.commit, "client_version": client_version,
                                  "scenario_set": scenario.id},
                "capture_root": str(self.capture_root), "capture_slot": "osu",
                "route_roots": {"/Songs/": str(self.capture_root / "osu" / "Songs")},
                "other_roots": {"files": str(self.capture_root / "lazer" / "files")},
                "inside_files": {}, "fixture_values": {}, "assets": [],
            },
        }

    def stub_build(self, calls: list | None = None):
        def build(cfg: dict, out: str) -> dict:
            if calls is not None:
                calls.append(out)
            return {"vosu": {"commit": self.commit, "version": PINNED_VOSU_VERSION,
                             "profile": "debug", "features": ["test-harness"],
                             "target": "x86_64-unknown-linux-gnu", "sha256": "d" * 64},
                    "owned_paths": []}
        return build

    def capture(self, scenario: gc.ScenarioSet, client: FakeClient, cfg: dict, *,
                build=None, timeout: float = 120.0) -> dict:
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            gc.golden_capture(cfg, scenario, str(self.out), timeout=timeout,
                              build=build or self.stub_build(), client=client,
                              operator=client.operator)
        return json.loads(self.out.read_text("utf-8"))

    def refuses(self, pattern: str, scenario: gc.ScenarioSet, client: FakeClient, cfg: dict,
                **kwargs) -> None:
        with self.assertRaisesRegex(gc.CaptureError, pattern):
            self.capture(scenario, client, cfg, **kwargs)
        self.assertFalse(self.out.exists(), "a refused run never writes the capture")
        self.assertFalse((self.capture_root / ".vosu-route-probe-root").exists())

    def golden(self, capture: Path) -> Path:
        candidate = self.tmp / "candidate"
        with contextlib.redirect_stdout(io.StringIO()):
            parity_golden.golden([str(capture)], str(candidate))
        return candidate

    def transcript(self) -> dict:
        return json.loads(self.out.with_name("capture.json.transcript.json").read_text("utf-8"))


class FakeTraversalTests(CaptureTestCase):
    def test_full_traversal_with_real_envelopes_is_accepted_by_golden(self) -> None:
        scenario = gc.ScenarioSet("test-traversal", gc.TRAVERSAL, TRAVERSAL_CASES)
        client = FakeClient(self.state)
        capture = self.capture(scenario, client, self.config(scenario))
        self.assertEqual(client.requests, list(gc.TRAVERSAL))
        self.assertEqual(capture["expected_cases"], [c.id for c in rm.MATRIX if c.id in TRAVERSAL_CASES])
        self.assertEqual([case["id"] for case in capture["cases"]], capture["expected_cases"])
        for case in capture["cases"]:
            attempt = case["attempts"][-1]
            before = attempt["vosu"]["dump_before"]["envelope"]
            after = attempt["vosu"]["dump_after"]["envelope"]
            self.assertEqual(case["dump_schema"], before["schema"])
            if before["schema"] == "sc" and case["barrier"] != "no-client":
                # The real sc provider advances per read; the sandwich still holds.
                self.assertLess(before["provider_generation"], after["provider_generation"])
                self.assertLess(before["derived"]["revision"], after["derived"]["revision"])
                self.assertEqual(before["digest"], after["digest"])
            else:
                self.assertEqual(before, after)
        self.assertEqual(oct(self.out.stat().st_mode & 0o777), "0o600")
        transcript = self.transcript()
        self.assertEqual([b["barrier"] for b in transcript["barriers"]], list(gc.TRAVERSAL))
        self.assertEqual(transcript["cleanup"]["errors"], [])
        candidate = self.golden(self.out)
        manifest = json.loads((candidate / "linux-lazer" / "manifest.json").read_text("utf-8"))
        self.assertEqual(manifest["case_counts"]["state-http"], 10)
        self.assertEqual(manifest["case_counts"]["ws"], 1)

    def test_retry_is_whole_case_and_attempts_are_never_mixed(self) -> None:
        scenario = gc.ScenarioSet("test-retry", ("menu",), ("state-http.json-v2-precise.get.menu",))
        cfg = self.config(scenario, vosu_options={"flaky_dump_reads": {"v2-precise": [1]}})
        capture = self.capture(scenario, FakeClient(self.state), cfg)
        (case,) = capture["cases"]
        self.assertEqual(case["attempts"][0], {
            "attempt": 1, "outcome": "failed",
            "reason": "vosu dump sandwich differs in provider_generation"})
        complete = case["attempts"][1]
        records = [*complete["source"], complete["vosu"]["route"],
                   complete["vosu"]["dump_before"], complete["vosu"]["dump_after"]]
        self.assertEqual({record["attempt"] for record in records}, {2})
        self.assertEqual([r["attempt"] for r in self.transcript()["cases"]], [1, 2])
        # A record from the failed attempt can never stand in the complete one.
        mixed = json.loads(self.out.read_text("utf-8"))
        mixed["cases"][0]["attempts"][1]["source"][1]["attempt"] = 1
        path = self.tmp / "mixed.json"
        path.write_text(json.dumps(mixed))
        with self.assertRaisesRegex(parity_golden.GoldenError, "never mixed"):
            self.golden(path)

    def test_three_failed_attempts_leave_no_capture(self) -> None:
        scenario = gc.ScenarioSet("test-exhausted", ("menu",), ("state-http.json-v2-precise.get.menu",))
        cfg = self.config(scenario, vosu_options={"flaky_dump_reads": {"v2-precise": [1, 3, 5]}})
        self.refuses("failed 3 whole-case attempts", scenario, FakeClient(self.state), cfg)
        self.assertTrue(self.transcript()["result"].startswith("failed"))

    def test_unstable_upstream_never_reaches_a_barrier(self) -> None:
        # A non-volatile value that changes on every read never yields identical barrier
        # views, so no case is ever recorded (upstream sandwich mismatches of non-volatile
        # bytes are covered by test_live_parity_volatile.LedgerTests).
        scenario = gc.ScenarioSet("test-unstable", ("menu",), ("state-http.json-v2.get.menu",))
        cfg = self.config(scenario, source_options={"unstable_upstream": True})
        self.refuses("barrier menu was not reached", scenario, FakeClient(self.state), cfg,
                     timeout=5.0)

    def test_caseless_play_barrier_passes_with_churning_views(self) -> None:
        # Active play has no cases: its advancing clock and provider generation never
        # stop it. The case-bearing barriers around it still need identical views.
        scenario = gc.ScenarioSet("test-play-churn", ("menu", "play", "paused"),
                                  ("state-http.json-v2.get.menu", "state-http.json-v2.get.paused"))
        churn = {"unstable_upstream": ["play"]}, {"churn_generation": ["play"]}
        cfg = self.config(scenario, source_options=churn[0], vosu_options=churn[1])
        capture = self.capture(scenario, FakeClient(self.state), cfg)
        self.assertEqual([case["id"] for case in capture["cases"]], list(scenario.cases))
        self.assertEqual([(b["barrier"], b["identical_views"]) for b in self.transcript()["barriers"]],
                         [("menu", True), ("play", False), ("paused", True)])

    def test_churning_views_never_reach_a_barrier_with_cases(self) -> None:
        scenario = gc.ScenarioSet("test-paused-churn", ("play", "paused"),
                                  ("state-http.json-v2.get.paused",))
        cfg = self.config(scenario, source_options={"unstable_upstream": ["paused"]},
                          vosu_options={"churn_generation": ["paused"]})
        self.refuses("barrier paused was not reached", scenario, FakeClient(self.state), cfg,
                     timeout=8.0)

    def test_barrier_out_of_order_is_refused(self) -> None:
        scenario = gc.ScenarioSet("test-order", gc.TRAVERSAL, TRAVERSAL_CASES)
        client = FakeClient(self.state, script={"play": "paused"})
        self.refuses("barrier out of order", scenario, client, self.config(scenario))
        self.out.with_name("capture.json.transcript.json").unlink()
        early_exit = FakeClient(self.state, script={"paused": "no-client"})
        self.refuses("barrier out of order", scenario, early_exit, self.config(scenario))

    def test_process_exit_must_be_observed(self) -> None:
        scenario = gc.ScenarioSet("test-exit", ("menu", "no-client"),
                                  ("state-http.json-v2.get.menu", "state-http.json-v2.get.no-client"))
        client = FakeClient(self.state, exits=False)
        self.refuses("no-client was not reached", scenario, client, self.config(scenario),
                     timeout=8)

    def test_capability_leak_writes_nothing(self) -> None:
        scenario = gc.ScenarioSet("test-leak", ("menu",), ("state-http.json-sc.get.menu",))
        cfg = self.config(scenario, vosu_options={"leak_capability": "sc"})
        self.refuses("capability leaked", scenario, FakeClient(self.state), cfg)
        self.assertEqual(list(self.out.parent.iterdir()), [])

    def test_build_without_harness_endpoint_is_refused(self) -> None:
        scenario = gc.ScenarioSet("test-release", ("menu",), ("state-http.json-v2.get.menu",))
        cfg = self.config(scenario, vosu_options={"no_harness": True})
        self.refuses("release or featureless build", scenario, FakeClient(self.state), cfg)

    def test_authorization_must_match_before_anything_runs(self) -> None:
        scenario = gc.ScenarioSet("test-auth", ("menu",), ("state-http.json-v2.get.menu",))
        calls: list = []
        cfg = self.config(scenario, client_version="2026.1117.0")
        self.refuses("authorization.client_version", scenario, FakeClient(self.state), cfg,
                     build=self.stub_build(calls))
        self.assertEqual(calls, [])
        self.out.with_name("capture.json.transcript.json").unlink()
        cfg = self.config(scenario)
        cfg["golden"]["authorization"]["vosu_commit"] = "0" * 40
        self.refuses("authorization.vosu_commit", scenario, FakeClient(self.state), cfg)

    def test_orphan_processes_are_killed_and_survivors_fail_the_run(self) -> None:
        scenario = gc.ScenarioSet("test-orphan", ("menu",), ("state-http.json-v2.get.menu",))
        pid_file = self.tmp / "orphan.pid"

        def run(options: dict) -> dict:
            for path in (self.out, self.out.with_name("capture.json.transcript.json"), pid_file):
                path.unlink(missing_ok=True)
            return self.config(scenario, source_options={"orphan_pid_file": str(pid_file),
                                                         **options})

        # A same-group child and one that left the group via setsid are both killed:
        # the process group and the recorded process tree are each checked.
        for options in ({}, {"orphan_setsid": True}):
            self.capture(scenario, FakeClient(self.state), run(options))
            self.assertFalse(Path(f"/proc/{pid_file.read_text()}").exists(), options)
        for options, patched in (({}, ("_kill_group", "_kill_tree")),
                                 ({"orphan_setsid": True}, ("_kill_tree",))):
            cfg = run(options)
            try:
                with contextlib.ExitStack() as stack:
                    for name in patched:
                        stack.enter_context(mock.patch.object(gc, name))
                    self.refuses("orphan process survived source daemon cleanup", scenario,
                                 FakeClient(self.state), cfg)
            finally:
                with contextlib.suppress(ProcessLookupError, FileNotFoundError):
                    os.kill(int(pid_file.read_text()), signal.SIGKILL)

    def test_closed_connection_case_is_recorded_not_failed_or_dropped(self) -> None:
        case_id = "state-http.json-v2.near.trailing-slash"
        self.assertIn("closed", rm.CASES[case_id].outcomes)
        scenario = gc.ScenarioSet("test-closed", ("menu",), (case_id, "state-http.json-v2.get.menu"))
        close = {"close_paths": ["/json/v2/"]}
        capture = self.capture(scenario, FakeClient(self.state),
                               self.config(scenario, source_options=close, vosu_options=close))
        self.assertEqual(sorted(capture["expected_cases"]),
                         sorted([case_id, "state-http.json-v2.get.menu"]))
        (closed,) = [case for case in capture["cases"] if case["id"] == case_id]
        self.assertEqual(len(closed["attempts"]), 1)
        attempt = closed["attempts"][0]
        for record in (*attempt["source"], attempt["vosu"]["route"]):
            self.assertEqual((record["outcome"], sorted(record)),
                             ("closed", ["attempt", "outcome", "request"]))
        candidate = self.golden(self.out)
        written = json.loads((candidate / "linux-lazer" / "cases" / f"{case_id}.json")
                             .read_text("utf-8"))
        self.assertEqual(written["expected"], {"outcome": "closed"})


class PlanTests(unittest.TestCase):
    def test_incomplete_case_set_and_platform_exclusions(self) -> None:
        context = rm.MatrixContext("linux-lazer", {})
        with self.assertRaisesRegex(gc.CaptureError, "incomplete case set"):
            gc.plan_cases(gc.SCENARIO_SETS["m4b-full-v1"], context)
        golden, _others, excluded = gc.plan_cases(gc.SCENARIO_SETS["m4b-dry-run-v1"], context)
        self.assertEqual([item.case.id for item in golden],
                         list(gc.SCENARIO_SETS["m4b-dry-run-v1"].cases))
        self.assertEqual(excluded, [])
        authority = gc.exclusion_authority
        self.assertIn("lazer-legacy-skin-404",
                      authority("files.skin.file.get: requires stable client", "linux-lazer"))
        self.assertIn("lazer-legacy-skin-404", authority(
            "path-security.skin.encoded-parent: no declared root for /files/skin/ on windows-lazer",
            "windows-lazer"))
        for reason, platform in (
                ("path-security.skin.encoded-parent: no declared root for /files/skin/ on "
                 "windows-stable", "windows-stable"),
                # The lazer files store is the declared root of its legacy file routes.
                ("path-security.songs.encoded-parent: no declared root for /Songs/ on "
                 "windows-lazer", "windows-lazer"),
                ("path-security.beatmap.symlink: symlink fixture unavailable: OSError",
                 "linux-lazer"),
                ("calculation.relative-path: fixture value calc_relative not provided",
                 "linux-lazer")):
            self.assertIsNone(authority(reason, platform), reason)
        self.assertIsNotNone(authority(
            "path-security.beatmap.symlink: symlink fixture unavailable: OSError", "windows-lazer"))

    def test_full_lazer_plan_runs_the_legacy_file_routes_from_the_files_store(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            capture = Path(tmp).resolve() / "capture"
            files = capture / "lazer" / "files"
            chart = files / "ab" / "abcd"
            chart.parent.mkdir(parents=True)
            chart.write_text("osu file format v14\n")
            source = Path(tmp) / "sentinel-source.osu"
            source.write_text("osu file format v14\n")
            layout = rs.prepare_security_layout(
                str(capture), {"/Songs/": str(files), "/files/beatmap/": str(files)}, {},
                str(source), {"files": str(files)})
            try:
                values = {"songs_file": "ab/abcd", "songs_dir": "ab", "beatmap_relative": "abcd",
                          "skin_file": "skin.ini", "calc_relative": "ab/abcd",
                          "calc_absolute_inside": str(chart)}
                golden, others, excluded = gc.plan_cases(
                    gc.SCENARIO_SETS["m4b-full-v1"],
                    rm.MatrixContext("linux-lazer", values, layout))
            finally:
                rs.remove_security_layout(layout)
        ids = {item.case.id for item in golden}
        self.assertTrue({"files.beatmap.relative.get", "files.songs.file.get",
                         "path-security.beatmap.symlink", "path-security.songs.encoded-parent",
                         "files.skin.lazer-file.get"} <= ids)
        self.assertFalse({item.case.id for item in others} & ids)
        self.assertTrue(all(rm.golden_eligible(item.case) for item in golden))
        # Every remaining exclusion names its contract or ledger authority.
        self.assertTrue(all(authority for _case, _reason, authority in excluded))
        self.assertEqual(sorted(case for case, _reason, _authority in excluded), sorted(
            ["files.skin.file.get", "files.skin.file.head"]
            + [c.id for c in rm.MATRIX if c.id.startswith("path-security.skin.")]
            + [c.id for c in rm.MATRIX if c.security and "windows" in
               rs.SPELLING_BY_NAME[c.security[1]].oses
               and "linux" not in rs.SPELLING_BY_NAME[c.security[1]].oses
               and not c.id.startswith("path-security.skin.")]))

    def test_role_segments_match_the_matrix_roles(self) -> None:
        # golden and adapter_golden refuse `negative`/`diagnostic` ID segments; that
        # is exactly the set of non-golden matrix roles.
        for case in rm.MATRIX:
            segments = set(case.id.split("."))
            self.assertEqual(rm.golden_eligible(case),
                             not segments & NON_GOLDEN_SEGMENTS, case.id)

    def test_process_tree_follows_parents_and_ignores_reused_pids(self) -> None:
        # pid -> (parent, creation); the parent-PID walk used on Windows and Linux.
        table = {10: (1, 100), 11: (10, 110), 12: (11, 120), 13: (10, 90), 14: (2, 130)}
        self.assertEqual(gc.descendants(table, {10: 100}), {10: 100, 11: 110, 12: 120})
        # A survivor reparented after its daemon died is still in the recorded tree.
        alive = {11: 110}
        with mock.patch.object(gc, "_process_table", return_value={11: (1, 110)}), \
                mock.patch.object(gc, "_alive", side_effect=lambda pid, c: alive.get(pid) == c), \
                mock.patch.object(gc, "_kill_tree") as kill, \
                mock.patch.object(gc.time, "sleep"), \
                mock.patch.object(gc.time, "monotonic", side_effect=[0, 10]):
            self.assertEqual(gc._tree_survivors({10: 100, 11: 110}), {11: 110})
        kill.assert_called_once_with({11: 110})

    def test_expected_cases_are_cross_checked_against_the_matrix(self) -> None:
        context = rm.MatrixContext("linux-lazer", {})
        scenario = gc.SCENARIO_SETS["m4b-dry-run-v1"]
        expected = list(scenario.cases)
        gc.check_expected_cases(expected, scenario, context)
        for wrong in (expected[:-1], [*expected, "state-http.negative.api-v1-suffix"],
                      list(reversed(expected))):
            with self.assertRaises(gc.CaptureError):
                gc.check_expected_cases(wrong, scenario, context)
        with self.assertRaisesRegex(gc.CaptureError, "non-golden"):
            gc.plan_cases(gc.ScenarioSet("x", ("menu",), ("state-http.negative.api-v1-suffix",)),
                          context)

    def test_scenarios_follow_the_contract_barrier_order(self) -> None:
        self.assertEqual(gc.SCENARIO_SETS["m4b-full-v1"].barriers,
                         ("menu", "play", "paused", "result", "no-client"))
        with self.assertRaises(ValueError):
            gc.ScenarioSet("bad", ("paused", "menu"))

    def test_dump_schema_follows_the_route_adapter(self) -> None:
        self.assertEqual(gc.dump_schema(rm.CASES["state-http.json-sc.get.menu"]), "sc")
        self.assertEqual(gc.dump_schema(rm.CASES["ws.tokens.upgrade.menu"]), "sc")
        self.assertEqual(gc.dump_schema(rm.CASES["ws.ws.upgrade.menu"]), "v1")
        self.assertEqual(gc.dump_schema(rm.CASES["files.beatmap.file.get"]), "v2")

    def test_golden_config_block_is_strict(self) -> None:
        self.assertIn("golden.capture_root is required", validate_golden_config({}, "linux"))
        block = {"authorization": {key: "x" for key in (
                     "reference", "platform", "source_revision", "vosu_commit",
                     "client_version", "scenario_set")},
                 "capture_root": "/srv/capture", "capture_slot": "osu",
                 "route_roots": {"/Songs/": "/srv/capture/Songs"}, "other_roots": {},
                 "inside_files": {}, "fixture_values": {}, "assets": []}
        self.assertEqual(validate_golden_config(block, "linux"), [])
        for key, value, message in (
                ("route_roots", {"/Songz/": "/srv/x"}, "unknown key"),
                ("capture_root", "relative/root", "absolute normalized"),
                ("fixture_values", {"songs_file": ""}, "non-empty"),
                ("extra", 1, "unknown field")):
            self.assertTrue(any(message in error for error in validate_golden_config(
                {**block, key: value}, "linux")), key)


class HarnessAttestationTests(unittest.TestCase):
    def test_release_or_featureless_executable_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            fake = Path(tmp) / "vosu"
            fake.write_bytes(b"\x7fELF" + PINNED_VOSU_VERSION.encode() + b"\0release build")
            with self.assertRaisesRegex(ValueError, "no test-harness adapter capture endpoint"):
                parity_build.attest_harness_binary(
                    fake, commit="a" * 40, features=["test-harness"], target="x86_64-unknown-linux-gnu")
            fake.write_bytes(fake.read_bytes() + b"".join(parity_build.HARNESS_MARKERS))
            record = parity_build.attest_harness_binary(
                fake, commit="a" * 40, features=["test-harness", "linux-process-events"],
                target="x86_64-unknown-linux-gnu")
            self.assertEqual((record["profile"], record["features"]),
                             ("debug", ["linux-process-events", "test-harness"]))
            with self.assertRaisesRegex(ValueError, "do not include test-harness"):
                parity_build.attest_harness_binary(
                    fake, commit="a" * 40, features=[], target="x86_64-unknown-linux-gnu")

    def test_harness_build_command_is_the_contract_command(self) -> None:
        command, features = parity_build.harness_build_command("cargo")
        self.assertNotIn("--release", command)
        expected = (["cargo", "build", "--locked", "--no-default-features", "--features",
                     "test-harness"] if sys.platform == "win32"
                    else ["cargo", "build", "--locked", "--features", "test-harness"])
        self.assertEqual(command, expected)
        self.assertIn("test-harness", features)


@unittest.skipUnless(sys.platform.startswith("linux"), "the local end-to-end runs on Linux")
class RealHarnessDaemonTests(CaptureTestCase):
    """HEAD's `cargo build --locked --features test-harness` vosu daemon, built by the test."""

    def test_end_to_end_with_the_real_harness_daemon(self) -> None:
        binary, commit, features = head_harness_binary()
        self.assertEqual(commit, self.commit)
        scenario = gc.ScenarioSet("test-real-daemon", ("no-client",), NO_CLIENT_CASES)
        cfg = self.config(scenario, vosu_command=[str(binary)])
        attested = []

        def build(cfg: dict, out: str) -> dict:
            record = parity_build.attest_harness_binary(
                binary, commit=commit, features=features, target="x86_64-unknown-linux-gnu")
            attested.append(record)
            return {"vosu": record, "owned_paths": []}

        client = FakeClient(self.state)
        client.gone = True
        capture = self.capture(scenario, client, cfg, build=build)
        self.assertEqual(capture["attestation"]["vosu"]["sha256"], sha256(str(binary)))
        self.assertEqual(len(capture["cases"]), len(NO_CLIENT_CASES))
        for case in capture["cases"]:
            envelope = case["attempts"][-1]["vosu"]["dump_before"]["envelope"]
            self.assertEqual((envelope["http"]["kind"], envelope["session"]), ("not_ready", None))
        transcript = self.transcript()
        self.assertEqual(transcript["harness_endpoint"], {
            "with_capability": 200, "without_capability": 404, "wrong_capability": 404})
        self.assertIn(gc.CAPABILITY_ENV, transcript["launch"]["vosu"]["environment_keys"])
        self.assertNotIn(gc.CAPABILITY_ENV, transcript["launch"]["source"]["environment_keys"])
        pids = [transcript["launch"][name]["pid"] for name in ("source", "vosu")]
        for pid in pids:
            self.assertFalse(Path(f"/proc/{pid}").exists(), "owned daemon left running")
        self.assertEqual(sorted(p.name for p in self.capture_root.iterdir()), ["lazer", "osu"])
        self.assertEqual(sorted(p.name for p in (self.capture_root / "osu/Songs").iterdir()), [])
        candidate = self.golden(self.out)
        self.assertTrue((candidate / "linux-lazer" / "cases"
                         / "state-http.json-v2.head.no-client.json").is_file())


if __name__ == "__main__":
    unittest.main()
