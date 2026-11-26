"""Exercise the SDK publication guard without running Cargo."""

import contextlib
import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("check_sdk_published", Path(__file__).with_name("check-sdk-published.py"))
guard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard)

VERSION = "1.0.0-alpha.2"
PUBLISHED = b"pub const ANSWER: i32 = 42;\n"


class CheckSdkPublishedTest(unittest.TestCase):
    def setUp(self):
        workspace = self.enterContext(tempfile.TemporaryDirectory())
        self.root = Path(workspace)
        self.sdk = self.root / guard.SDK_DIR
        self.sdk.mkdir(parents=True)
        digest = hashlib.sha256(PUBLISHED).hexdigest()
        (self.sdk / "published.toml").write_text(
            f'["{VERSION}"]\nregistry_sha256 = "{"0" * 64}"\n'
            f'["{VERSION}".files]\n"src/lib.rs" = "{digest}"\n',
            encoding="utf-8",
        )
        self.stdout = self.enterContext(contextlib.redirect_stdout(io.StringIO()))
        self.stderr = self.enterContext(contextlib.redirect_stderr(io.StringIO()))

    def write_manifest(self, version):
        (self.sdk / "Cargo.toml").write_text(f'[package]\nversion = "{version}"\n', encoding="utf-8")

    def archive(self, files):
        prefix = f"{guard.CRATE}-{VERSION}"
        archive = self.root / f"{prefix}.crate"
        with tarfile.open(archive, "w:gz") as package:
            for name, data in files.items():
                member = tarfile.TarInfo(f"{prefix}/{name}")
                member.size = len(data)
                package.addfile(member, io.BytesIO(data))
        return archive

    def test_published_version_with_identical_files_passes_despite_vcs_metadata(self):
        self.write_manifest(VERSION)
        archive = self.archive({"src/lib.rs": PUBLISHED, ".cargo_vcs_info.json": b'{"git":{"sha1":"other"}}'})
        with patch.object(guard, "package_sdk", return_value=archive):
            self.assertEqual(guard.check(self.root), 0)
        self.assertIn("matches the published package", self.stdout.getvalue())

    def test_published_version_with_changed_file_fails(self):
        self.write_manifest(VERSION)
        archive = self.archive({"src/lib.rs": b"pub const ANSWER: i32 = 43;\n"})
        with patch.object(guard, "package_sdk", return_value=archive):
            self.assertEqual(guard.check(self.root), 1)
        message = self.stderr.getvalue()
        self.assertIn(VERSION, message)
        self.assertIn("bump the SDK version", message)
        self.assertIn("changed: src/lib.rs", message)

    def test_published_version_with_different_file_list_fails(self):
        self.write_manifest(VERSION)
        archive = self.archive({"src/new.rs": PUBLISHED})
        with patch.object(guard, "package_sdk", return_value=archive):
            self.assertEqual(guard.check(self.root), 1)
        message = self.stderr.getvalue()
        self.assertIn("removed: src/lib.rs", message)
        self.assertIn("added: src/new.rs", message)

    def test_unpublished_version_passes_without_packaging(self):
        self.write_manifest("1.0.0-alpha.3")
        with patch.object(guard, "package_sdk") as package:
            self.assertEqual(guard.check(self.root), 0)
            package.assert_not_called()


if __name__ == "__main__":
    unittest.main()
