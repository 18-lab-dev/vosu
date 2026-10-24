"""Exercise release packaging scripts with fixture bytes, not native builds."""

import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tarfile
import unittest
import unittest.mock
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
SIGN_SPEC = importlib.util.spec_from_file_location("sign_release", Path(__file__).with_name("sign-release.py"))
sign_release = importlib.util.module_from_spec(SIGN_SPEC)
SIGN_SPEC.loader.exec_module(sign_release)
REPOSITORY = Path(__file__).resolve().parents[1]


class LauncherPackageTests(unittest.TestCase):
    def test_vosux_is_a_relative_symlink(self):
        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / "package.tar"
            for target, valid in [("vosu", True), ("/usr/bin/vosu", False), ("other", False)]:
                with tarfile.open(archive, "w") as tar:
                    item = tarfile.TarInfo("usr/bin/vosux")
                    item.type = tarfile.SYMTYPE
                    item.linkname = target
                    item.mode = 0o777
                    tar.addfile(item)
                if valid:
                    self.assertEqual(linux.tar_entries(["cat", str(archive)]), {"usr/bin/vosux": 0o777})
                else:
                    with self.assertRaisesRegex(ValueError, "relative symlink"):
                        linux.tar_entries(["cat", str(archive)])
        config = (REPOSITORY / "packaging/linux/nfpm.yaml").read_text()
        self.assertIn("  - src: vosu\n    dst: /usr/bin/vosux\n    type: symlink", config)
        self.assertIn("usr/bin/vosux", linux.FILES)

    def test_packages_install_every_rendered_icon(self):
        config = (REPOSITORY / "packaging/linux/nfpm.yaml").read_text()
        for size in linux.ICON_SIZES:
            source = f"assets/branding/icons/vosu-{size}.png"
            destination = f"usr/share/icons/hicolor/{size}x{size}/apps/vosu.png"
            self.assertIn(f"  - src: {source}\n    dst: /{destination}\n", config)
            self.assertIn(destination, linux.FILES)
            self.assertTrue((REPOSITORY / source).is_file(), source)


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

    def test_release_notes_pin_relative_links_and_keep_only_the_tag_section(self):
        changelog = (
            "# Changelog\n\n## 1.2.3 — 2026-01-01\n\n"
            "- See [privacy](PRIVACY.md), [ADR](./docs/decisions/0001-x.md#why), "
            "[site](https://example.com/a), [anchor](#top), [root](/abs), "
            "[mail](mailto:a@b.c) and ![icon](assets/i.svg).\n\n"
            "## 1.2.2 — 2025-12-01\n\n- older [entry](OLD.md)\n"
        )
        base = "https://github.com/18-lab-dev/vosu/blob/v1.2.3"
        notes = release.release_notes("v1.2.3", changelog, signed=False)
        self.assertIn(f"[privacy]({base}/PRIVACY.md)", notes)
        self.assertIn(f"[ADR]({base}/docs/decisions/0001-x.md#why)", notes)
        self.assertIn(f"![icon]({base}/assets/i.svg)", notes)
        for unchanged in ("(https://example.com/a)", "(#top)", "(/abs)", "(mailto:a@b.c)"):
            self.assertIn(unchanged, notes)
        self.assertNotIn("older", notes)
        self.assertIn("unsigned", notes)
        self.assertNotIn("certificate by", notes)
        signed = release.release_notes("v1.2.3", changelog, signed=True)
        self.assertIn("certificate by [SignPath Foundation]", signed)
        with self.assertRaises(ValueError):
            release.release_notes("v9.9.9", changelog, signed=False)

    def test_release_set_requires_distribution_packages(self):
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
            names = linux.package_names("1.2.3")
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


@unittest.skipUnless(shutil.which("gpg"), "gpg is required")
class ReleaseSignatureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="sig-")
        self.root = Path(self.temporary.name)
        home = self.root / "gnupg"
        home.mkdir(mode=0o700)
        patcher = unittest.mock.patch.dict("os.environ", {"GNUPGHOME": str(home)})
        patcher.start()
        self.addCleanup(self.temporary.cleanup)
        self.addCleanup(patcher.stop)
        self.addCleanup(subprocess.run, ["gpgconf", "--kill", "all"], check=False)
        self.release_key = self.generate("Release <release@example.invalid>")
        self.other_key = self.generate("Other <other@example.invalid>")
        self.asset = self.root / "vosu-linux-v1.2.3.zip"
        self.asset.write_bytes(b"published archive")

    def generate(self, identity):
        subprocess.run(["gpg", "--batch", "--pinentry-mode", "loopback", "--passphrase", "",
                        "--quick-gen-key", identity, "ed25519", "sign", "never"],
                       check=True, capture_output=True)
        listing = subprocess.run(["gpg", "--with-colons", "--list-keys", identity],
                                 check=True, capture_output=True, text=True).stdout
        return next(line.split(":")[9] for line in listing.splitlines() if line.startswith("fpr:"))

    def test_fresh_signature_is_attributed_to_the_release_key(self):
        signature = sign_release.sign(self.asset, self.release_key)
        self.assertEqual(sign_release.signer(self.asset, signature), self.release_key)

    def test_replaced_asset_with_stale_signature_fails(self):
        signature = sign_release.sign(self.asset, self.release_key)
        self.asset.write_bytes(b"replaced archive")
        with self.assertRaises(SystemExit):
            sign_release.require_signed_by(self.asset, signature, self.release_key)

    def test_signature_by_another_key_fails(self):
        signature = sign_release.sign(self.asset, self.other_key)
        with self.assertRaises(SystemExit):
            sign_release.require_signed_by(self.asset, signature, self.release_key)


if __name__ == "__main__":
    unittest.main()
