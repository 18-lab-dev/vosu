from __future__ import annotations

import base64
import contextlib
import copy
import hashlib
import io
import json
import re
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from live_parity import PINNED_SOURCE_REVISION, PINNED_VOSU_VERSION
import parity_golden as golden_mod
import parity_golden_ledger as golden_ledger
from parity_golden_rules import (
    NORMALIZATION_VERSION, RULE_IDS, GoldenError, Root, RootRewriter, apply_timestamp_pointers,
    check_approved_rules, rewrite_cbor_text,
)

PIN = PINNED_SOURCE_REVISION
CONTRACT = (f"Normalization version: {NORMALIZATION_VERSION}\n\n| Rule ID | Scope |\n|---|---|\n"
            + "".join(f"| `{rule}` | exact |\n" for rule in RULE_IDS))
WIN_ROOT = "C:\\Users\\vosu\\AppData\\Local\\osulazer\\"
SYN_ROOT = "C:\\vosu-golden\\osu!\\"
DATE_A = "Mon, 24 Nov 2026 10:00:00 GMT"
DATE_B = "Mon, 24 Nov 2026 10:00:07 GMT"
HASH = "a" * 64
COMMIT = "b" * 40


def text(value: str) -> dict[str, str]:
    return {"text": value}


def request(method: str = "GET", target: str = "/json/v2", *extra: str) -> str:
    return "\r\n".join([f"{method} {target} HTTP/1.1", "Host: 127.0.0.1:24050", *extra]) + "\r\n\r\n"


def cbor_text(value: str) -> bytes:
    data = value.encode()
    assert len(data) < 256
    return (bytes([0x60 | len(data)]) if len(data) < 24 else bytes([0x78, len(data)])) + data


def envelope(body: str = '{"ok":true}', schema: str = "v2", generation: int = 3,
             entries: list | None = None) -> dict:
    session = {"ordinal": 0, "generation": 1, "owner": "vosu.osu", "owner_instance": 0,
               "game": "osu", "variant": "lazer", "foreground": True}
    value = {
        "format": "vosu-adapter-capture-v1", "schema": schema, "provider_generation": generation,
        "session": session, "focus_revision": 1,
        "roster": {"revision": 1, "sessions": [copy.deepcopy(session)]},
        "bus_revision": 7, "namespace_revisions": [["vosu.osu", 7]], "entries": entries or [],
        "derived": {"revision": 0, "status": "none"},
        "http": {"kind": "ready", "status": 200, "body_utf8": body}, "digest": "",
    }
    value["digest"] = golden_mod.envelope_digest(value)
    return value


def response(body: dict | None = None, headers: list | None = None, status: str = "HTTP/1.1 200 OK",
             frames: list | None = None) -> dict:
    value = {"status_line": status, "headers": headers if headers is not None else [],
             "body": body if body is not None else text("")}
    if frames is not None:
        value["frames"] = frames
    return value


def case(case_id: str, req: str, upstream: tuple[dict, dict], *, vosu: dict | None = None,
         dump: dict | None = None, dump_after: dict | None = None, schema: str = "v2",
         conditional: str | None = None, route_request: str | None = None,
         failed_attempts: int = 0) -> dict:
    number = failed_attempts + 1

    def record(resp: dict, req_text: str) -> dict:
        return {"attempt": number, "request": text(req_text), **copy.deepcopy(resp)}

    dump = dump or envelope(schema=schema)
    attempts = [{"attempt": n, "outcome": "failed", "reason": "barrier lost"}
                for n in range(1, number)]
    attempts.append({
        "attempt": number, "outcome": "complete",
        "source": [record(upstream[0], req), record(upstream[1], req)],
        "vosu": {
            "dump_before": {"attempt": number, "envelope": dump},
            "route": record(vosu or upstream[0], route_request or req),
            "dump_after": {"attempt": number, "envelope": copy.deepcopy(dump_after or dump)},
        },
    })
    return {"id": case_id, "family": case_id.split(".", 1)[0], "barrier": "menu",
            "dump_schema": schema, "conditional": conditional, "attempts": attempts}


def state_case(body: str = '{"ok":true}', case_id: str = "state-http.get-json-v2") -> dict:
    up = response(text(body), [["Date", DATE_A], ["Content-Type", "application/json"],
                               ["Content-Length", str(len(body.encode()))], ["X-Powered-By", "x"]])
    up_b = copy.deepcopy(up)
    up_b["headers"][0][1] = DATE_B
    return case(case_id, request(), (up, up_b), dump=envelope(body))


def capture(cases: list[dict], **overrides) -> dict:
    value = {
        "format": "vosu-golden-capture-v1", "source_revision": PIN, "platform": "windows-lazer",
        "scenario_set": "pilot-1",
        "authorization": {"reference": "owner-2026-11-24-pilot", "platform": "windows-lazer",
                          "source_revision": PIN, "vosu_commit": COMMIT,
                          "client_version": "2026.1118.0", "scenario_set": "pilot-1"},
        "attestation": {
            "source": {"revision": PIN, "kind": "source", "sha256": HASH},
            "vosu": {"commit": COMMIT, "version": PINNED_VOSU_VERSION, "profile": "debug",
                     "features": ["test-harness"], "target": "x86_64-pc-windows-msvc",
                     "sha256": HASH},
            "client": {"variant": "lazer", "version": "2026.1118.0", "sha256": HASH},
        },
        "roots": [{"slot": "osu!", "private": WIN_ROOT}],
        "assets": [],
        "expected_cases": [item["id"] for item in cases],
        "cases": cases,
    }
    value.update(overrides)
    return value


