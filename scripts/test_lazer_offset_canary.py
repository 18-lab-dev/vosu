"""Unit tests for the deterministic parts of lazer-offset-canary.py (no network, no dotnet)."""

import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("lazer_offset_canary", Path(__file__).resolve().parent / "lazer-offset-canary.py")
canary = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(canary)


class SelectReleaseTests(unittest.TestCase):
    def test_picks_newest_non_prerelease_lazer_tag(self):
        releases = [
            {"tag_name": "2026.1117.0-lazer", "prerelease": True},
            {"tag_name": "2026.1105.0-lazer", "prerelease": False},
            {"tag_name": "2026.1005.0", "prerelease": False},
        ]
        self.assertEqual(canary.select_release(releases)["tag_name"], "2026.1105.0-lazer")

    def test_explicit_tag_overrides_newest(self):
        releases = [
            {"tag_name": "2026.1117.0-lazer", "prerelease": False},
            {"tag_name": "2026.921.0-lazer", "prerelease": False},
        ]
        self.assertEqual(canary.select_release(releases, "2026.921.0-lazer")["tag_name"], "2026.921.0-lazer")

    def test_missing_explicit_tag_raises(self):
        with self.assertRaises(canary.CanaryError):
            canary.select_release([{"tag_name": "2026.921.0-lazer", "prerelease": False}], "2099.1.1-lazer")

    def test_no_matching_release_raises(self):
        with self.assertRaises(canary.CanaryError):
            canary.select_release([{"tag_name": "2026.921.0", "prerelease": False}])


class FindAssetTests(unittest.TestCase):
    def test_finds_named_asset(self):
        release = {"tag_name": "x", "assets": [{"name": "osu.AppImage", "browser_download_url": "u"}]}
        self.assertEqual(canary.find_asset(release)["browser_download_url"], "u")

    def test_missing_asset_raises(self):
        with self.assertRaises(canary.CanaryError):
            canary.find_asset({"tag_name": "x", "assets": []})


class ExpectedSha256Tests(unittest.TestCase):
    def test_parses_sha256_digest(self):
        value = "a" * 64
        self.assertEqual(canary.expected_sha256({"digest": f"sha256:{value}"}), value)

    def test_rejects_other_algorithms(self):
        self.assertIsNone(canary.expected_sha256({"digest": "sha512:" + "a" * 128}))

    def test_missing_digest_returns_none(self):
        self.assertIsNone(canary.expected_sha256({}))


class DiffSummaryTests(unittest.TestCase):
    def test_reports_added_removed_and_changed(self):
        old = {
            "OsuVersion": "2026.921.0",
            "GameBaseVtable": 111,
            "osu.Game.Skinning.SkinManager": {"<retroSkin>k__BackingField": 136, "<CurrentSkin>k__BackingField": 80},
            "osu.Game.Removed": {"x": 1},
        }
        new = {
            "OsuVersion": "2026.1117.0",
            "GameBaseVtable": 222,
            "osu.Game.Skinning.SkinManager": {"<retroSkin>k__BackingField": 48, "<CurrentSkin>k__BackingField": 88},
            "osu.Game.Added": {"y": 2},
        }
        summary = canary.diff_summary(old, new)
        self.assertEqual(summary["added_types"], ["osu.Game.Added"])
        self.assertEqual(summary["removed_types"], ["osu.Game.Removed"])
        self.assertEqual(
            summary["changed_fields"]["osu.Game.Skinning.SkinManager"],
            {"<retroSkin>k__BackingField": (136, 48), "<CurrentSkin>k__BackingField": (80, 88)},
        )
        self.assertEqual(summary["old_version"], "2026.921.0")
        self.assertEqual(summary["new_version"], "2026.1117.0")
        self.assertEqual(summary["old_game_base_vtable"], 111)
        self.assertEqual(summary["new_game_base_vtable"], 222)

    def test_identical_documents_report_no_changes(self):
        document = {"OsuVersion": "v", "GameBaseVtable": 1, "osu.Game.X": {"a": 1}}
        summary = canary.diff_summary(document, dict(document))
        self.assertEqual(summary["added_types"], [])
        self.assertEqual(summary["removed_types"], [])
        self.assertEqual(summary["changed_fields"], {})


if __name__ == "__main__":
    unittest.main()
