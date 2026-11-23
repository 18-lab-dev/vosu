"""Table-driven tests for scripts/windows_sensitivity.py's windows_sensitive()."""

import unittest

from windows_sensitivity import windows_sensitive


# Each case: (description, changed_paths, diff_text, expected).
PORTABLE_DIFF = (
    "diff --git a/crates/vosu-core/src/plugin/host.rs b/crates/vosu-core/src/plugin/host.rs\n"
    "--- a/crates/vosu-core/src/plugin/host.rs\n"
    "+++ b/crates/vosu-core/src/plugin/host.rs\n"
    "@@ -1,2 +1,2 @@\n"
    "-fn load(name: &str) -> Plugin {\n"
    "+fn load(name: &str, timeout: u64) -> Plugin {\n"
)

CASES = (
    ("crate boundary", ["crates/vosu-sys/src/lib.rs"], PORTABLE_DIFF, True),
    ("windows substring in path", ["src/counters/windows_catalog.rs"], PORTABLE_DIFF, True),
    ("win32 substring in path", ["crates/vosu-sys/win32/handle.rs"], PORTABLE_DIFF, True),
    ("ps1 file", ["scripts/dev-helper.ps1"], PORTABLE_DIFF, True),
    ("build.rs", ["crates/vosu-sys/build.rs"], PORTABLE_DIFF, True),
    ("root Cargo.toml", ["Cargo.toml"], PORTABLE_DIFF, True),
    ("nested Cargo.toml", ["crates/vosu-core/Cargo.toml"], PORTABLE_DIFF, True),
    ("Cargo.lock", ["Cargo.lock"], PORTABLE_DIFF, True),
    ("rust-toolchain.toml", ["rust-toolchain.toml"], PORTABLE_DIFF, True),
    ("dot cargo config", [".cargo/config.toml"], PORTABLE_DIFF, True),
    ("native-ci.ps1 (also .ps1)", ["scripts/native-ci.ps1"], PORTABLE_DIFF, True),
    ("prepush.py VM staging", ["scripts/prepush.py"], PORTABLE_DIFF, True),
    ("workflow file", [".github/workflows/ci.yml"], PORTABLE_DIFF, True),
    ("added cfg(windows)", ["src/foo.rs"], "+#[cfg(windows)]\n", True),
    ('added cfg(target_os = "windows")', ["src/foo.rs"], '+#[cfg(target_os = "windows")]\n', True),
    ("added cfg(not(windows))", ["src/foo.rs"], "+#[cfg(not(windows))]\n", True),
    ("removed cfg(not(windows))", ["src/foo.rs"], "-#[cfg(not(windows))]\n", True),
    ("added windows_sys use", ["src/foo.rs"], "+use windows_sys::Win32::Foundation::HANDLE;\n", True),
    ("added cfg(unix)", ["src/foo.rs"], "+#[cfg(unix)]\n", True),
    ("added target_os match", ["src/foo.rs"], '+    match target_os {\n', True),
    ("updater dir", ["src/updater/stage.rs"], PORTABLE_DIFF, True),
    ("vosu-core updater dir", ["crates/vosu-core/src/updater/rollback.rs"], PORTABLE_DIFF, True),
    ("src/process* file", ["src/process.rs"], PORTABLE_DIFF, True),
    ("src/process* submodule", ["src/process_tracker/mod.rs"], PORTABLE_DIFF, True),
    ("src/platform* file", ["src/platform.rs"], PORTABLE_DIFF, True),
    ("osu-reader process* file", ["crates/vosu-osu-reader/src/process_memory.rs"], PORTABLE_DIFF, True),
    ("plugin registry.rs", ["crates/vosu-core/src/plugin/registry.rs"], PORTABLE_DIFF, True),
    ("plugin instance.rs", ["crates/vosu-core/src/plugin/instance.rs"], PORTABLE_DIFF, True),
    (
        "unsafe block added",
        ["crates/vosu-core/src/plugin/host.rs"],
        "+    unsafe { ffi_call() };\n",
        True,
    ),
    (
        "unsafe block only removed, not added",
        ["crates/vosu-core/src/plugin/host.rs"],
        "-    unsafe { ffi_call() };\n",
        False,
    ),
    ("diff could not be computed (paths)", None, PORTABLE_DIFF, True),
    ("diff could not be computed (diff text)", ["README.md"], None, True),
    ("docs-only change", ["docs/architecture/overview.md"], "+docs change\n", False),
    (
        "portable plugin host logic change",
        ["crates/vosu-core/src/plugin/host.rs"],
        PORTABLE_DIFF,
        False,
    ),
    ("api projection change", ["src/api/v2/payload.rs"], "+    field: u32,\n", False),
)


class WindowsSensitiveTest(unittest.TestCase):
    def test_cases(self):
        for description, paths, diff_text, expected in CASES:
            with self.subTest(description):
                self.assertEqual(windows_sensitive(paths, diff_text), expected, description)


if __name__ == "__main__":
    unittest.main()