class GoldenTestCase(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.repo = self.root / "repo"
        (self.repo / "tests/fixtures/upstream").mkdir(parents=True)
        (self.repo / "Cargo.toml").write_text(f'[package.metadata]\nsource-commit = "{PIN}"\n')
        self.contract = self.repo / "tests/fixtures/upstream/GOLDEN_CONTRACT.md"
        self.contract.write_text(CONTRACT)
        self.count = 0

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def run_golden(self, *captures: dict, candidate: Path | None = None, **kwargs) -> Path:
        paths = []
        for value in captures:
            self.count += 1
            path = self.root / f"capture-{self.count}.json"
            path.write_text(json.dumps(value))
            paths.append(str(path))
        candidate = candidate or self.root / f"candidate-{self.count}"
        with contextlib.redirect_stdout(io.StringIO()):
            golden_mod.golden(paths, str(candidate), str(self.contract), repo_root=self.repo, **kwargs)
        return candidate

    def rejects(self, value: dict, message: str) -> None:
        candidate = self.root / "rejected"
        with self.assertRaisesRegex(GoldenError, message):
            self.run_golden(value, candidate=candidate)
        self.assertFalse(candidate.exists(), "a rejected capture must not publish a candidate")
        self.assertEqual([], list(self.root.glob(".golden-candidate-*")))

    def case_file(self, candidate: Path, case_id: str, platform: str = "windows-lazer") -> dict:
        return json.loads((candidate / platform / "cases" / f"{case_id}.json").read_text("utf-8"))


class CandidateLayoutTests(GoldenTestCase):
    def test_outside_calculation_subtracts_only_the_evidenced_record_parts(self) -> None:
        case_id = "calculation.absolute-outside-root"
        target = "/api/calculate/pp?path=" + WIN_ROOT + "probe\\sentinel.osu"
        success = '{"pp":1}'
        up = response(text(success), [["Content-Type", "application/json"],
                                      ["Content-Length", str(len(success))]])
        rejection = '{"error":"invalid beatmap path: outside declared roots or unsafe spelling"}'
        rejected = response(text(rejection), [["Content-Type", "application/json"],
                                               ["Content-Length", str(len(rejection))]],
                            status="HTTP/1.1 400 Bad Request")
        probe = case(case_id, request("GET", target), (up, up), vosu=rejected)
        candidate = self.run_golden(capture([probe]))
        expected = self.case_file(candidate, case_id)["expected"]
        self.assertEqual((expected["status"], expected["body"]), (200, text(success)))
        rules = json.loads((candidate / "divergence-rules.json").read_text())["rules"]
        self.assertEqual({rule["part"]: rule["vosu_value"] for rule in rules},
                         {"status": 400, "body": rejection, "headers.content-length": "75"})
        for rule in rules:
            self.assertEqual(rule["ledger_id"], "legacy-calculate-declared-roots")
            self.assertEqual(rule["platforms"], ["windows-lazer"])
            self.assertEqual(rule["cases"], [case_id])
        inventory = json.loads((candidate / "inventory.json").read_text())["files"]
        self.assertIn("divergence-rules.json", {entry["path"] for entry in inventory})
        near = dict(probe, id="calculation.absolute-inside-root-osu")
        other = self.run_golden(capture([near]))
        self.assertFalse((other / "divergence-rules.json").exists())

    def test_writes_deterministic_inventoried_candidate(self) -> None:
        value = capture([state_case()])
        first = self.run_golden(value)
        second = self.run_golden(value)
        files = sorted(p.relative_to(first).as_posix() for p in first.rglob("*") if p.is_file())
        self.assertEqual(files, sorted(
            p.relative_to(second).as_posix() for p in second.rglob("*") if p.is_file()))
        for rel in files:
            self.assertEqual((first / rel).read_bytes(), (second / rel).read_bytes(), rel)
        self.assertEqual([
            "inventory.json", "manifest.json", "windows-lazer/assets.json",
            "windows-lazer/brand-inventory.txt", "windows-lazer/cases/state-http.get-json-v2.json",
            "windows-lazer/inventory.json", "windows-lazer/manifest.json"], files)
        manifest = json.loads((first / "manifest.json").read_text())
        self.assertEqual((PIN, "captured", ["windows-lazer"]),
                         (manifest["source_revision"], manifest["provenance"], manifest["platforms"]))
        inventory = json.loads((first / "inventory.json").read_text())
        self.assertEqual(PIN, inventory["source_revision"])
        for entry in inventory["files"]:
            data = (first / entry["path"]).read_bytes()
            self.assertEqual((entry["bytes"], entry["sha256"]),
                             (len(data), hashlib.sha256(data).hexdigest()))
        self.assertNotIn("inventory.json", [entry["path"] for entry in inventory["files"]])
        platform = json.loads((first / "windows-lazer/manifest.json").read_text())
        self.assertEqual({"osu!": SYN_ROOT}, platform["synthetic_roots"])
        self.assertEqual(1, platform["case_counts"]["state-http"])
        self.assertEqual("clean", json.loads(
            (first / "windows-lazer/inventory.json").read_text())["privacy_scan"]["result"])

    def test_expected_keeps_status_and_compared_headers_only(self) -> None:
        candidate = self.run_golden(capture([state_case()]))
        expected = self.case_file(candidate, "state-http.get-json-v2")["expected"]
        self.assertEqual(200, expected["status"])
        self.assertEqual([["content-length", "11"], ["content-type", "application/json"],
                          ["date", "<golden:date>"]], expected["headers"])
        self.assertEqual({"text": '{"ok":true}'}, expected["body"])

    def test_refuses_non_empty_directory_and_committed_fixture_tree(self) -> None:
        busy = self.root / "busy"
        busy.mkdir()
        (busy / "keep.json").write_text("{}")
        with self.assertRaisesRegex(GoldenError, "not an empty directory"):
            self.run_golden(capture([state_case()]), candidate=busy)
        self.assertEqual("{}", (busy / "keep.json").read_text())
        committed = self.repo / "tests/fixtures/upstream" / PIN
        with self.assertRaisesRegex(GoldenError, "committed fixture tree"):
            self.run_golden(capture([state_case()]), candidate=committed)
        self.assertFalse(committed.exists())

    def test_refuses_directory_with_git_tracked_paths(self) -> None:
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        tracked = self.repo / "evidence/old-candidate"
        tracked.mkdir(parents=True)
        (tracked / "manifest.json").write_text("{}")
        subprocess.run(["git", "-C", str(self.repo), "add", "evidence"], check=True)
        (tracked / "manifest.json").unlink()  # Empty on disk, but still a tracked fixture path.
        with self.assertRaisesRegex(GoldenError, "git-tracked"):
            self.run_golden(capture([state_case()]), candidate=tracked)
        self.assertEqual([], list(tracked.iterdir()))

    def test_empty_existing_directory_is_replaced_atomically(self) -> None:
        empty = self.root / "empty"
        empty.mkdir()
        self.run_golden(capture([state_case()]), candidate=empty)
        self.assertTrue((empty / "manifest.json").is_file())

    def test_synthetic_provenance_labels_both_manifests_and_nothing_else(self) -> None:
        value = capture([state_case()])
        captured = self.run_golden(value)
        synthetic = self.run_golden(value, provenance="synthetic")
        for manifest in ("manifest.json", "windows-lazer/manifest.json"):
            self.assertEqual("captured", json.loads((captured / manifest).read_text())["provenance"])
            self.assertEqual("synthetic", json.loads((synthetic / manifest).read_text())["provenance"])
        # The label is the only difference; the inventories cover the relabelled manifests.
        case = "windows-lazer/cases/state-http.get-json-v2.json"
        self.assertEqual((captured / case).read_bytes(), (synthetic / case).read_bytes())
        inventory = json.loads((synthetic / "windows-lazer/inventory.json").read_text())
        manifest = (synthetic / "windows-lazer/manifest.json").read_bytes()
        self.assertIn({"path": "manifest.json", "bytes": len(manifest),
                       "sha256": hashlib.sha256(manifest).hexdigest()}, inventory["files"])
        with self.assertRaisesRegex(GoldenError, "provenance must be"):
            self.run_golden(value, candidate=self.root / "unlabelled", provenance="upstream")
        self.assertFalse((self.root / "unlabelled").exists())

    def test_contract_table_must_equal_implemented_rules(self) -> None:
        check_approved_rules(CONTRACT)
        missing = CONTRACT.replace("| `client-timestamp-pointer` | exact |\n", "")
        with self.assertRaisesRegex(GoldenError, "unapproved"):
            check_approved_rules(missing)
        with self.assertRaisesRegex(GoldenError, "unimplemented"):
            check_approved_rules(CONTRACT + "| `any-time-key` | every time key |\n")
        # A contract still at the previous version is refused.
        stale = NORMALIZATION_VERSION - 1
        with self.assertRaisesRegex(GoldenError, f"version {stale} differs"):
            check_approved_rules(CONTRACT.replace(
                f"version: {NORMALIZATION_VERSION}", f"version: {stale}"))

    def test_cli_writes_candidate_outside_repository(self) -> None:
        path = self.root / "cli-capture.json"
        path.write_text(json.dumps(capture([state_case()])))
        candidate = self.root / "cli-candidate"
        result = subprocess.run(
            [sys.executable, str(HERE / "run.py"), "golden", "--capture", str(path),
             "--candidate-dir", str(candidate), "--approved-rules", str(self.contract)],
            capture_output=True, text=True, check=False)
        self.assertEqual(0, result.returncode, result.stderr)
        summary = json.loads(result.stdout)
        self.assertEqual(hashlib.sha256((candidate / "inventory.json").read_bytes()).hexdigest(),
                         summary["inventory_sha256"])
        self.assertEqual(1, summary["platforms"]["windows-lazer"]["cases"])
        self.assertNotIn(WIN_ROOT.lower(), result.stdout.lower())


class CaptureValidationTests(GoldenTestCase):
    def test_rejects_wrong_pin_platform_and_unattested_builds(self) -> None:
        self.rejects(capture([state_case()], source_revision="c" * 40), "wrong pin")
        self.rejects(capture([state_case()], platform="linux-stable"), "not a supported platform")
        release = capture([state_case()])
        release["attestation"]["vosu"]["profile"] = "release"
        self.rejects(release, "debug test-harness build")
        featureless = capture([state_case()])
        featureless["attestation"]["vosu"]["features"] = []
        self.rejects(featureless, "debug test-harness build")
        unapproved = capture([state_case()])
        unapproved["authorization"]["client_version"] = "2026.1001.0"
        self.rejects(unapproved, "owner authorization")
        unhashed = capture([state_case()])
        unhashed["attestation"]["client"]["sha256"] = "not-a-hash"
        self.rejects(unhashed, "client.sha256")

    def test_rejects_incomplete_and_mixed_attempts(self) -> None:
        self.rejects(capture([state_case()], expected_cases=["state-http.get-json-v2",
                                                             "state-http.head-json-v2"]),
                     "incomplete")
        failed_last = state_case()
        failed_last["attempts"][-1] = {"attempt": 1, "outcome": "failed", "reason": "timeout"}
        self.rejects(capture([failed_last]), "incomplete|no complete attempt")
        mixed = case("state-http.get-json-v2", request(),
                     (response(text("{}")), response(text("{}"))), failed_attempts=1,
                     dump=envelope("{}"))
        mixed["attempts"][1]["source"][1]["attempt"] = 1
        self.rejects(capture([mixed]), "never mixed")
        retried = case("state-http.get-json-v2", request(),
                       (response(text("{}")), response(text("{}"))), failed_attempts=2,
                       dump=envelope("{}"))
        candidate = self.run_golden(capture([retried]))
        self.assertEqual(3, self.case_file(candidate, "state-http.get-json-v2")["attempts"])

    def test_rejects_unknown_fields_and_duplicate_keys(self) -> None:
        extra = capture([state_case()])
        extra["notes"] = "operator remark"
        self.rejects(extra, "unknown")
        path = self.root / "duplicate.json"
        path.write_text('{"format": "a", "format": "b"}')
        with self.assertRaisesRegex(GoldenError, "duplicate JSON key"):
            golden_mod._load_capture(str(path))

    def test_excludes_api_ingame_entirely(self) -> None:
        for target in ("/api/ingame", "/api/%69ngame/edit", "//API/InGame"):
            value = capture([case("state-http.ingame", request(target=target),
                                  (response(), response()))])
            self.rejects(value, "/api/ingame is excluded")

    def test_rejects_capability_and_credentials(self) -> None:
        capability = case("origin-peer.capability",
                          request("GET", "/json/v2", "X-Vosu-Test-Capture: <redacted>"),
                          (response(text("{}")), response(text("{}"))), dump=envelope("{}"))
        self.rejects(capture([capability]), "capability header")
        cookie = case("origin-peer.cookie", request("GET", "/json", "Cookie: sid=1"),
                      (response(), response()), schema="v1")
        self.rejects(capture([cookie]), "credential header cookie")
        echoed = state_case('{"note":"x-vosu-test-capture: 0123"}')
        self.rejects(capture([echoed]), "capability header")

    def test_dump_sandwich_and_route_coherence(self) -> None:
        moved = state_case()
        moved["attempts"][0]["vosu"]["dump_after"]["envelope"] = envelope(generation=4)
        self.rejects(capture([moved]), "differs in provider_generation")
        bytes_differ = state_case()
        bytes_differ["attempts"][0]["vosu"]["route"]["body"] = text('{"ok":false}')
        self.rejects(capture([bytes_differ]), "differ from the dump's adapter bytes")
        request_differs = state_case()
        request_differs["attempts"][0]["vosu"]["route"]["request"] = text(request(target="/json/v2?x"))
        self.rejects(capture([request_differs]), "request bytes differ")
        tampered = state_case()
        tampered["attempts"][0]["vosu"]["dump_before"]["envelope"]["bus_revision"] = 8
        tampered["attempts"][0]["vosu"]["dump_after"]["envelope"]["bus_revision"] = 8
        self.rejects(capture([tampered]), "digest")

    def test_sc_sandwich_exempts_only_generation_and_derived_revision(self) -> None:
        body = '{"sc":1}'

        def sc_case(after: dict) -> dict:
            before = envelope(body, schema="sc", generation=3)
            before["derived"] = {"revision": 1, "status": "ready"}
            before["digest"] = golden_mod.envelope_digest(before)
            up = response(text(body), [["Content-Type", "application/json"]])
            return case("state-http.json-sc.get.menu", request(target="/json/sc"), (up, up),
                        dump=before, dump_after=after, schema="sc")

        def after(generation: int = 4, revision: int = 2, status: str = "ready",
                  http_body: str = body, entries: list | None = None) -> dict:
            value = envelope(http_body, schema="sc", generation=generation, entries=entries)
            value["derived"] = {"revision": revision, "status": status}
            value["digest"] = golden_mod.envelope_digest(value)
            return value

        # The real sc provider advances generation and derived revision on every read.
        candidate = self.run_golden(capture([sc_case(after())]))
        self.assertEqual(self.case_file(candidate, "state-http.json-sc.get.menu")
                         ["replay"]["provider_generation"], 3)
        moved_entries = [{"ns": "vosu.osu", "pointer": "/x",
                          "value_cbor_b64": base64.b64encode(b"\x01").decode(), "error": None}]
        self.rejects(capture([sc_case(after(entries=moved_entries))]), "differs in entries")
        self.rejects(capture([sc_case(after(status="pending"))]), "differs in derived.status")
        self.rejects(capture([sc_case(after(http_body='{"sc":2}'))]), "differs in http")
        generation_only = state_case()
        generation_only["attempts"][0]["vosu"]["dump_after"]["envelope"] = envelope(generation=4)
        self.rejects(capture([generation_only]), "differs in provider_generation")
        revision_only = state_case()
        moved = envelope()
        moved["derived"]["revision"] = 1
        revision_only["attempts"][0]["vosu"]["dump_after"]["envelope"] = moved
        self.rejects(capture([revision_only]), "differs in derived")

    def test_closed_connection_is_an_explicit_golden_outcome(self) -> None:
        closed = {"outcome": "closed"}
        target = request("GET", "/json/v2", "Host: second")
        value = capture([case("origin-peer.host.duplicate", target, (closed, closed))])
        candidate = self.run_golden(value)
        record = self.case_file(candidate, "origin-peer.host.duplicate")
        self.assertEqual(record["expected"], {"outcome": "closed"})
        ws = case("ws.v2.invalid.missing-key", request(), ({**closed, "frames": []},) * 2)
        self.assertEqual(self.case_file(self.run_golden(capture([ws])), "ws.v2.invalid.missing-key")
                         ["expected"], {"outcome": "closed"})
        # One upstream response and one close are a sandwich mismatch, never a golden.
        mixed = case("origin-peer.host.duplicate", target, (closed, response(text("{}"))))
        self.rejects(capture([mixed]), "sandwich responses differ")
        framed = case("ws.v2.invalid.missing-key", request(),
                      ({**closed, "frames": [{"from": "server"}]},) * 2)
        self.rejects(capture([framed]), "closed without a response has no frames")
        self.rejects(capture([case("origin-peer.host.duplicate", target,
                                   ({"outcome": "reset-by-peer"},) * 2)]), "outcome")

    def test_negative_and_diagnostic_route_cases_are_never_goldens(self) -> None:
        for case_id in ("state-http.negative.api-v1-suffix", "ws.v2.diagnostic.feed"):
            value = capture([case(case_id, request(target="/api/v1x"),
                                  (response(text("{}")), response(text("{}"))))])
            self.rejects(value, "never goldens")

    def test_head_state_route_keeps_its_own_route_status(self) -> None:
        # The dump records the adapter's GET result; HEAD status comes from the route.
        not_ready = '{"error":"osu is not ready/running"}'
        dump = envelope(not_ready)
        dump["http"] = {"kind": "not_ready", "status": 500, "body_utf8": not_ready}
        dump["digest"] = golden_mod.envelope_digest(dump)
        up = response(text(""), [["Content-Type", "application/json"]],
                      status="HTTP/1.1 500 Internal Server Error")
        head = case("state-http.json-v2.head.no-client", request("HEAD"), (up, up),
                    vosu=response(text(""), [], status="HTTP/1.1 404 Not Found"), dump=dump)
        self.run_golden(capture([head]))
        get = case("state-http.json-v2.get.no-client", request(), (up, up),
                   vosu=response(text(not_ready), [], status="HTTP/1.1 404 Not Found"), dump=dump)
        self.rejects(capture([get]), "route status differs")

    def test_request_budget_ledger_decides_the_route_not_the_dump(self) -> None:
        # Pilot 2: an over-budget request never reaches the adapter, so vosu answers the
        # ledger's 404 while the dump still records the adapter's 200.
        pairs = lambda count: "/json/v2?" + "&".join(f"p{index}=0" for index in range(count))
        up = response(text('{"ok":true}'), [["Content-Type", "application/json"]])
        not_found = response(text("Not Found"), [], status="HTTP/1.1 404 Not Found")
        over = case("state-http.json-v2.budget.query-pairs-129", request(target=pairs(129)),
                    (up, up), vosu=not_found)
        self.run_golden(capture([over]))
        self.rejects(capture([case("state-http.json-v2.budget.query-pairs-129",
                                   request(target=pairs(129)), (up, up))]),
                     "ledger adapter-surface-request-budget .*must answer 404, not 200")
        # Within the budget the case stays strict, and no other case gets the rule.
        self.rejects(capture([case("state-http.json-v2.budget.query-pairs-128",
                                   request(target=pairs(128)), (up, up), vosu=not_found)]),
                     "route status differs")
        self.run_golden(capture([case("state-http.json-v2.budget.query-pairs-128",
                                      request(target=pairs(128)), (up, up))]))
        self.rejects(capture([case("state-http.json-v2.near.query", request(target=pairs(129)),
                                   (up, up), vosu=not_found)]), "route status differs")

    def test_request_budget_mirrors_the_surface_limits(self) -> None:
        excess = golden_ledger.request_budget_excess
        query = lambda total: request(target="/json/v2?q=" + "x" * (total - 1))
        tags = lambda count: [f'If-None-Match: "t{index}"' for index in range(count)]
        self.assertIsNone(excess(query(16_384)))
        self.assertRegex(excess(query(16_385)), "16385 metadata bytes")
        self.assertIsNone(excess(request("GET", "/json/v2", *tags(32))))
        self.assertRegex(excess(request("GET", "/json/v2", *tags(33))), "33 forwarded headers")
        self.assertIsNone(excess(request("GET", "/json/v2", "X-Other: \x01")))
        self.assertRegex(excess(request("GET", "/json/v2", "Range: bytes=0-1\x7f")),
                         "forwarded range value")
        # Decoded like form_urlencoded: empty sequences are skipped, a bare key is a pair.
        self.assertIsNone(excess(request(target="/json/v2?" + "&" * 300 + "a")))
        self.assertRegex(excess(request(target="/json/v2?" + "&".join("k" * 129))), "129 query")
        self.assertIsNone(golden_ledger.route_rule("state-http.json-v2.get.menu",
                                                   request(target="/json/v2?" + "&".join("k" * 129))))

    def test_identity_ordinals_are_verified(self) -> None:
        dump = envelope()
        dump["roster"]["sessions"][0]["ordinal"] = 1
        dump["session"]["ordinal"] = 1
        dump["digest"] = golden_mod.envelope_digest(dump)
        broken = state_case()
        broken["attempts"][0]["vosu"]["dump_before"]["envelope"] = dump
        broken["attempts"][0]["vosu"]["dump_after"]["envelope"] = copy.deepcopy(dump)
        self.rejects(capture([broken]), "first-appearance ordinals")

    def test_over_budget_is_rejected_not_compressed(self) -> None:
        with mock.patch.object(golden_mod, "JSON_BUDGET", 1024):
            self.rejects(capture([state_case("{" + '"pad":"' + "x" * 2048 + '"}')]), "over budget")


def files_case(case_id: str, etag: str | None, body: dict | None = None, *, extra: tuple = (),
               route_request: str | None = None, conditional: str | None = None,
               headers_b: list | None = None) -> dict:
    headers = [["Date", DATE_A], ["Content-Type", "application/octet-stream"]]
    if etag is not None:
        headers.append(["ETag", etag])
    up = response(body or text("x"), headers)
    up_b = response(body or text("x"), headers_b if headers_b is not None else headers)
    return case(case_id, request("GET", "/files/beatmap/file", *extra), (up, up_b),
                route_request=route_request, conditional=conditional)


class NormalizationTests(GoldenTestCase):
    def test_nested_json_escaped_root_is_replaced_and_length_recomputed(self) -> None:
        def body(root: str) -> str:
            return json.dumps({
                "path": root + "Songs\\1 a\\a.osu", "game": root[:-1],
                "nested": json.dumps({"p": root + "x"}), "forward": root.replace("\\", "/") + "y",
                "device": "\\\\?\\" + root + "z"}, separators=(",", ":"))

        candidate = self.run_golden(capture([state_case(body(WIN_ROOT))]))
        data = self.case_file(candidate, "state-http.get-json-v2")
        self.assertEqual({"text": body(SYN_ROOT)}, data["expected"]["body"])
        self.assertIn(["content-length", str(len(body(SYN_ROOT).encode()))], data["expected"]["headers"])
        self.assertEqual(body(SYN_ROOT), data["replay"]["http"]["body_utf8"])
        self.assertIn("root-prefix", data["normalized"])
        raw = (candidate / "windows-lazer/cases/state-http.get-json-v2.json").read_text()
        self.assertNotIn("users", raw.lower())

    def test_unattested_private_paths_fail_the_privacy_scan(self) -> None:
        other = json.dumps({"p": "C:\\Users\\someone\\secret.txt"})
        self.rejects(capture([state_case(other)]), "private path")
        lower_hex = "/api/calculate/pp?path=" + "C%3a%5cUsers%5cvosu%5cAppData%5cLocal%5cosulazer%5ca.osu"
        calc = case("calculation.absolute-inside-root", request(target=lower_hex),
                    (response(text("{}")), response(text("{}"))))
        self.rejects(capture([calc]), "unnormalized private root 'osu!'")

    def test_percent_encoded_root_in_request_is_replaced(self) -> None:
        from urllib.parse import quote, unquote
        target = "/api/calculate/pp?path=" + quote(WIN_ROOT + "Songs\\a.osu", safe="")
        calc = case("calculation.absolute-inside-root", request(target=target),
                    (response(text("{}")), response(text("{}"))))
        candidate = self.run_golden(capture([calc]))
        line = self.case_file(candidate, "calculation.absolute-inside-root")["request"]["text"]
        normalized = line.split(" ")[1]
        self.assertTrue(normalized.startswith("/api/calculate/pp?path=C%3A%5Cvosu-golden%5Cosu"))
        self.assertEqual("/api/calculate/pp?path=" + SYN_ROOT + "Songs\\a.osu", unquote(normalized))

    def test_absolute_root_right_after_a_route_prefix_is_replaced(self) -> None:
        # Rehearsal 2026-11-25: a path-security target spells the absolute sentinel path
        # right after its route prefix, so the prefix's `/` precedes the root. (The songs and
        # beatmap spellings are declared `private-path-echo` and write no case file.)
        from urllib.parse import quote
        sentinel = WIN_ROOT + "vosu-route-probe\\sentinel.txt"
        spellings = {"path-security.skin.absolute-drive": "/files/skin/" + quote(sentinel, safe=""),
                     "path-security.skin.absolute-drive-slash": "/files/skin/"
                     + quote(sentinel.replace("\\", "/"), safe="/:")}
        missing = response(text("{}"), status="HTTP/1.1 404 Not Found")
        cases = [case(case_id, request(target=target), (missing, missing))
                 for case_id, target in spellings.items()]
        candidate = self.run_golden(capture(cases))
        self.assertEqual(
            self.case_file(candidate, "path-security.skin.absolute-drive")["request"]["text"]
            .split(" ")[1], "/files/skin/C%3A%5Cvosu-golden%5Cosu!%5Cvosu-route-probe%5Csentinel.txt")
        self.assertEqual(
            self.case_file(candidate, "path-security.skin.absolute-drive-slash")["request"]
            ["text"].split(" ")[1], "/files/skin/C:/vosu-golden/osu!/vosu-route-probe/sentinel.txt")
        # Only the route prefix ends a token: a root inside a path segment stays private.
        inside = case("path-security.skin.absolute-drive",
                      request(target="/files/skin/a" + quote(sentinel, safe="")), (missing, missing))
        self.rejects(capture([inside]), "unnormalized private root 'osu!'")

    def test_upstream_static_fallback_path_is_replaced_only_inside_the_root(self) -> None:
        # Rehearsal 2026-11-25: tosu's static fallback answers a missing file with the
        # absolute path it tried; golden-capture gives it a static folder in the root.
        def enoent(static: str) -> dict:
            body = f"ENOENT: no such file or directory, stat '{static}vosu-route-probe\\sentinel.txt'"
            return response(text(body), status="HTTP/1.1 404 Not Found")

        def dot_segment(answer: dict) -> dict:
            return case("path-security.songs.dot-segment",
                        request(target="/Songs/../../vosu-route-probe/sentinel.txt"), (answer, answer))

        candidate = self.run_golden(capture([dot_segment(enoent(WIN_ROOT + "vosu-upstream-static\\"))]))
        self.assertEqual(
            {"text": "ENOENT: no such file or directory, stat "
                     "'C:\\vosu-golden\\osu!\\vosu-upstream-static\\vosu-route-probe\\sentinel.txt'"},
            self.case_file(candidate, "path-security.songs.dot-segment")["expected"]["body"])
        runtime = "V:\\bt\\run\\la\\vosu-live-parity\\vosu-live-parity-source-0pxap40n\\static\\"
        self.rejects(capture([dot_segment(enoent(runtime))]), r"private path \(drive path\)")

    def test_linux_root_and_private_home_paths(self) -> None:
        root = "/home/vosu/.local/share/osu/"
        linux = {"platform": "linux-lazer", "roots": [{"slot": "osu", "private": root}]}
        value = capture([state_case(json.dumps({"p": root + "files/a"}))], **linux)
        value["authorization"]["platform"] = "linux-lazer"
        value["attestation"]["vosu"]["target"] = "x86_64-unknown-linux-gnu"
        candidate = self.run_golden(value)
        self.assertEqual({"text": json.dumps({"p": "/vosu-golden/osu/files/a"})},
                         self.case_file(candidate, "state-http.get-json-v2", "linux-lazer")["expected"]["body"])
        leaked = copy.deepcopy(value)
        leaked["cases"] = [state_case(json.dumps({"p": "/home/vosu/Music/a"}))]
        self.rejects(leaked, "private path")
        shallow = copy.deepcopy(value)
        shallow["roots"] = [{"slot": "osu", "private": "/osu/"}]
        self.rejects(shallow, "at least two segments")

    def test_numeric_zero_and_other_numbers_are_never_normalized(self) -> None:
        body = '{"a":0,"b":"0","c":0.0,"d":-0}'
        candidate = self.run_golden(capture([files_case("files.beatmap-file", '"0"', text(body))]))
        expected = self.case_file(candidate, "files.beatmap-file")["expected"]
        self.assertEqual({"text": body}, expected["body"])
        self.assertIn(["etag", '"<golden:etag:1>"'], expected["headers"])
        scope = (("files", "body", "/a"),)
        with self.assertRaisesRegex(GoldenError, "targets a number"):
            apply_timestamp_pointers(body, "files", "body", scope)
        self.assertEqual(('{"t":"<golden:client-timestamp>","time":"1"}', True), apply_timestamp_pointers(
            '{"t":"12:00","time":"1"}', "files", "body", (("files", "body", "/t"),)))
        self.assertEqual(('{"time":null}', False), apply_timestamp_pointers(
            '{"time":null}', "files", "body", (("files", "body", "/t"),)))

    def test_unapproved_volatile_time_key_is_not_erased(self) -> None:
        volatile = case("state-http.get-json-v2", request(), (
            response(text('{"time":"10:00:00"}')), response(text('{"time":"10:00:07"}'))),
            dump=envelope('{"time":"10:00:00"}'))
        self.rejects(capture([volatile]), "sandwich responses differ")

    def test_validators_keep_distinct_values_distinct(self) -> None:
        cases = [
            files_case("files.a", '"aa"'), files_case("files.b", 'W/"bb"'),
            files_case("files.c", '"aa"'),
            files_case("files.d-if-none-match", '"bb"', extra=('If-None-Match: "bb"',),
                       route_request=request("GET", "/files/beatmap/file",
                                             'If-None-Match: "vosu-own"'),
                       conditional="etag"),
        ]
        candidate = self.run_golden(capture(cases))
        tags = {case_id: dict(self.case_file(candidate, case_id)["expected"]["headers"])["etag"]
                for case_id in ("files.a", "files.b", "files.c")}
        self.assertEqual({"files.a": '"<golden:etag:1>"', "files.b": 'W/"<golden:etag:2>"',
                          "files.c": '"<golden:etag:1>"'}, tags)
        self.assertIn('If-None-Match: "<golden:etag:2>"',
                      self.case_file(candidate, "files.d-if-none-match")["request"]["text"])
        with self.assertRaisesRegex(GoldenError, "same synthetic root"):
            RootRewriter([Root("a", "C:\\one\\", SYN_ROOT), Root("b", "C:\\two\\", SYN_ROOT)], True)
        with self.assertRaisesRegex(GoldenError, "nested"):
            RootRewriter([Root("a", "C:\\one\\", "C:\\vosu-golden\\a\\"),
                          Root("b", "C:\\one\\two\\", "C:\\vosu-golden\\b\\")], True)

    def test_etag_must_be_present_in_both_sandwich_responses(self) -> None:
        missing = files_case("files.beatmap-file", '"aa"',
                             headers_b=[["Date", DATE_A], ["Content-Type", "application/octet-stream"]])
        self.rejects(capture([missing]), "header-etag invariant")
        malformed = files_case("files.beatmap-file", "aa")
        self.rejects(capture([malformed]), "not a valid entity-tag")

    def test_text_is_stored_readable_never_base64(self) -> None:
        body = '{"server":"tosu"}'
        value = state_case()
        for record in value["attempts"][0]["source"] + [value["attempts"][0]["vosu"]["route"]]:
            record["body"] = {"b64": base64.b64encode(body.encode()).decode()}
            record["headers"][2][1] = str(len(body))
        for key in ("dump_before", "dump_after"):
            value["attempts"][0]["vosu"][key]["envelope"] = envelope(body)
        candidate = self.run_golden(capture([value]))
        rel = "windows-lazer/cases/state-http.get-json-v2.json"
        raw = (candidate / rel).read_text("utf-8")
        self.assertIn('\\"server\\":\\"tosu\\"', raw)
        count = len(re.findall("tosu", raw, flags=re.IGNORECASE))
        self.assertEqual(2, count)
        inventory = (candidate / "windows-lazer/brand-inventory.txt").read_text().splitlines()
        self.assertIn(f"tests/fixtures/upstream/{PIN}/{rel.split('/', 1)[0]}/cases/"
                      f"state-http.get-json-v2.json {count} parity", "\n".join(inventory))

    def with_cbor_entry(self, value_cbor: bytes) -> dict:
        entry = {"ns": "vosu.osu", "pointer": "/beatmap",
                 "value_cbor_b64": base64.b64encode(value_cbor).decode(), "error": None}
        value = state_case()
        for key in ("dump_before", "dump_after"):
            value["attempts"][0]["vosu"][key]["envelope"] = envelope(entries=[copy.deepcopy(entry)])
        return capture([value])

    def test_cbor_entries_are_scanned_at_any_depth_including_map_keys(self) -> None:
        def nested(key: str, item: str) -> bytes:
            # {"a": [{key: item}]}: the content exists only inside base64 CBOR.
            return (bytes([0xa1]) + cbor_text("a") + bytes([0x81, 0xa1])
                    + cbor_text(key) + cbor_text(item))

        hidden_brand = "tosu default"
        stranger = "C:\\Users\\someone\\skin"
        self.rejects(self.with_cbor_entry(nested(hidden_brand, "x")), "inside base64 CBOR")
        self.rejects(self.with_cbor_entry(nested("skin", hidden_brand)), "inside base64 CBOR")
        self.rejects(self.with_cbor_entry(nested(stranger, "x")), "private path .* dump CBOR string")
        self.rejects(self.with_cbor_entry(nested("skin", stranger)), "private path .* dump CBOR string")
        self.rejects(self.with_cbor_entry(nested(WIN_ROOT + "Skins", "x")), "CBOR map key")
        candidate = self.run_golden(self.with_cbor_entry(nested("skin", WIN_ROOT + "Skins\\a")))
        replay = self.case_file(candidate, "state-http.get-json-v2")["replay"]
        self.assertEqual(nested("skin", SYN_ROOT + "Skins\\a"),
                         base64.b64decode(replay["entries"][0]["value_cbor_b64"]))
        scan = json.loads((candidate / "windows-lazer/inventory.json").read_text())["privacy_scan"]
        self.assertEqual(("clean", 3, 0), (scan["result"], scan["cbor_text_strings"],
                                           scan["cbor_old_brand_strings"]))
        self.assertIn("dump CBOR text strings at any depth including map keys", scan["checks"])

    def test_cbor_text_root_is_rewritten_verbatim_elsewhere(self) -> None:
        def map_bytes(root: str) -> bytes:
            return (bytes([0xa3]) + cbor_text("folder") + cbor_text(root + "Songs\\a")
                    + cbor_text("n") + b"\x01" + cbor_text("f") + b"\xf9\x3c\x00")

        entry = {"ns": "vosu.osu", "pointer": "/beatmap",
                 "value_cbor_b64": base64.b64encode(map_bytes(WIN_ROOT)).decode(), "error": None}
        value = state_case()
        for key in ("dump_before", "dump_after"):
            value["attempts"][0]["vosu"][key]["envelope"] = envelope(entries=[copy.deepcopy(entry)])
        replay = self.case_file(self.run_golden(capture([value])), "state-http.get-json-v2")["replay"]
        self.assertEqual(map_bytes(SYN_ROOT), base64.b64decode(replay["entries"][0]["value_cbor_b64"]))
        self.assertEqual(golden_mod.envelope_digest(replay), replay["digest"])
        with self.assertRaisesRegex(GoldenError, "map key"):
            rewrite_cbor_text(bytes([0xa1]) + cbor_text(WIN_ROOT) + b"\x01",
                              lambda value: value.replace(WIN_ROOT, SYN_ROOT))
        with self.assertRaisesRegex(GoldenError, "indefinite"):
            rewrite_cbor_text(b"\x7f\x61a\xff", lambda value: value)

    def test_websocket_frames_keep_flags_and_recompute_changed_length(self) -> None:
        payload = json.dumps({"folder": WIN_ROOT + "Songs"})
        frames = [
            {"from": "client", "fin": True, "opcode": 1, "masked": True, "mask_key": "01020304",
             "length": 2, "payload": text("{}")},
            {"from": "server", "fin": True, "opcode": 1, "masked": False, "mask_key": None,
             "length": len(payload.encode()), "payload": text(payload)},
            {"from": "server", "fin": True, "opcode": 8, "masked": False, "mask_key": None,
             "length": 2, "payload": {"b64": base64.b64encode(b"\x03\xe8").decode()}},
        ]
        upgrade = response(headers=[["Upgrade", "websocket"], ["Connection", "Upgrade"],
                                    ["Sec-WebSocket-Accept", "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="]],
                           status="HTTP/1.1 101 Switching Protocols", frames=frames)
        req = request("GET", "/websocket/v2", "Upgrade: websocket", "Connection: Upgrade",
                      "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==", "Sec-WebSocket-Version: 13")
        candidate = self.run_golden(capture([case("ws.v2-first-frame", req, (upgrade, upgrade))]))
        out = self.case_file(candidate, "ws.v2-first-frame")["expected"]["frames"]
        synthetic = json.dumps({"folder": SYN_ROOT + "Songs"})
        self.assertEqual({"from": "client", "fin": True, "opcode": 1, "masked": True, "length": 2,
                          "payload": {"text": "{}"}}, out[0])
        self.assertEqual((len(synthetic.encode()), {"text": synthetic}), (out[1]["length"], out[1]["payload"]))
        self.assertEqual({"hex": "03e8"}, out[2]["payload"])
        binary = copy.deepcopy(upgrade)
        binary["frames"][1] = {"from": "server", "fin": True, "opcode": 2, "masked": False,
                               "mask_key": None, "length": 2, "payload": {"b64": "/wA="}}
        self.rejects(capture([case("ws.v2-first-frame", req, (binary, binary))]), "binary WebSocket")

    def test_head_content_length_follows_the_matching_get_body(self) -> None:
        body = json.dumps({"p": WIN_ROOT + "a"})
        head_up = response(headers=[["Content-Length", str(len(body.encode()))]])
        head = case("state-http.head-json-v2", request("HEAD"), (head_up, head_up),
                    dump=envelope(body))
        candidate = self.run_golden(capture([state_case(body), head]))
        expected = self.case_file(candidate, "state-http.head-json-v2")["expected"]
        self.assertEqual([["content-length", str(len(json.dumps({"p": SYN_ROOT + "a"}).encode()))]],
                         expected["headers"])

    def test_binary_bodies_must_be_approved_assets(self) -> None:
        png = b"\x89PNG\r\n\x1a\n\x00\xff\xfe" + bytes(range(256))
        ranged = {"b64": base64.b64encode(png[8:40]).decode()}
        self.rejects(capture([files_case("files.background-range", None, ranged)]), "binary body")
        asset = {"id": "background.png", "slot": "files", "relative": "a/ab/bg.png",
                 "media_type": "image/png", "generator": "vosu test generator",
                 "license": "CC0-1.0", "sha256": hashlib.sha256(png).hexdigest(),
                 "b64": base64.b64encode(png).decode()}
        candidate = self.run_golden(capture([files_case("files.background-range", None, ranged)],
                                            assets=[asset]))
        self.assertEqual({"asset": "background.png", "offset": 8, "length": 32},
                         self.case_file(candidate, "files.background-range")["expected"]["body"])
        self.assertEqual(png, (candidate / "windows-lazer/assets/background.png").read_bytes())
        listed = json.loads((candidate / "windows-lazer/assets.json").read_text())["assets"][0]
        self.assertEqual(("CC0-1.0", len(png)), (listed["license"], listed["bytes"]))
        wrong = dict(asset, sha256="0" * 64)
        self.rejects(capture([files_case("files.background-range", None, ranged)], assets=[wrong]),
                     "do not match sha256")


if __name__ == "__main__":
    unittest.main()
