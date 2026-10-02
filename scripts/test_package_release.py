"""Exercise license integrity without treating fixture bytes as a native build."""

import importlib.util
from pathlib import Path
import tempfile
import unittest
import warnings
import zipfile


SPEC = importlib.util.spec_from_file_location(
    "package_release", Path(__file__).with_name("package-release.py")
)
release = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release)


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


if __name__ == "__main__":
    unittest.main()
