"""Comprehensive offline unit tests for scripts/check-day0.py and sign-release.py day-0 gate."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
import unittest.mock


ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "scripts"


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


day0 = _load("check_day0_test_mod", SCRIPTS / "check-day0.py")
sign_release = _load("sign_release_test_mod", SCRIPTS / "sign-release.py")

CANDIDATE_COMMIT = "a" * 40
BASELINE_PRE_M0 = "3a91ff9" + "0" * 33
BASELINE_PREV = "b" * 40
CARGO_PIN = day0.read_cargo_source_pin(ROOT / "Cargo.toml") or ("c" * 40)

REPO_COMMITS = {
    "18-lab-dev/vosu-tools": "1" * 40,
    "18-lab-dev/vosulay": "2" * 40,
    "18-lab-dev/vosu-overlays": "3" * 40,
    "18-lab-dev/vosu-overlay-quickstart": "4" * 40,
}
REPO_TAGS = {
    "18-lab-dev/vosu-tools": "v1.0.0",
    "18-lab-dev/vosulay": "v1.6.0",
    "18-lab-dev/vosu-overlays": "v1.0.0",
    "18-lab-dev/vosu-overlay-quickstart": "v2.0.0",
}
REPO_IDS = {
    "18-lab-dev/vosu-tools": 101,
    "18-lab-dev/vosulay": 102,
    "18-lab-dev/vosu-overlays": 103,
    "18-lab-dev/vosu-overlay-quickstart": 104,
}


def _make_release_payload(repo: str, **overrides) -> dict:
    base = {
        "id": REPO_IDS[repo],
        "tag_name": REPO_TAGS[repo],
        "draft": False,
        "prerelease": False,
        "assets": (
            []
            if repo == day0.QUICKSTART_REPO
            else [{"name": "catalog-v2.json"}, {"name": "catalog.json"}]
        ),
    }
    base.update(overrides)
    return base


def _make_mock_fetcher(responses: dict[str, object], recorded_calls: list | None = None):
    def fetcher(url: str, *, headers: dict[str, str], timeout: float, max_bytes: int):
        if recorded_calls is not None:
            recorded_calls.append((url, dict(headers), timeout, max_bytes))
        for repo, outcome in responses.items():
            if url == f"https://api.github.com/repos/{repo}/releases/latest":
                if isinstance(outcome, BaseException):
                    raise outcome
                if isinstance(outcome, day0.HttpResponse):
                    return outcome
                return day0.HttpResponse(status=200, body=json.dumps(outcome).encode("utf-8"))
        return day0.HttpResponse(status=404, body=b'{"message":"Not Found"}')

    return fetcher


def _all_good_responses() -> dict[str, dict]:
    return {repo: _make_release_payload(repo) for repo in day0.ALL_DAY0_REPOS}


def _hex64(seed: str) -> str:
    return hashlib.sha256(seed.encode("utf-8")).hexdigest()


def _make_receipt(
    *,
    kind: str,
    repository: str,
    target: str,
    runtime: str,
    child: str | None,
    package_ids: list[str],
    host_tag: str = "v2.0.0-alpha.2",
) -> dict:
    return {
        "receipt_version": 1,
        "kind": kind,
        "repository": repository,
        "commit": REPO_COMMITS[repository],
        "tag": REPO_TAGS[repository],
        "host": {
            "tag": host_tag,
            "binary_sha256": _hex64(f"host-{target}"),
            "source": "github-release",
        },
        "sdk": {"version": "1.0.0", "crate_sha256": _hex64("sdk-crate")},
        "target": target,
        "runtime": runtime,
        "child": child,
        "packages": [
            {
                "id": pid,
                "kind": "plugin",
                "version": "1.0.0",
                "target": target,
                "file": f"{pid}-{target}.zip",
                "sha256": _hex64(f"{pid}-{target}"),
                "size": 1024,
            }
            for pid in package_ids
        ],
        "catalogs": [
            {"file": "catalog-v2.json", "sha256": _hex64(f"catv2-{repository}"), "size": 512},
            {"file": "catalog.json", "sha256": _hex64(f"catv1-{repository}"), "size": 512},
        ],
        "workflow": {
            "run_url": f"https://github.com/{repository}/actions/runs/12345",
            "run_id": 12345,
            "job": f"{kind}-{target}",
            "attempt": 1,
        },
        "tests": [
            {
                "suite": f"{kind}-suite",
                "command": "cargo test --locked",
                "selected": 10,
                "passed": 10,
                "failed": 0,
                "skipped": 0,
            }
        ],
        "native": {
            "sandbox": {"active": True},
            "cleanup": [{"pid": 4242, "executable": "vosu", "generation": 1, "outcome": "exited"}],
        },
        "approvals": ["A8.6: adopted 2026-11-24, Main"],
        "result": "pass",
        "started_at": "2026-11-27T01:00:00Z",
        "finished_at": "2026-11-27T01:05:00Z",
    }


def _valid_manifest(ledger_result: day0.LedgerCheckResult | None = None) -> dict:
    if ledger_result is None:
        ledger_result = day0.validate_ledger_file(day0.DEFAULT_LEDGER)
    linux_bin_sha = _hex64("vosu-linux-bin")
    win_bin_sha = _hex64("vosu-windows-bin")
    linux_zip_sha = _hex64("vosu-linux-v2.0.0.zip")
    win_zip_sha = _hex64("vosu-windows-v2.0.0.zip")
    receipts = []
    for target in day0.NATIVE_TARGETS:
        receipts.append(
            _make_receipt(
                kind="own-ci-process",
                repository="18-lab-dev/vosu-tools",
                target=target,
                runtime="process",
                child="production",
                package_ids=list(day0.REQUIRED_TOOL_PLUGINS),
            )
        )
        receipts.append(
            _make_receipt(
                kind="own-ci-process",
                repository="18-lab-dev/vosulay",
                target=target,
                runtime="process",
                child="fixture:non-cef-lifecycle",
                package_ids=[day0.SUPERVISOR_PLUGIN_ID],
            )
        )
        receipts.append(
            _make_receipt(
                kind="native-sandbox",
                repository="18-lab-dev/vosulay",
                target=target,
                runtime="in-process",
                child="production",
                package_ids=[day0.SUPERVISOR_PLUGIN_ID],
            )
        )

    return {
        "manifest_version": 1,
        "candidate": {
            "tag": "v2.0.0",
            "version": "2.0.0",
            "commit": CANDIDATE_COMMIT,
            "created_at": "2026-11-27T02:00:00Z",
            "sdk": {
                "version": "1.0.0",
                "crate_sha256": _hex64("sdk-crate"),
                "header_sha256": _hex64("vosu_plugin.h"),
            },
        },
        "artifacts": [
            {
                "file": "vosu-linux-v2.0.0.zip",
                "target": "x86_64-unknown-linux-gnu",
                "sha256": linux_zip_sha,
                "size": 10000,
                "signed": True,
            },
            {
                "file": "vosu",
                "target": "x86_64-unknown-linux-gnu",
                "sha256": linux_bin_sha,
                "size": 9000,
                "signed": True,
            },
            {
                "file": "vosu-windows-v2.0.0.zip",
                "target": "x86_64-pc-windows-msvc",
                "sha256": win_zip_sha,
                "size": 11000,
                "signed": True,
            },
            {
                "file": "vosu.exe",
                "target": "x86_64-pc-windows-msvc",
                "sha256": win_bin_sha,
                "size": 9500,
                "signed": True,
            },
            {
                "file": "vosu_plugin.h",
                "target": "any",
                "sha256": _hex64("vosu_plugin.h"),
                "size": 2048,
                "signed": True,
            },
        ],
        "day0_check": {
            "checked_at": "2026-11-27T02:00:00Z",
            "ledger_sha256": ledger_result.sha256,
            "ledger_counts": dict(ledger_result.counts),
            "repositories": [
                {
                    "repository": repo,
                    "tag": REPO_TAGS[repo],
                    "commit": REPO_COMMITS[repo],
                    "release_id": REPO_IDS[repo],
                    "catalog_v2_sha256": None if repo == day0.QUICKSTART_REPO else _hex64(f"catv2-{repo}"),
                    "catalog_v1_sha256": None if repo == day0.QUICKSTART_REPO else _hex64(f"catv1-{repo}"),
                    "checked_at": "2026-11-27T02:00:00Z",
                }
                for repo in day0.ALL_DAY0_REPOS
            ],
        },
        "ecosystem_receipts": receipts,
        "m4b_parity": {
            "source_pin": CARGO_PIN,
            "inventory_sha256": _hex64("m4b-inventory"),
            "pairs": [
                {
                    "platform": plat,
                    "authorization_ref": f"auth-{plat}",
                    "vosu_commit": CANDIDATE_COMMIT,
                    "client_version": "2026.1117.0",
                    "states": ["menu", "play", "paused", "result"],
                    "case_count": 42,
                    "capture_sha256": _hex64(f"capture-{plat}"),
                    "result": "pass",
                }
                for plat in ("windows-stable", "windows-lazer", "linux-lazer")
            ],
        },
        "d17_performance": {
            "decision": "pass",
            "baseline_pre_m0_commit": BASELINE_PRE_M0,
            "baseline_prev_commit": BASELINE_PREV,
            "candidate_commit": CANDIDATE_COMMIT,
            "linux_receipt_dir": "evidence/d17-linux",
            "windows_receipt_dir": "evidence/d17-windows",
            "broker_linux_ratios": {"precise": 1.15, "regular": 1.08, "cap": 2.0},
            "broker_windows_ratios": {"precise": 1.22, "regular": 1.11, "cap": 2.0},
            "cumulative_verdict": "pass",
            "build_budget_verdict": "pass",
        },
        "native_verification": {
            "linux_prepush_sha": CANDIDATE_COMMIT,
            "windows_native_sha": CANDIDATE_COMMIT,
            "packaged_smoke": [
                {
                    "platform": "linux",
                    "binary_sha256": linux_bin_sha,
                    "child_fixture_sha256": _hex64("child-linux"),
                    "api_ingame_404": True,
                    "consoleless_lifecycle_stop": True,
                    "no_orphans": True,
                    "result": "pass",
                },
                {
                    "platform": "windows",
                    "binary_sha256": win_bin_sha,
                    "child_fixture_sha256": _hex64("child-win"),
                    "api_ingame_404": True,
                    "consoleless_lifecycle_stop": True,
                    "no_orphans": True,
                    "result": "pass",
                },
            ],
            "updater_1_2_4": [
                {
                    "platform": "linux",
                    "source_binary_sha256": _hex64("1.2.4-linux"),
                    "candidate_sha256": linux_zip_sha,
                    "decline_no_download": True,
                    "accept_upgrade": True,
                    "rollback": True,
                    "result": "pass",
                },
                {
                    "platform": "windows",
                    "source_binary_sha256": _hex64("1.2.4-win"),
                    "candidate_sha256": win_zip_sha,
                    "decline_no_download": True,
                    "accept_upgrade": True,
                    "rollback": True,
                    "result": "pass",
                },
            ],
            "packages": [
                {
                    "manager": "deb",
                    "owner_repo": "18-lab-dev/vosu",
                    "package_id": "vosu",
                    "artifact_sha256": _hex64("vosu-deb"),
                    "install_upgrade_uninstall": True,
                    "result": "pass",
                }
            ],
            "owner_live_sessions": [
                {
                    "platform": plat,
                    "client_version": "2026.1117.0",
                    "states_traversed": ["menu", "play", "paused", "result"],
                    "installed_ids": ["map-analysis-v2", "vosu.tools.map-analysis", "ratings-timeline-v2", "vosulay-editor", "vosu.vosulay"],
                    "cef_sandbox_verified": True,
                    "cleanup_verified": True,
                    "result": "pass",
                }
                for plat in ("windows-stable", "windows-lazer", "linux-lazer")
            ],
            "d15_forge_receipts": [
                {"forge": forge, "checked_at": "2026-11-27T01:30:00Z", "result": "pass"}
                for forge in ("github", "gitlab", "codeberg", "gitea_or_forgejo")
            ],
        },
    }


class FourRepoGateTests(unittest.TestCase):
    def test_all_four_passing_and_strictly_unauthenticated(self):
        calls = []
        fetcher = _make_mock_fetcher(_all_good_responses(), calls)
        results = day0.check_all_repositories(fetcher=fetcher)
        self.assertEqual(len(results), 4)
        self.assertTrue(all(r.passed for r in results))
        self.assertEqual(len(calls), 4)
        for (url, headers, timeout, max_bytes), repo in zip(calls, day0.ALL_DAY0_REPOS):
            self.assertEqual(url, f"https://api.github.com/repos/{repo}/releases/latest")
            self.assertEqual(headers.get("Accept"), "application/vnd.github+json")
            self.assertFalse(any(k.lower() == "authorization" for k in headers))
            self.assertEqual(timeout, day0.DEFAULT_TIMEOUT_SECONDS)
            self.assertEqual(max_bytes, day0.MAX_RESPONSE_BYTES)

    def test_default_fetcher_rejects_http_non_github_and_auth_header(self):
        with self.assertRaisesRegex(ValueError, "non-HTTPS"):
            day0.default_fetcher("http://api.github.com/repos/18-lab-dev/vosu-tools/releases/latest", headers={})
        with self.assertRaisesRegex(ValueError, "non-GitHub-API"):
            day0.default_fetcher("https://example.invalid/repos/18-lab-dev/vosu-tools/releases/latest", headers={})
        with self.assertRaisesRegex(ValueError, "unauthenticated"):
            day0.default_fetcher(
                "https://api.github.com/repos/18-lab-dev/vosu-tools/releases/latest",
                headers={"Authorization": "Bearer secret"},
            )

    def test_one_of_four_failing_fails_whole_gate(self):
        for failing_repo in day0.ALL_DAY0_REPOS:
            with self.subTest(failing_repo=failing_repo):
                responses = _all_good_responses()
                responses[failing_repo] = day0.HttpResponse(status=404, body=b'{"message":"Not Found"}')
                result = day0.run_day0_gate(fetcher=_make_mock_fetcher(responses))
                self.assertFalse(result.passed)
                self.assertEqual(sum(1 for r in result.repos if r.passed), 3)
                self.assertTrue(any(failing_repo in err for err in result.errors))

    def test_catalog_repos_require_catalog_v2_and_non_draft_prerelease(self):
        for repo in day0.CATALOG_REPOS:
            for label, override in (
                ("missing_catalog_v2", {"assets": [{"name": "catalog.json"}]}),
                ("empty_assets", {"assets": []}),
                ("draft_true", {"draft": True}),
                ("prerelease_true", {"prerelease": True}),
                ("missing_draft", {"draft": None}),
                ("missing_tag", {"tag_name": ""}),
            ):
                with self.subTest(repo=repo, case=label):
                    responses = _all_good_responses()
                    responses[repo] = _make_release_payload(repo, **override)
                    res = day0.check_single_repository(repo, fetcher=_make_mock_fetcher(responses))
                    self.assertFalse(res.passed)

    def test_quickstart_semver_major2_rules_and_no_catalog_required(self):
        for good_tag in ("v2.0.0", "2.0.0", "v2.4.1", "2.10.0"):
            with self.subTest(good_tag=good_tag):
                responses = _all_good_responses()
                responses[day0.QUICKSTART_REPO] = _make_release_payload(
                    day0.QUICKSTART_REPO, tag_name=good_tag, assets=[]
                )
                res = day0.check_single_repository(day0.QUICKSTART_REPO, fetcher=_make_mock_fetcher(responses))
                self.assertTrue(res.passed, res.error)

        for bad_label, override in (
            ("v1.9.0", {"tag_name": "v1.9.0"}),
            ("v3.0.0", {"tag_name": "v3.0.0"}),
            ("v2.0.0-alpha.1", {"tag_name": "v2.0.0-alpha.1"}),
            ("2.0.0-rc.1", {"tag_name": "2.0.0-rc.1"}),
            ("v2.0.0+build.1", {"tag_name": "v2.0.0+build.1"}),
            ("v02.0.0", {"tag_name": "v02.0.0"}),
            ("not-semver", {"tag_name": "v2"}),
            ("draft", {"tag_name": "v2.0.0", "draft": True}),
            ("prerelease", {"tag_name": "v2.0.0", "prerelease": True}),
        ):
            with self.subTest(bad=bad_label):
                responses = _all_good_responses()
                responses[day0.QUICKSTART_REPO] = _make_release_payload(day0.QUICKSTART_REPO, **override)
                res = day0.check_single_repository(day0.QUICKSTART_REPO, fetcher=_make_mock_fetcher(responses))
                self.assertFalse(res.passed)

    def test_http_errors_timeouts_malformed_and_oversized_responses_fail_closed(self):
        repo = "18-lab-dev/vosu-tools"
        cases = (
            ("http_403", day0.HttpResponse(status=403, body=b'{"message":"rate limit"}')),
            ("http_404", day0.HttpResponse(status=404, body=b'{"message":"not found"}')),
            ("http_429", day0.HttpResponse(status=429, body=b'{"message":"too many requests"}')),
            ("http_500", day0.HttpResponse(status=500, body=b"internal error")),
            ("timeout", TimeoutError("timed out")),
            ("network_error", OSError("connection reset")),
            ("malformed_json", day0.HttpResponse(status=200, body=b"{not json")),
            ("non_object_json", day0.HttpResponse(status=200, body=b"[]")),
            ("oversized_body", day0.HttpResponse(status=200, body=b"{" + b" " * (day0.MAX_RESPONSE_BYTES + 10) + b"}")),
        )
        for label, outcome in cases:
            with self.subTest(case=label):
                res = day0.check_single_repository(repo, fetcher=_make_mock_fetcher({repo: outcome}))
                self.assertFalse(res.passed)


class DivergenceLedgerTests(unittest.TestCase):
    def test_committed_repository_ledger_passes(self):
        result = day0.validate_ledger_file(day0.DEFAULT_LEDGER)
        self.assertTrue(result.passed, result.errors)
        self.assertGreater(len(result.rows), 0)
        self.assertEqual(result.counts["temporary"], 0)
        self.assertGreater(result.counts["security"], 0)
        self.assertGreater(result.counts["contract"], 0)
        self.assertGreater(result.counts["licensing"], 0)

    def test_temporary_row_first_or_hidden_later_fails(self):
        hidden = (
            "# Divergences\n\n"
            "| id | class | scope | reason | closed by |\n"
            "| --- | --- | --- | --- | --- |\n"
            "| perm-1 | security | /a | reason 1 | permanent |\n"
            "| perm-2 | contract | /b | reason 2 | permanent |\n"
            "| temp-hidden | temporary | /c | still open | M8 |\n"
        )
        res = day0.validate_ledger_text(hidden)
        self.assertFalse(res.passed)
        self.assertEqual(res.counts["temporary"], 1)
        self.assertTrue(any("temp-hidden" in e and "temporary" in e for e in res.errors))

    def test_missing_unknown_ambiguous_class_duplicate_and_malformed_rows_fail(self):
        base_header = (
            "| id | class | scope | reason | closed by |\n"
            "| --- | --- | --- | --- | --- |\n"
        )
        bad_ledgers = {
            "missing_class": base_header + "| row-1 |  | /a | reason | permanent |\n",
            "unknown_class": base_header + "| row-1 | permanent | /a | reason | permanent |\n",
            "uppercase_class": base_header + "| row-1 | Security | /a | reason | permanent |\n",
            "ambiguous_class": base_header + "| row-1 | security/contract | /a | reason | permanent |\n",
            "duplicate_id": (
                base_header
                + "| dup-1 | security | /a | r1 | c1 |\n"
                + "| dup-1 | contract | /b | r2 | c2 |\n"
            ),
            "four_columns": (
                base_header
                + "| ok-1 | security | /a | r1 | c1 |\n"
                + "| bad-4 | security | /b | missing closed |\n"
            ),
            "six_columns": (
                base_header
                + "| ok-1 | security | /a | r1 | c1 |\n"
                + "| bad-6 | security | /b | r2 | c2 | extra |\n"
            ),
            "missing_trailing_pipe": (
                base_header
                + "| ok-1 | security | /a | r1 | c1 |\n"
                + "| broken | security | /b | r2 | c2\n"
            ),
            "missing_leading_pipe": (
                base_header
                + "| ok-1 | security | /a | r1 | c1 |\n"
                + "broken | security | /b | r2 | c2 |\n"
            ),
            "stray_pipe_row_after_blank": (
                base_header
                + "| ok-1 | security | /a | r1 | c1 |\n\n"
                + "| hidden-stray | temporary | /x | r | c |\n"
            ),
            "stray_pipe_row_before_table": (
                "| stray-before | security | /a | r | c |\n\n"
                + base_header
                + "| ok-1 | security | /a | r1 | c1 |\n"
            ),
            "empty_table": base_header + "\n",
            "missing_table": "# Just prose\nNo table here.\n",
            "duplicate_table": (
                base_header
                + "| ok-1 | security | /a | r1 | c1 |\n\n"
                + base_header
                + "| ok-2 | contract | /b | r2 | c2 |\n"
            ),
            "empty_reason": base_header + "| ok-1 | security | /a |   | c1 |\n",
        }
        for label, text in bad_ledgers.items():
            with self.subTest(case=label):
                res = day0.validate_ledger_text(text)
                self.assertFalse(res.passed, f"{label} should have failed")
                self.assertTrue(res.errors)


class EvidenceManifestTests(unittest.TestCase):
    def setUp(self):
        self.ledger = day0.validate_ledger_file(day0.DEFAULT_LEDGER)
        self.live_repos = day0.check_all_repositories(fetcher=_make_mock_fetcher(_all_good_responses()))

    def _validate(self, manifest, **kwargs):
        defaults = {
            "expected_tag": "v2.0.0",
            "expected_commit": CANDIDATE_COMMIT,
            "expected_source_pin": CARGO_PIN,
            "live_ledger": self.ledger,
            "live_repos": self.live_repos,
        }
        defaults.update(kwargs)
        return day0.validate_evidence_manifest(manifest, **defaults)

    def test_valid_format1_manifest_passes(self):
        errors = self._validate(_valid_manifest(self.ledger))
        self.assertEqual(errors, ())

    def test_wrong_manifest_version_and_candidate_binding_rejected(self):
        for label, mutator in (
            ("version_2", lambda m: m.__setitem__("manifest_version", 2)),
            ("version_bool", lambda m: m.__setitem__("manifest_version", True)),
            ("wrong_tag", lambda m: m["candidate"].__setitem__("tag", "v2.0.1")),
            ("stale_commit", lambda m: m["candidate"].__setitem__("commit", "f" * 40)),
            ("bad_artifact_sha", lambda m: m["artifacts"][0].__setitem__("sha256", "not-a-sha")),
            ("d17_fail", lambda m: m["d17_performance"].__setitem__("decision", "fail")),
            ("d17_cumulative_fail", lambda m: m["d17_performance"].__setitem__("cumulative_verdict", "fail")),
            ("d17_budget_fail", lambda m: m["d17_performance"].__setitem__("build_budget_verdict", "fail")),
            ("d17_broker_over_cap", lambda m: m["d17_performance"]["broker_linux_ratios"].__setitem__("precise", 2.5)),
            ("m4b_stale_pin", lambda m: m["m4b_parity"].__setitem__("source_pin", "d" * 40)),
            ("m4b_missing_pair", lambda m: m["m4b_parity"]["pairs"].pop()),
            ("m4b_missing_paused", lambda m: m["m4b_parity"]["pairs"][0].__setitem__("states", ["menu", "play", "result"])),
            ("day0_stale_repo_tag", lambda m: m["day0_check"]["repositories"][0].__setitem__("tag", "v0.9.9")),
            ("day0_temp_nonzero", lambda m: m["day0_check"]["ledger_counts"].__setitem__("temporary", 1)),
            ("smoke_hash_mismatch", lambda m: m["native_verification"]["packaged_smoke"][0].__setitem__("binary_sha256", "e" * 64)),
            ("smoke_ingame_false", lambda m: m["native_verification"]["packaged_smoke"][0].__setitem__("api_ingame_404", False)),
            ("smoke_orphans_false", lambda m: m["native_verification"]["packaged_smoke"][0].__setitem__("no_orphans", False)),
            ("live_session_missing_paused", lambda m: m["native_verification"]["owner_live_sessions"][0].__setitem__("states_traversed", ["menu", "play", "result"])),
            ("forge_missing", lambda m: m["native_verification"]["d15_forge_receipts"].pop()),
        ):
            with self.subTest(case=label):
                m = copy.deepcopy(_valid_manifest(self.ledger))
                mutator(m)
                errors = self._validate(m)
                self.assertTrue(errors, f"{label} should fail validation")

    def test_a86_supervisor_split_and_receipt_rejection_rules(self):
        # Removing either supervisor process-fixture or supervisor in-process native-sandbox on Linux or Windows fails.
        base = _valid_manifest(self.ledger)
        for target in day0.NATIVE_TARGETS:
            for kind in ("own-ci-process", "native-sandbox"):
                with self.subTest(missing_supervisor=(kind, target)):
                    m = copy.deepcopy(base)
                    m["ecosystem_receipts"] = [
                        r
                        for r in m["ecosystem_receipts"]
                        if not (
                            r["kind"] == kind
                            and r["target"] == target
                            and any(p["id"] == day0.SUPERVISOR_PLUGIN_ID for p in r["packages"])
                        )
                    ]
                    errors = self._validate(m)
                    self.assertTrue(any("A8.6" in e for e in errors), errors)

        # Fixture child offered as production CEF evidence fails.
        m = copy.deepcopy(base)
        for r in m["ecosystem_receipts"]:
            if r["kind"] == "native-sandbox":
                r["child"] = "fixture:fake-cef"
                break
        errors = self._validate(m)
        self.assertTrue(any("A8.6" in e for e in errors), errors)

        # Zero selected tests, skipped counted as passed, and runtime mismatch fail.
        for label, mutator in (
            ("selected_zero", lambda r: r["tests"][0].update({"selected": 0, "passed": 0})),
            ("skipped_counted_as_passed", lambda r: r["tests"][0].update({"selected": 10, "passed": 10, "skipped": 2})),
            ("empty_tests_asset_inference", lambda r: r.__setitem__("tests", [])),
            ("process_runtime_mismatch", lambda r: r.__setitem__("runtime", "in-process")),
            ("unrelated_repo", lambda r: r.__setitem__("repository", "18-lab-dev/unrelated-repo")),
            ("wrong_tools_owner_repo", lambda r: r.update({"repository": "18-lab-dev/vosulay", "commit": REPO_COMMITS["18-lab-dev/vosulay"]})),
            ("host_tag_v1_rejected", lambda r: r["host"].__setitem__("tag", "v1.2.4")),
            ("swapped_pkg_target", lambda r: r["packages"][0].__setitem__("target", "x86_64-pc-windows-msvc")),
        ):
            with self.subTest(case=label):
                m = copy.deepcopy(base)
                mutator(m["ecosystem_receipts"][0])
                errors = self._validate(m)
                self.assertTrue(errors, f"{label} should fail")

        # Wrong owner repo for vosu.vosulay fails; 18-lab-dev/vosu with candidate commit passes for native-sandbox.
        m_wrong_sup = copy.deepcopy(base)
        for r in m_wrong_sup["ecosystem_receipts"]:
            if r["kind"] == "native-sandbox":
                r["repository"] = "18-lab-dev/vosu-tools"
                r["commit"] = REPO_COMMITS["18-lab-dev/vosu-tools"]
                break
        self.assertTrue(self._validate(m_wrong_sup))

        m_vosu_sup = copy.deepcopy(base)
        for r in m_vosu_sup["ecosystem_receipts"]:
            if r["kind"] == "native-sandbox":
                r["repository"] = "18-lab-dev/vosu"
                r["commit"] = CANDIDATE_COMMIT
                break
        self.assertEqual(self._validate(m_vosu_sup), ())

    def test_unhashable_json_values_return_errors_never_typeerror(self):
        base = _valid_manifest(self.ledger)
        bad_val = ["unhashable"]
        mutations = (
            ("candidate.tag", lambda m: m["candidate"].__setitem__("tag", bad_val)),
            ("candidate.sdk.version", lambda m: m["candidate"]["sdk"].__setitem__("version", bad_val)),
            ("artifacts.target", lambda m: m["artifacts"][0].__setitem__("target", bad_val)),
            ("day0.repository", lambda m: m["day0_check"]["repositories"][0].__setitem__("repository", bad_val)),
            ("receipt.kind", lambda m: m["ecosystem_receipts"][0].__setitem__("kind", bad_val)),
            ("receipt.repository", lambda m: m["ecosystem_receipts"][0].__setitem__("repository", bad_val)),
            ("receipt.host.source", lambda m: m["ecosystem_receipts"][0]["host"].__setitem__("source", bad_val)),
            ("receipt.target", lambda m: m["ecosystem_receipts"][0].__setitem__("target", bad_val)),
            ("receipt.runtime", lambda m: m["ecosystem_receipts"][0].__setitem__("runtime", bad_val)),
            ("receipt.pkg.kind", lambda m: m["ecosystem_receipts"][0]["packages"][0].__setitem__("kind", bad_val)),
            ("receipt.pkg.target", lambda m: m["ecosystem_receipts"][0]["packages"][0].__setitem__("target", bad_val)),
            ("receipt.catalog.file", lambda m: m["ecosystem_receipts"][0]["catalogs"][0].__setitem__("file", bad_val)),
            ("m4b.platform", lambda m: m["m4b_parity"]["pairs"][0].__setitem__("platform", bad_val)),
            ("m4b.states_nested_list", lambda m: m["m4b_parity"]["pairs"][0].__setitem__("states", [["menu"], "play", "paused", "result"])),
            ("smoke.platform", lambda m: m["native_verification"]["packaged_smoke"][0].__setitem__("platform", bad_val)),
            ("updater.platform", lambda m: m["native_verification"]["updater_1_2_4"][0].__setitem__("platform", bad_val)),
            ("packages.manager", lambda m: m["native_verification"]["packages"][0].__setitem__("manager", bad_val)),
            ("sessions.platform", lambda m: m["native_verification"]["owner_live_sessions"][0].__setitem__("platform", bad_val)),
            ("sessions.states_nested", lambda m: m["native_verification"]["owner_live_sessions"][0].__setitem__("states_traversed", [{"a": 1}, "play", "paused", "result"])),
            ("forges.forge", lambda m: m["native_verification"]["d15_forge_receipts"][0].__setitem__("forge", bad_val)),
        )
        for label, mutator in mutations:
            with self.subTest(field=label):
                m = copy.deepcopy(base)
                mutator(m)
                errors = self._validate(m)
                self.assertTrue(errors, f"{label} with unhashable list must fail cleanly")
                self.assertTrue(all(isinstance(e, str) for e in errors))


class TagModeAndSignReleaseTests(unittest.TestCase):
    def test_prerelease_and_1x_tags_skip_day0_gate_without_network(self):
        called = []

        def forbidden_fetcher(*args, **kwargs):
            called.append(args)
            raise AssertionError("network fetcher must not be called for prerelease or 1.x tags")

        for tag in ("v2.0.0-alpha.2", "v2.0.0-rc.1", "v1.2.6"):
            with self.subTest(tag=tag):
                res = day0.run_day0_gate(
                    tag=tag,
                    require_evidence=True,
                    fetcher=forbidden_fetcher,
                )
                self.assertTrue(res.passed)
                self.assertTrue(res.skipped)
                self.assertEqual(called, [])

    def test_stable_v2_enforces_day0_and_require_evidence(self):
        fetcher = _make_mock_fetcher(_all_good_responses())
        missing_ev = day0.run_day0_gate(tag="v2.0.0", require_evidence=True, fetcher=fetcher)
        self.assertFalse(missing_ev.passed)
        self.assertTrue(any("--evidence" in e for e in missing_ev.errors))

        with tempfile.TemporaryDirectory() as tmp:
            ev_path = Path(tmp) / "evidence.json"
            ev_path.write_text(json.dumps(_valid_manifest()), encoding="utf-8")
            ok = day0.run_day0_gate(
                tag="v2.0.0",
                commit=CANDIDATE_COMMIT,
                evidence_path=ev_path,
                require_evidence=True,
                fetcher=fetcher,
            )
            self.assertTrue(ok.passed, ok.errors)

    def test_ledger_only_cannot_be_combined_with_release_tag(self):
        res = day0.run_day0_gate(tag="v2.0.0", ledger_only=True)
        self.assertFalse(res.passed)

    def test_sign_release_stable_v2_publish_requires_evidence_and_fresh_day0(self):
        calls = []

        def fake_run(*command, capture=False):
            calls.append(command)
            return "" if capture else None

        asset_bytes = b"vosu-linux-v2.0.0.zip"
        asset_sha = hashlib.sha256(asset_bytes).hexdigest()

        def fake_find_release(repo, tag):
            return {
                "id": 99,
                "draft": True,
                "target_commitish": CANDIDATE_COMMIT,
                "assets": [
                    {"name": "vosu-linux-v2.0.0.zip", "digest": f"sha256:{asset_sha}"},
                    {"name": "vosu-linux-v2.0.0.zip.sig", "digest": f"sha256:{_hex64('sig')}"},
                ],
            }

        def fake_download(tag, repo, name, directory):
            path = directory / name
            path.write_bytes(asset_bytes if not name.endswith(".sig") else b"sig")
            return path

        # 1. Missing --evidence fails BEFORE calling find_release or PATCH.
        with unittest.mock.patch.object(sign_release, "run", fake_run), \
             unittest.mock.patch.object(sign_release, "find_release", side_effect=AssertionError("must not call gh")), \
             self.assertRaisesRegex(SystemExit, "--evidence"):
            sign_release.main(["--tag", "v2.0.0", "--publish"])
        self.assertEqual(calls, [])

        with tempfile.TemporaryDirectory() as tmp:
            ev_path = Path(tmp) / "evidence.json"
            ev_path.write_text(json.dumps(_valid_manifest()), encoding="utf-8")

            # 2. Empty draft assets fail closed before PATCH (never skips asset binding).
            with unittest.mock.patch.object(sign_release, "run", fake_run), \
                 unittest.mock.patch.object(sign_release, "find_release", lambda r, t: {"id": 99, "draft": True, "target_commitish": CANDIDATE_COMMIT, "assets": []}), \
                 self.assertRaisesRegex(SystemExit, "no assets"):
                sign_release.main(
                    ["--tag", "v2.0.0", "--publish", "--evidence", str(ev_path)],
                    fetcher=_make_mock_fetcher(_all_good_responses()),
                )
            self.assertEqual(calls, [])

            # 3. Failing day-0 (1 of 4 repos 404) refuses before PATCH.
            bad_responses = _all_good_responses()
            bad_responses["18-lab-dev/vosulay"] = day0.HttpResponse(status=404, body=b"{}")
            with unittest.mock.patch.object(sign_release, "run", fake_run), \
                 unittest.mock.patch.object(sign_release, "find_release", fake_find_release), \
                 unittest.mock.patch.object(sign_release, "download", fake_download), \
                 unittest.mock.patch.object(sign_release, "require_signed_by"), \
                 self.assertRaisesRegex(SystemExit, "day-0 gate failed"):
                sign_release.main(
                    ["--tag", "v2.0.0", "--publish", "--evidence", str(ev_path)],
                    fetcher=_make_mock_fetcher(bad_responses),
                )
            self.assertEqual(calls, [])

            # 4. Invalid/stale evidence manifest refuses before PATCH.
            stale = _valid_manifest()
            stale["d17_performance"]["decision"] = "fail"
            stale_path = Path(tmp) / "stale.json"
            stale_path.write_text(json.dumps(stale), encoding="utf-8")
            with unittest.mock.patch.object(sign_release, "run", fake_run), \
                 unittest.mock.patch.object(sign_release, "find_release", fake_find_release), \
                 unittest.mock.patch.object(sign_release, "download", fake_download), \
                 unittest.mock.patch.object(sign_release, "require_signed_by"), \
                 self.assertRaisesRegex(SystemExit, "day-0 gate failed"):
                sign_release.main(
                    ["--tag", "v2.0.0", "--publish", "--evidence", str(stale_path)],
                    fetcher=_make_mock_fetcher(_all_good_responses()),
                )
            self.assertEqual(calls, [])

            # 5. Passing fresh day-0 + valid evidence + matching asset publishes via PATCH.
            buf = io.StringIO()
            with unittest.mock.patch.object(sign_release, "run", fake_run), \
                 unittest.mock.patch.object(sign_release, "find_release", fake_find_release), \
                 unittest.mock.patch.object(sign_release, "download", fake_download), \
                 unittest.mock.patch.object(sign_release, "require_signed_by"), \
                 unittest.mock.patch("sys.stdout", buf):
                sign_release.main(
                    ["--tag", "v2.0.0", "--publish", "--evidence", str(ev_path)],
                    fetcher=_make_mock_fetcher(_all_good_responses()),
                )
            self.assertEqual(len(calls), 1)
            self.assertEqual(calls[0][:3], ("gh", "api", "--method"))
            self.assertIn("draft=false", calls[0])
            self.assertNotIn("prerelease=true", calls[0])

    def test_sign_release_non_sha_target_commitish_resolves_annotated_tag_or_fails_closed(self):
        asset_bytes = b"vosu-linux-v2.0.0.zip"
        asset_sha = hashlib.sha256(asset_bytes).hexdigest()
        tag_obj_sha = "e" * 40

        def fake_find_release(repo, tag):
            return {
                "id": 101,
                "draft": True,
                "target_commitish": "main",
                "assets": [
                    {"name": "vosu-linux-v2.0.0.zip", "digest": f"sha256:{asset_sha}"},
                    {"name": "vosu-linux-v2.0.0.zip.sig", "digest": f"sha256:{_hex64('sig')}"},
                ],
            }

        def fake_download(tag, repo, name, directory):
            path = directory / name
            path.write_bytes(asset_bytes if not name.endswith(".sig") else b"sig")
            return path

        with tempfile.TemporaryDirectory() as tmp:
            ev_path = Path(tmp) / "evidence.json"
            ev_path.write_text(json.dumps(_valid_manifest()), encoding="utf-8")

            # Annotated tag peeling to CANDIDATE_COMMIT succeeds.
            calls = []

            def fake_run_peel(*command, capture=False):
                calls.append(command)
                if command == ("gh", "api", "repos/18-lab-dev/vosu/git/ref/tags/v2.0.0"):
                    return json.dumps({"object": {"type": "tag", "sha": tag_obj_sha}})
                if command == ("gh", "api", f"repos/18-lab-dev/vosu/git/tags/{tag_obj_sha}"):
                    return json.dumps({"object": {"type": "commit", "sha": CANDIDATE_COMMIT}})
                return "" if capture else None

            with unittest.mock.patch.object(sign_release, "run", fake_run_peel), \
                 unittest.mock.patch.object(sign_release, "find_release", fake_find_release), \
                 unittest.mock.patch.object(sign_release, "download", fake_download), \
                 unittest.mock.patch.object(sign_release, "require_signed_by"):
                sign_release.main(
                    ["--tag", "v2.0.0", "--publish", "--evidence", str(ev_path)],
                    fetcher=_make_mock_fetcher(_all_good_responses()),
                )
            self.assertTrue(any(c[:3] == ("gh", "api", "--method") for c in calls))

            # Tag resolving to a different commit fails closed before PATCH.
            bad_calls = []

            def fake_run_wrong_sha(*command, capture=False):
                bad_calls.append(command)
                if command == ("gh", "api", "repos/18-lab-dev/vosu/git/ref/tags/v2.0.0"):
                    return json.dumps({"object": {"type": "commit", "sha": "f" * 40}})
                return "" if capture else None

            with unittest.mock.patch.object(sign_release, "run", fake_run_wrong_sha), \
                 unittest.mock.patch.object(sign_release, "find_release", fake_find_release), \
                 unittest.mock.patch.object(sign_release, "download", fake_download), \
                 unittest.mock.patch.object(sign_release, "require_signed_by"), \
                 self.assertRaisesRegex(SystemExit, "candidate.commit"):
                sign_release.main(
                    ["--tag", "v2.0.0", "--publish", "--evidence", str(ev_path)],
                    fetcher=_make_mock_fetcher(_all_good_responses()),
                )
            self.assertFalse(any(c[:3] == ("gh", "api", "--method") for c in bad_calls))

            # Unresolvable remote tag fails closed before PATCH.
            with unittest.mock.patch.object(sign_release, "run", side_effect=OSError("gh unavailable")), \
                 unittest.mock.patch.object(sign_release, "find_release", fake_find_release), \
                 unittest.mock.patch.object(sign_release, "download", fake_download), \
                 unittest.mock.patch.object(sign_release, "require_signed_by"), \
                 self.assertRaisesRegex(SystemExit, "failed to resolve remote tag commit"):
                sign_release.main(
                    ["--tag", "v2.0.0", "--publish", "--evidence", str(ev_path)],
                    fetcher=_make_mock_fetcher(_all_good_responses()),
                )

    def test_sign_release_prerelease_publish_not_blocked_by_day0(self):
        calls = []

        def fake_run(*command, capture=False):
            calls.append(command)
            return "" if capture else None

        def fake_find_release(repo, tag):
            return {"id": 77, "draft": True, "assets": []}

        def forbidden_fetcher(*args, **kwargs):
            raise AssertionError("prerelease publish must not call day-0 network gate")

        with unittest.mock.patch.object(sign_release, "run", fake_run), \
             unittest.mock.patch.object(sign_release, "find_release", fake_find_release):
            sign_release.main(["--tag", "v2.0.0-alpha.2", "--publish"], fetcher=forbidden_fetcher)

        self.assertEqual(len(calls), 1)
        self.assertIn("prerelease=true", calls[0])
        self.assertIn("make_latest=false", calls[0])

    def test_sign_release_stable_v2_asset_digest_mismatch_blocks_publish(self):
        calls = []

        def fake_run(*command, capture=False):
            calls.append(command)
            return "" if capture else None

        asset_bytes = b"mismatched release archive bytes"
        asset_sha = hashlib.sha256(asset_bytes).hexdigest()

        def fake_find_release(repo, tag):
            return {
                "id": 88,
                "draft": True,
                "target_commitish": CANDIDATE_COMMIT,
                "assets": [
                    {"name": "vosu-linux-v2.0.0.zip", "digest": f"sha256:{asset_sha}"},
                    {"name": "vosu-linux-v2.0.0.zip.sig", "digest": f"sha256:{_hex64('sig')}"},
                ],
            }

        def fake_download(tag, repo, name, directory):
            path = directory / name
            path.write_bytes(asset_bytes if not name.endswith(".sig") else b"sig")
            return path

        with tempfile.TemporaryDirectory() as tmp:
            ev_path = Path(tmp) / "evidence.json"
            ev_path.write_text(json.dumps(_valid_manifest()), encoding="utf-8")
            with unittest.mock.patch.object(sign_release, "run", fake_run), \
                 unittest.mock.patch.object(sign_release, "find_release", fake_find_release), \
                 unittest.mock.patch.object(sign_release, "download", fake_download), \
                 unittest.mock.patch.object(sign_release, "require_signed_by"), \
                 self.assertRaisesRegex(SystemExit, "differs from evidence manifest"):
                sign_release.main(
                    ["--tag", "v2.0.0", "--publish", "--evidence", str(ev_path)],
                    fetcher=_make_mock_fetcher(_all_good_responses()),
                )
            self.assertFalse(any(c[:2] == ("gh", "api") for c in calls))


if __name__ == "__main__":
    unittest.main()
