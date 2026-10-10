"""Exercise release packaging scripts with fixture bytes, not native builds."""

import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import warnings
import zipfile


SPEC = importlib.util.spec_from_file_location(
    "package_release", Path(__file__).with_name("package-release.py")
)
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)

LINUX_SPEC = importlib.util.spec_from_file_location(
    "package_linux", Path(__file__).with_name("package-linux.py")
)
linux = importlib.util.module_from_spec(LINUX_SPEC)
LINUX_SPEC.loader.exec_module(linux)

VERSION_BUILD = Path(__file__).with_name("version-build.py")
VERSION_SPEC = importlib.util.spec_from_file_location("version_build", VERSION_BUILD)
version_build = importlib.util.module_from_spec(VERSION_SPEC)
VERSION_SPEC.loader.exec_module(version_build)
REPOSITORY = Path(__file__).resolve().parents[1]


class ReleaseLicenseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.license = self.root / "current-license"
        self.license.write_bytes(b"current license\n")
        self.archive = self.root / "release.zip"

    def write_archive(self, entries):
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", UserWarning)
            with zipfile.ZipFile(self.archive, "w") as archive:
                for name, content in entries:
                    archive.writestr(name, content)

    def test_stale_license_is_rejected(self):
        self.write_archive([("vosu", b"fixture"), ("LICENSE", b"old license\n")])
        with self.assertRaisesRegex(ValueError, "LICENSE does not match"):
            release.verify_archive(self.archive, "linux", self.license)

    def test_missing_license_is_rejected(self):
        self.write_archive([("vosu", b"fixture")])
        with self.assertRaisesRegex(ValueError, "exactly"):
            release.verify_archive(self.archive, "linux", self.license)

    def test_extra_duplicate_and_nested_entries_are_rejected(self):
        for entries in (
            [("vosu", b"fixture"), ("LICENSE", b"one"), ("README.md", b"extra")],
            [("vosu", b"fixture"), ("LICENSE", b"one"), ("LICENSE", b"two")],
            [("folder/vosu", b"fixture"), ("LICENSE", b"one")],
        ):
            with self.subTest(entries=[name for name, _ in entries]):
                self.write_archive(entries)
                with self.assertRaisesRegex(ValueError, "exactly"):
                    release.verify_archive(self.archive, "linux", self.license)

    def test_corrupt_executable_crc_is_rejected(self):
        self.write_archive([("vosu", b"fixture"), ("LICENSE", self.license.read_bytes())])
        data = bytearray(self.archive.read_bytes())
        with zipfile.ZipFile(self.archive) as archive:
            info = archive.getinfo("vosu")
            offset = info.header_offset
        filename_length = int.from_bytes(data[offset + 26:offset + 28], "little")
        extra_length = int.from_bytes(data[offset + 28:offset + 30], "little")
        data[offset + 30 + filename_length + extra_length] ^= 1
        self.archive.write_bytes(data)
        with self.assertRaisesRegex(ValueError, "CRC"):
            release.verify_archive(self.archive, "linux", self.license)


class DistributionReleaseTests(unittest.TestCase):
    def test_package_asset_names(self):
        self.assertEqual(linux.package_names("1.2.3"), [
            "vosu_1.2.3_amd64.deb", "vosu-1.2.3-1.x86_64.rpm",
            "vosu-1.2.3-1-x86_64.pkg.tar.zst",
        ])

    def test_tag_must_match_manifest_and_be_stable(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text('[package]\nversion = "1.2.3"\n')
            self.assertEqual(release.version_from_tag("v1.2.3", manifest), "1.2.3")
            for tag in ("v1.2.4", "v1.2.3-rc1", "1.2.3", "v01.2.3"):
                with self.subTest(tag=tag), self.assertRaises(ValueError):
                    release.version_from_tag(tag, manifest)

    def test_release_set_requires_packages_and_aur(self):
        import argparse
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "Cargo.toml"
            manifest.write_text('[package]\nversion = "1.2.3"\n')
            dist = root / "dist"
            dist.mkdir()
            license_path = root / "LICENSE"
            license_path.write_bytes(b"explicit test fixture license")
            args = argparse.Namespace(tag="v1.2.3", manifest=manifest,
                                      directory=dist, license=license_path)
            for platform, executable in release.PLATFORM_EXECUTABLE.items():
                with zipfile.ZipFile(dist / f"vosu-{platform}-v1.2.3.zip", "w") as archive:
                    archive.writestr(executable, b"explicit non-daemon fixture")
                    archive.writestr("LICENSE", license_path.read_bytes())
            names = [*linux.package_names("1.2.3"), "vosu-bin-aur-v1.2.3.tar.gz"]
            # Only the release-set filename consumer is under test here; native
            # package contents are exercised with real binaries, not fixtures.
            for name in names:
                (dist / name).write_bytes(b"explicit asset-name fixture")
            release.verify_release_set(args)
            for name in names:
                (dist / name).unlink()
                with self.subTest(missing=name), self.assertRaises(ValueError):
                    release.verify_release_set(args)
                (dist / name).write_bytes(b"explicit asset-name fixture")
            (dist / "unexpected").touch()
            with self.assertRaises(ValueError):
                release.verify_release_set(args)

    def test_aur_version_and_archive_checksum(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "vosu-linux-v1.2.3.zip"
            archive.write_bytes(b"release archive")
            linux.render_aur("1.2.3", archive, root / "aur")
            pkgbuild = (root / "aur/PKGBUILD").read_text()
            srcinfo = (root / "aur/.SRCINFO").read_text()
            self.assertIn("pkgver=1.2.3\n", pkgbuild)
            self.assertIn(linux.sha256(archive), pkgbuild)
            self.assertIn("\tpkgver = 1.2.3\n", srcinfo)
            self.assertIn("\tsha256sums = " + linux.sha256(archive), srcinfo)
            self.assertNotIn("@VERSION@", pkgbuild)


class DevelopmentArchiveTests(unittest.TestCase):
    def test_archive_carries_current_attribution_and_build_metadata(self):
        sha = "9c2187e683775485b8551fd6e7cb09b6e6786332"
        for platform, (target, executable, features) in version_build.TARGETS.items():
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as directory:
                checkout = Path(directory) / "checkout"
                for name in (*version_build.ATTRIBUTION, "Cargo.toml", "rust-toolchain.toml"):
                    destination = checkout / name
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(REPOSITORY / name, destination)
                binary = checkout / "target" / target / "release" / executable
                binary.parent.mkdir(parents=True)
                binary.write_bytes(b"explicit non-daemon fixture")
                output = Path(directory) / "dist"
                subprocess.run(
                    [sys.executable, VERSION_BUILD, "package", "--platform", platform,
                     "--sha", sha, "--output", output,
                     "--destination", Path(directory) / "extract"],
                    cwd=checkout, check=True, capture_output=True,
                )
                [archive] = output.glob("*.zip")
                with zipfile.ZipFile(archive) as file:
                    self.assertEqual(
                        set(file.namelist()),
                        {executable, "BUILD-INFO.json", *version_build.ATTRIBUTION},
                    )
                    for name in version_build.ATTRIBUTION:
                        self.assertEqual(file.read(name), (REPOSITORY / name).read_bytes(), name)
                    info = json.loads(file.read("BUILD-INFO.json"))
                self.assertEqual((info["target"], info["features"], info["commit"]),
                                 (target, features, sha))


if __name__ == "__main__":
    unittest.main()
