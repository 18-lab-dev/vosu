"""Exercise release packaging scripts with fixture bytes, not native builds."""

import importlib.util
import json
import os
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
            [("vosu", b"fixture"), ("LICENSE", b"one"), ("vosu_plugin.h", b"header")],
            [("vosu", b"fixture"), ("LICENSE", b"one"), ("LICENSE", b"two")],
            [("folder/vosu", b"fixture"), ("LICENSE", b"one")],
        ):
            with self.subTest(entries=[name for name, _ in entries]):
                self.write_archive(entries)
                with self.assertRaisesRegex(ValueError, "exactly"):
                    release.verify_archive(self.archive, "linux", self.license)

    def test_plugin_header_inside_windows_daemon_zip_is_rejected(self):
        self.write_archive([
            ("vosu.exe", b"fixture"),
            ("LICENSE", self.license.read_bytes()),
            ("vosu_plugin.h", b"/* must stay standalone */"),
        ])
        with self.assertRaisesRegex(ValueError, "exactly"):
            release.verify_archive(self.archive, "windows", self.license)

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
        self.assertEqual([package.asset for package in release.distribution_packages("1.2.3")], [
            "vosu_1.2.3_amd64.deb", "vosu-1.2.3-1.x86_64.rpm",
            "vosu-1.2.3-1-x86_64.pkg.tar.zst",
        ])

    def test_stable_packages_keep_their_plain_native_version(self):
        self.assertEqual(
            [(package.packager, package.version) for package in release.distribution_packages("1.2.3")],
            [("deb", "1.2.3"), ("rpm", "1.2.3"), ("archlinux", "1.2.3")],
        )

    def test_prerelease_packages_render_each_native_version(self):
        packages = release.distribution_packages("2.0.0-alpha.1")
        self.assertEqual([(package.packager, package.asset, package.version) for package in packages], [
            ("deb", "vosu_2.0.0-alpha.1_amd64.deb", "2.0.0~alpha.1"),
            ("rpm", "vosu-2.0.0-alpha.1-1.x86_64.rpm", "2.0.0~alpha.1"),
            ("archlinux", "vosu-2.0.0-alpha.1-1-x86_64.pkg.tar.zst", "2.0.0alpha.1"),
        ])
        for package in packages:
            with self.subTest(package=package.packager):
                # GitHub renames `~` (and other specials) in uploaded asset names.
                self.assertRegex(package.asset, r"\A[0-9A-Za-z._-]+\Z")
        # A hyphen is the Debian revision separator and invalid in RPM Version / Arch pkgver.
        self.assertNotIn("-", packages[1].version)
        self.assertNotIn("-", packages[2].version)

    def test_prereleases_distributions_cannot_order_are_rejected(self):
        for version in ("2.0.0-1", "2.0.0-alpha-1", "2.0.0-alpha.beta", "2.0.0-0alpha", "2.0.0-alpha1.x"):
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, "dpkg, rpm and pacman"):
                release.distribution_packages(version)
        for version in ("2.0.0-alpha.01", "2.0.0-", "2.0.0+build", "v2.0.0"):
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, "SemVer"):
                release.distribution_packages(version)

    def test_rendered_versions_sort_like_semver_in_available_distribution_tools(self):
        # Ascending SemVer order, including both prerelease/release directions.
        ordered = ["1.2.5", "2.0.0-alpha", "2.0.0-alpha.1", "2.0.0-alpha.2", "2.0.0-alpha.10",
                   "2.0.0-beta", "2.0.0-rc.1", "2.0.0", "2.0.1-alpha.1", "2.0.1"]
        comparators = {}
        if shutil.which("vercmp"):
            comparators["archlinux"] = lambda a, b: int(subprocess.run(
                ["vercmp", a, b], check=True, capture_output=True, text=True).stdout)
        if shutil.which("dpkg"):
            comparators["deb"] = lambda a, b: 0 if a == b else (
                -1 if subprocess.run(["dpkg", "--compare-versions", a, "lt", b]).returncode == 0 else 1)
        if shutil.which("rpmdev-vercmp"):
            comparators["rpm"] = lambda a, b: {0: 0, 11: 1, 12: -1}[subprocess.run(
                ["rpmdev-vercmp", a, b], capture_output=True).returncode]
        if not comparators:
            self.skipTest("no vercmp, dpkg or rpmdev-vercmp on PATH")
        for packager, compare in comparators.items():
            native = [next(package.version for package in release.distribution_packages(version)
                           if package.packager == packager) for version in ordered]
            for lower, higher in zip(native, native[1:]):
                with self.subTest(packager=packager, lower=lower, higher=higher):
                    self.assertEqual(compare(lower, higher), -1)
                    self.assertEqual(compare(higher, lower), 1)

    def test_tag_must_match_manifest_and_be_stable(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text('[package]\nversion = "1.2.3"\n')
            self.assertEqual(release.version_from_tag("v1.2.3", manifest), "1.2.3")
            for tag in ("v1.2.4", "v1.2.3-rc1", "1.2.3", "v01.2.3"):
                with self.subTest(tag=tag), self.assertRaises(ValueError):
                    release.version_from_tag(tag, manifest)

    def test_prerelease_tag_must_be_strict_semver_and_equal_the_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text('[package]\nversion = "2.0.0-alpha.1"\n')
            self.assertEqual(release.version_from_tag("v2.0.0-alpha.1", manifest), "2.0.0-alpha.1")
            for tag in ("v2.0.0", "v2.0.0-alpha.2", "v2.0.0-alpha", "v2.0.0-alpha.1.0",
                        "v2.0.0-ALPHA.1", "2.0.0-alpha.1", "V2.0.0-alpha.1"):
                with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, "does not match|release tag"):
                    release.version_from_tag(tag, manifest)
            for tag in ("v2.0.0-alpha.01", "v02.0.0-alpha.1", "v2.00.0-alpha.1", "v2.0.0-alpha..1",
                        "v2.0.0-", "v2.0.0-alpha.1+build.5", "v2.0.0-alpha_1", "v2.0.0-alpha.1\n",
                        "v\N{FULLWIDTH DIGIT TWO}.0.0-alpha.1", "v2.0.0-alpha.\N{ARABIC-INDIC DIGIT ONE}",
                        "v2.0-alpha.1", "v2.0.0.0-alpha.1", "vv2.0.0-alpha.1"):
                with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, "SemVer"):
                    release.version_from_tag(tag, manifest)

    def test_repository_version_is_a_packageable_release_version(self):
        import tomllib
        version = tomllib.loads((REPOSITORY / "Cargo.toml").read_text())["package"]["version"]
        self.assertEqual(release.version_from_tag(f"v{version}", REPOSITORY / "Cargo.toml"), version)
        self.assertEqual(len(release.distribution_packages(version)), 3)

    def test_validate_tag_command_rejects_a_version_packages_cannot_carry(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            for version, code in (("2.0.0-alpha.1", 0), ("2.0.0-1", 1), ("1.2.3", 0)):
                manifest.write_text(f'[package]\nversion = "{version}"\n')
                result = subprocess.run(
                    [sys.executable, SPEC.origin, "--manifest", manifest, "validate-tag", "--tag", f"v{version}"],
                    capture_output=True, text=True,
                )
                with self.subTest(version=version):
                    self.assertEqual(result.returncode, code, result.stderr)
                    if code == 0:
                        self.assertEqual(result.stdout, f"{version}\n")

    def test_release_flags_make_a_prerelease_never_latest(self):
        self.assertEqual(release.release_flags("2.0.0-alpha.1"), {"prerelease": "true", "make_latest": "false"})
        self.assertEqual(release.release_flags("2.0.0-rc.1"), {"prerelease": "true", "make_latest": "false"})
        self.assertEqual(release.release_flags("2.0.0"), {"prerelease": "false", "make_latest": ""})
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text('[package]\nversion = "2.0.0-alpha.1"\n')
            output = subprocess.run(
                [sys.executable, SPEC.origin, "--manifest", manifest, "release-flags", "--tag", "v2.0.0-alpha.1"],
                check=True, capture_output=True, text=True,
            ).stdout
            self.assertEqual(output, "prerelease=true\nmake_latest=false\n")
            mismatch = subprocess.run(
                [sys.executable, SPEC.origin, "--manifest", manifest, "release-flags", "--tag", "v2.0.0"],
                capture_output=True, text=True,
            )
            self.assertEqual(mismatch.returncode, 1)
            self.assertEqual(mismatch.stdout, "")

    def test_release_workflow_creates_prerelease_drafts_that_are_never_latest(self):
        workflow = (REPOSITORY / ".github/workflows/release.yml").read_text()
        self.assertIn(
            "      - name: Select GitHub release kind\n"
            "        id: release-kind\n"
            '        run: python scripts/package-release.py release-flags --tag "$RELEASE_TAG" >> "$GITHUB_OUTPUT"\n',
            workflow,
        )
        create = workflow[workflow.index("      - name: Create draft GitHub release\n"):]
        self.assertIn("          draft: true\n", create)
        self.assertIn("          prerelease: ${{ steps.release-kind.outputs.prerelease }}\n", create)
        self.assertIn("          make_latest: ${{ steps.release-kind.outputs.make_latest }}\n", create)
        self.assertLess(workflow.index("id: release-kind"), workflow.index("- name: Create draft GitHub release"))
        # Every job validates the tag before it builds anything.
        self.assertEqual(workflow.count("package-release.py validate-tag"), 2)

    def test_nfpm_receives_rendered_native_versions(self):
        config = (REPOSITORY / "packaging/linux/nfpm.yaml").read_text()
        # nfpm's default SemVer split omits the prerelease from Arch .PKGINFO.
        self.assertIn("version: ${VERSION}\n", config)
        self.assertIn("\nversion_schema: none\n", config)
        calls = []
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text('[package]\nversion = "2.0.0-alpha.1"\n')
            binary = root / "vosu-binary"
            binary.write_bytes(b"explicit non-daemon fixture")
            args = unittest.mock.Mock(tag="v2.0.0-alpha.1", binary=binary, output=root / "dist", nfpm="nfpm")
            with unittest.mock.patch.object(linux, "check_glibc"), \
                 unittest.mock.patch.object(linux, "run", lambda *command, env: calls.append((command, env["VERSION"]))), \
                 unittest.mock.patch.object(linux, "verify_package", lambda path, version: calls.append((path.name, version))), \
                 unittest.mock.patch.object(linux.release, "verify_archive"):
                # build() reads the root Cargo.toml relative to the working directory.
                previous = Path.cwd()
                os.chdir(root)
                try:
                    linux.build(args)
                finally:
                    os.chdir(previous)
        built = [(command[command.index("--packager") + 1], Path(command[command.index("--target") + 1]).name, version)
                 for command, version in calls[0::2]]
        self.assertEqual(built, [
            ("deb", "vosu_2.0.0-alpha.1_amd64.deb", "2.0.0~alpha.1"),
            ("rpm", "vosu-2.0.0-alpha.1-1.x86_64.rpm", "2.0.0~alpha.1"),
            ("archlinux", "vosu-2.0.0-alpha.1-1-x86_64.pkg.tar.zst", "2.0.0alpha.1"),
        ])
        self.assertEqual(calls[1::2], [(name, version) for _, name, version in built])

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

    def test_release_notes_select_the_exact_prerelease_or_stable_section(self):
        changelog = (
            "# Changelog\n\n## 2.0.0-alpha.1 — 2026-11-24\n\n- first alpha\n\n"
            "## 1.2.5 — 2026-11-22\n\n- stable\n"
        )
        notes = release.release_notes("v2.0.0-alpha.1", changelog, signed=False)
        self.assertIn("first alpha", notes)
        self.assertNotIn("stable", notes.split("## Code signing policy")[0])
        # `## 2.0.0` must not resolve to the alpha section.
        for tag in ("v2.0.0", "v2.0.0-alpha"):
            with self.subTest(tag=tag), self.assertRaisesRegex(ValueError, "no section"):
                release.release_notes(tag, changelog, signed=False)

    def test_release_set_requires_distribution_packages_and_plugin_header(self):
        import argparse
        for version in ("1.2.3", "2.0.0-alpha.1"):
            with self.subTest(version=version), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                manifest = root / "Cargo.toml"
                manifest.write_text(f'[package]\nversion = "{version}"\n')
                dist = root / "dist"
                dist.mkdir()
                license_path = root / "LICENSE"
                license_path.write_bytes(b"explicit test fixture license")
                header_path = root / "include" / "vosu_plugin.h"
                header_path.parent.mkdir()
                header_bytes = (REPOSITORY / "include" / "vosu_plugin.h").read_bytes()
                header_path.write_bytes(header_bytes)
                args = argparse.Namespace(tag=f"v{version}", manifest=manifest,
                                          directory=dist, license=license_path,
                                          header=header_path)
                for platform, executable in release.PLATFORM_EXECUTABLE.items():
                    with zipfile.ZipFile(dist / f"vosu-{platform}-v{version}.zip", "w") as archive:
                        archive.writestr(executable, b"explicit non-daemon fixture")
                        archive.writestr("LICENSE", license_path.read_bytes())
                names = [package.asset for package in release.distribution_packages(version)]
                # Only the release-set filename consumer is under test here; native
                # package contents are exercised with real binaries, not fixtures.
                for name in names:
                    (dist / name).write_bytes(b"explicit asset-name fixture")
                (dist / release.PLUGIN_HEADER_ASSET).write_bytes(header_bytes)
                release.verify_release_set(args)
                for name in (*names, release.PLUGIN_HEADER_ASSET):
                    (dist / name).unlink()
                    with self.subTest(missing=name), self.assertRaises(ValueError):
                        release.verify_release_set(args)
                    (dist / name).write_bytes(
                        header_bytes if name == release.PLUGIN_HEADER_ASSET else b"explicit asset-name fixture"
                    )
                # Modified standalone vosu_plugin.h fails SHA-256 verification.
                tampered = bytearray(header_bytes)
                tampered[-1] ^= 0x01
                (dist / release.PLUGIN_HEADER_ASSET).write_bytes(bytes(tampered))
                with self.assertRaisesRegex(ValueError, "sha256"):
                    release.verify_release_set(args)
                (dist / release.PLUGIN_HEADER_ASSET).write_bytes(header_bytes)
                (dist / "unexpected").touch()
                with self.assertRaises(ValueError):
                    release.verify_release_set(args)
                (dist / "unexpected").unlink()
                if version == "2.0.0-alpha.1":
                    # nfpm's conventional tilde name is not the published asset.
                    (dist / names[0]).rename(dist / "vosu_2.0.0~alpha.1_amd64.deb")
                    with self.assertRaises(ValueError):
                        release.verify_release_set(args)

    def test_package_header_cli_stages_exact_copy_and_keeps_daemon_zip_two_entry(self):
        real_header = REPOSITORY / "include" / "vosu_plugin.h"
        self.assertTrue(real_header.is_file())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "Cargo.toml"
            manifest.write_text('[package]\nversion = "2.0.0"\n')
            license_path = root / "LICENSE"
            license_path.write_bytes(b"fixture license\n")
            binary = root / "vosu"
            binary.write_bytes(b"fixture binary")
            dist = root / "dist"
            subprocess.run(
                [sys.executable, SPEC.origin, "--manifest", manifest, "package",
                 "--platform", "linux", "--tag", "v2.0.0", "--binary", binary,
                 "--license", license_path, "--output", dist],
                check=True, capture_output=True, text=True,
            )
            with zipfile.ZipFile(dist / "vosu-linux-v2.0.0.zip") as archive:
                self.assertEqual(set(archive.namelist()), {"vosu", "LICENSE"})
                self.assertNotIn("vosu_plugin.h", archive.namelist())
            result = subprocess.run(
                [sys.executable, SPEC.origin, "--manifest", manifest, "package-header",
                 "--tag", "v2.0.0", "--header", real_header, "--output", dist],
                check=True, capture_output=True, text=True,
            )
            staged = dist / "vosu_plugin.h"
            self.assertEqual(result.stdout.strip(), str(staged))
            self.assertEqual(staged.read_bytes(), real_header.read_bytes())
            self.assertEqual(
                release.verify_plugin_header(staged, real_header),
                release.sha256_bytes(real_header.read_bytes()),
            )

    def test_release_workflow_stages_verifies_and_uploads_standalone_plugin_header(self):
        workflow = (REPOSITORY / ".github/workflows/release.yml").read_text()
        self.assertIn("python scripts/package-release.py package-header", workflow)
        self.assertIn("python scripts/package-release.py verify-set", workflow)
        create = workflow[workflow.index("      - name: Create draft GitHub release\n"):]
        self.assertIn("dist/vosu_plugin.h", create)


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


class PrereleasePublishTests(unittest.TestCase):
    def test_prerelease_tags_are_detected(self):
        self.assertTrue(sign_release.is_prerelease_tag("v2.0.0-alpha.1"))
        self.assertTrue(sign_release.is_prerelease_tag("v2.0.0-rc.2"))
        self.assertFalse(sign_release.is_prerelease_tag("v2.0.0"))
        self.assertFalse(sign_release.is_prerelease_tag("v1.2.4"))

    def test_malformed_tag_is_rejected_before_any_release_lookup(self):
        # The old prefix regex accepted these and would have signed them.
        for tag in ("v2.0.0-alpha.01", "v2.0.0-", "2.0.0-alpha.1", "v2.0.0-alpha.1+build"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                sign_release.is_prerelease_tag(tag)
        calls = []
        with unittest.mock.patch.object(sign_release, "run", lambda *command, capture=False: calls.append(command)), \
             unittest.mock.patch.object(sign_release, "find_release", lambda repo, tag: calls.append(tag)), \
             self.assertRaisesRegex(SystemExit, "SemVer"):
            sign_release.main(["--tag", "v2.0.0-alpha.01", "--publish"])
        self.assertEqual(calls, [])

    def test_publishing_a_prerelease_tag_sets_prerelease_and_not_latest(self):
        calls = []

        def fake_run(*command, capture=False):
            calls.append(command)
            return "" if capture else None

        def fake_find_release(repo, tag):
            return {"id": 42, "draft": True, "assets": []}

        with unittest.mock.patch.object(sign_release, "run", fake_run), \
             unittest.mock.patch.object(sign_release, "find_release", fake_find_release):
            sign_release.main(["--tag", "v2.0.0-alpha.1", "--publish"])

        patch_call = next(c for c in calls if c[:2] == ("gh", "api"))
        self.assertIn("prerelease=true", patch_call)
        self.assertIn("make_latest=false", patch_call)

    def test_publishing_a_stable_tag_does_not_touch_prerelease_flags(self):
        calls = []

        def fake_run(*command, capture=False):
            calls.append(command)
            return "" if capture else None

        def fake_find_release(repo, tag):
            return {"id": 42, "draft": True, "assets": []}

        with unittest.mock.patch.object(sign_release, "run", fake_run), \
             unittest.mock.patch.object(sign_release, "find_release", fake_find_release):
            sign_release.main(["--tag", "v1.2.4", "--publish"])

        patch_call = next(c for c in calls if c[:2] == ("gh", "api"))
        self.assertNotIn("prerelease=true", patch_call)
        self.assertNotIn("make_latest=false", patch_call)
        self.assertIn("draft=false", patch_call)


if __name__ == "__main__":
    unittest.main()
