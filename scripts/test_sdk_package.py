"""Unit tests for scripts/sdk-package.py and the SDK package's static contract (AF.9).

These run without cargo. `python scripts/sdk-package.py verify` performs the
real package, standalone-test, isolated-consumer and private-surface checks.
"""

import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import tomllib
import unittest

SPEC = importlib.util.spec_from_file_location("sdk_package", Path(__file__).with_name("sdk-package.py"))
sdk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sdk)

ROOT = Path(__file__).resolve().parents[1]
PREFIX = "vosu-plugin-api-1.0.0-alpha.0"
GLOBS = sdk.include_globs((sdk.CRATE_DIR / "Cargo.toml").read_text(encoding="utf-8"))
NORMALIZED = """[package]
edition = "2024"
name = "vosu-plugin-api"
version = "1.0.0-alpha.0"
description = "SDK"
readme = "README.md"
license = "MIT OR Apache-2.0"
repository = "https://github.com/18-lab-dev/vosu"

[dependencies.serde]
version = "1.0.228"
features = ["derive"]
"""


def members(*relative, prefix=PREFIX):
    return [(f"{prefix}/{path}", True) for path in relative]


def complete():
    return members(*sorted(sdk.REQUIRED), ".cargo_vcs_info.json", "src/sdk/glue.rs", "tests/layout.rs")


class MemberListTests(unittest.TestCase):
    def test_a_complete_include_set_passes(self):
        files, errors = sdk.check_members(PREFIX, complete(), GLOBS)
        self.assertEqual(errors, [])
        self.assertIn("LICENSE-MIT", files)
        self.assertIn("examples/vosu-package-check/main.rs", files)

    def test_missing_licenses_or_readme_fail(self):
        for missing in ["LICENSE-MIT", "LICENSE-APACHE", "README.md", "Cargo.lock"]:
            listed = [m for m in complete() if not m[0].endswith("/" + missing)]
            _, errors = sdk.check_members(PREFIX, listed, GLOBS)
            self.assertIn(f"{missing}: missing from the package", errors)

    def test_files_outside_the_include_set_or_root_fail(self):
        for extra, message in [
            ("tests/layout_check.c", "not in the manifest's include set"),
            ("docs/v2/schemas/manifest.schema.json", "not in the manifest's include set"),
            ("../escape.rs", "outside the package root"),
        ]:
            listed = complete() + [(f"{PREFIX}/{extra}", True)]
            _, errors = sdk.check_members(PREFIX, listed, GLOBS)
            self.assertTrue(any(message in error for error in errors), (extra, errors))
        _, errors = sdk.check_members(PREFIX, complete() + [("other/src/lib.rs", True)], GLOBS)
        self.assertTrue(any("outside the package root" in error for error in errors))

    def test_links_are_rejected(self):
        listed = complete() + [(f"{PREFIX}/src/link.rs", False)]
        _, errors = sdk.check_members(PREFIX, listed, GLOBS)
        self.assertIn("src/link.rs: not a regular file", errors)


class ManifestTests(unittest.TestCase):
    def test_a_standalone_featureless_manifest_passes(self):
        self.assertEqual(sdk.check_normalized_manifest(NORMALIZED), [])

    def test_path_git_workspace_and_feature_leaks_fail(self):
        for leak, expected in [
            ('\n[dev-dependencies.vosu-proptest]\npath = "../vosu-proptest"\n', "path or git source"),
            ('\n[dependencies.x]\ngit = "https://example.org/x"\n', "path or git source"),
            ('\n[features]\nunstable-sync-reads = []\n', "declares Cargo features"),
            ('\n[target."cfg(windows)".dependencies.vosu-sys]\nversion = "1"\n', "workspace crate"),
            ('\n[lints]\nworkspace = true\n', "still names a workspace"),
        ]:
            errors = sdk.check_normalized_manifest(NORMALIZED + leak)
            self.assertTrue(any(expected in error for error in errors), (leak, errors))
        unpublished = NORMALIZED.replace('[package]\n', '[package]\npublish = false\n')
        self.assertIn("package.publish is false", sdk.check_normalized_manifest(unpublished))

    def test_the_real_sdk_manifest_is_featureless_and_path_free(self):
        with open(sdk.CRATE_DIR / "Cargo.toml", "rb") as handle:
            manifest = tomllib.load(handle)
        self.assertNotIn("features", manifest)
        self.assertNotIn("dev-dependencies", manifest)
        for name, spec in manifest["dependencies"].items():
            self.assertFalse(isinstance(spec, dict) and ("path" in spec or "git" in spec), name)
        self.assertNotIn("publish", manifest["package"])

    def test_the_package_license_copies_match_the_repository_licenses(self):
        for name in ["LICENSE-MIT", "LICENSE-APACHE"]:
            self.assertEqual((sdk.CRATE_DIR / name).read_bytes(), (ROOT / name).read_bytes(), name)


class SourceScanTests(unittest.TestCase):
    def test_internal_surface_is_flagged(self):
        for text in [
            "pub use plan_memory::PlanMemory;",
            "let b = SyncReadBuffers::new();",
            "vosu_plugin_internal::read_plan_profile::Span::new(p);",
            '#[cfg(feature = "unstable-sync-reads")]',
            "(bundled $ty:ty) => {}",
            "use vosu_proptest::vosu_proptest_config;",
        ]:
            self.assertTrue(sdk.check_sources({"src/lib.rs": text}), text)

    def test_prose_and_non_source_files_are_not_flagged(self):
        self.assertEqual(sdk.check_sources({"src/sdk.rs": "//! bundled registration lives elsewhere"}), [])
        self.assertEqual(sdk.check_sources({"LICENSE-MIT": "PlanMemory"}), [])

    def test_the_real_sdk_sources_are_clean(self):
        sources = {}
        for path in sdk.CRATE_DIR.rglob("*"):
            relative = path.relative_to(sdk.CRATE_DIR).as_posix()
            if path.is_file() and any(sdk.glob_matches(relative, g) for g in GLOBS):
                sources[relative] = path.read_text(encoding="utf-8")
        self.assertIn("src/lib.rs", sources)
        self.assertEqual(sdk.check_sources(sources), [])


class ExtractionTests(unittest.TestCase):
    def crate(self, directory, entries):
        archive = Path(directory) / "x.crate"
        with tarfile.open(archive, "w:gz") as tar:
            for name, kind in entries:
                info = tarfile.TarInfo(name)
                if kind == "file":
                    data = b"fn main() {}\n"
                    info.size = len(data)
                    tar.addfile(info, io.BytesIO(data))
                else:
                    info.type = tarfile.SYMTYPE
                    info.linkname = "/etc/passwd"
                    tar.addfile(info)
        return archive

    def test_regular_members_extract(self):
        with tempfile.TemporaryDirectory() as directory:
            archive = self.crate(directory, [(f"{PREFIX}/src/lib.rs", "file")])
            out = Path(directory) / "out"
            sdk.safe_extract(archive, out)
            self.assertTrue((out / PREFIX / "src/lib.rs").is_file())

    def test_traversal_and_links_are_refused(self):
        for entries in [[("../evil.rs", "file")], [("/abs.rs", "file")], [(f"{PREFIX}/link", "link")]]:
            with tempfile.TemporaryDirectory() as directory:
                archive = self.crate(directory, entries)
                with self.assertRaises(ValueError):
                    sdk.safe_extract(archive, Path(directory) / "out")

    def test_an_ancestor_manifest_is_detected(self):
        self.assertTrue(sdk.ancestors_have_manifest(ROOT / "crates"))
        with tempfile.TemporaryDirectory() as directory:
            self.assertFalse(sdk.ancestors_have_manifest(Path(directory)))


class ConsumerTests(unittest.TestCase):
    def test_the_consumer_uses_only_an_exact_registry_requirement(self):
        files = sdk.consumer_files("1.0.0-alpha.7")
        self.assertIn("[workspace]", files["Cargo.toml"])
        for member in ["serialized", "concurrent"]:
            manifest = tomllib.loads(files[f"{member}/Cargo.toml"])
            self.assertEqual(manifest["dependencies"], {"vosu-plugin-api": "=1.0.0-alpha.7"})
            self.assertEqual(manifest["lib"]["crate-type"], ["cdylib", "rlib"])
        self.assertIn("export_plugin!(Serialized);", files["serialized/src/lib.rs"])
        self.assertIn("impl Plugin for Serialized", files["serialized/src/lib.rs"])
        self.assertIn("export_plugin!(concurrent Concurrent);", files["concurrent/src/lib.rs"])
        self.assertIn("impl ConcurrentPlugin for Concurrent", files["concurrent/src/lib.rs"])

    def test_private_surface_probes_cover_features_bundled_and_plan_memory(self):
        probes = sdk.private_surface_probes("1.0.0-alpha.7")
        self.assertEqual(
            set(probes),
            {"feature unstable-sync-reads", "feature bundled", "export_plugin!(bundled ...)", "PlanMemory"},
        )
        for files, expected in probes.values():
            self.assertIn("[workspace]", files["Cargo.toml"])
            self.assertNotIn("path", files["Cargo.toml"])
            self.assertTrue(expected)

    def metadata(self, sdk_manifest, features=None, extra=()):
        packages = [{"id": "sdk", "name": sdk.CRATE, "manifest_path": sdk_manifest, "features": features or {}}]
        packages += [
            {"id": name, "name": name, "manifest_path": path, "features": {}} for name, path in extra
        ]
        return {"packages": packages, "resolve": {"nodes": [{"id": "sdk", "features": []}]}}

    def test_consumer_metadata_accepts_the_extracted_featureless_sdk(self):
        with tempfile.TemporaryDirectory() as directory:
            extracted = Path(directory) / PREFIX
            metadata = self.metadata(str(extracted / "Cargo.toml"), extra=[("serde", "/registry/serde/Cargo.toml")])
            self.assertEqual(sdk.check_consumer_metadata(metadata, extracted), [])

    def test_consumer_metadata_rejects_checkout_paths_features_and_workspace_crates(self):
        with tempfile.TemporaryDirectory() as directory:
            extracted = Path(directory) / PREFIX
            checkout = str(sdk.CRATE_DIR / "Cargo.toml")
            self.assertTrue(sdk.check_consumer_metadata(self.metadata(checkout), extracted))
            featured = self.metadata(str(extracted / "Cargo.toml"), features={"bundled": []})
            self.assertTrue(sdk.check_consumer_metadata(featured, extracted))
            internal = self.metadata(
                str(extracted / "Cargo.toml"), extra=[("vosu-plugin-internal", "/elsewhere/Cargo.toml")]
            )
            self.assertTrue(sdk.check_consumer_metadata(internal, extracted))

    def test_the_pinned_toolchain_is_read_from_the_repository(self):
        self.assertRegex(sdk.pinned_toolchain(), r"^\d+\.\d+\.\d+$")


if __name__ == "__main__":
    unittest.main()
