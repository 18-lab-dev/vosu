"""Classify a diff as Windows-sensitive.

scripts/prepush.py uses `windows_sensitive` to decide whether its native
Windows VM phase (native-clippy, native-test-workspace, native-test-doc) needs
to run locally. Hosted CI's `windows-native` job still runs for every Rust
change (see `scripts/ci-changes.py`'s `native` flag), so this classifier only
controls the slower local VM phase: a false negative here is caught by hosted
CI before landing, a false positive only costs local time. Fail safe anyway
when the diff cannot be computed.
"""

import re
from pathlib import Path

# Path prefixes (relative to the repo root, forward slashes) whose changes are
# always Windows-sensitive: the native process/platform boundary, the updater
# staging/replacement code, process- and platform-specific modules, and the
# plugin host's native-library loading.
_SENSITIVE_PREFIXES = (
    "crates/vosu-sys/",
    "src/updater/",
    "crates/vosu-core/src/updater/",
    "src/process",
    "src/platform",
    "crates/vosu-osu-reader/src/process",
    ".cargo/",
    ".github/workflows/",
)

# Exact paths: the plugin host's dlopen/registry code, and the files that
# drive the native CI wiring itself (changing either needs the real VM run to
# prove the wiring still works, not just hosted CI on the next push).
_SENSITIVE_EXACT_PATHS = (
    "crates/vosu-core/src/plugin/registry.rs",
    "crates/vosu-core/src/plugin/instance.rs",
    "scripts/native-ci.ps1",
    "scripts/prepush.py",
)

# Cargo/toolchain manifests anywhere in the tree: a dependency or toolchain
# change can add or change platform-specific code paths.
_SENSITIVE_FILENAMES = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml")

# Added or removed diff lines containing any of these mark the change
# platform-sensitive even when the touched file's path alone would not (e.g.
# a new `#[cfg(windows)]` branch inside an otherwise-portable module).
_SENSITIVE_DIFF_MARKERS = (
    "cfg(windows)",
    'cfg(target_os = "windows")',
    "cfg(not(windows))",
    "windows_sys",
    "#[cfg(unix)]",
    "target_os",
)

_UNSAFE_RE = re.compile(r"\bunsafe\b")


def _path_is_sensitive(path):
    lowered = path.lower()
    if "windows" in lowered or "win32" in lowered:
        return True
    if path.endswith(".ps1"):
        return True
    name = Path(path).name
    if name == "build.rs":
        return True
    if name in _SENSITIVE_FILENAMES:
        return True
    if path.startswith(_SENSITIVE_PREFIXES):
        return True
    if path in _SENSITIVE_EXACT_PATHS:
        return True
    return False


def _diff_lines_are_sensitive(diff_text):
    for line in diff_text.splitlines():
        if not line or line[0] not in "+-" or line.startswith("+++") or line.startswith("---"):
            continue
        content = line[1:]
        if any(marker in content for marker in _SENSITIVE_DIFF_MARKERS):
            return True
        if line[0] == "+" and _UNSAFE_RE.search(content):
            return True
    return False


def windows_sensitive(changed_paths, diff_text):
    """True when the local native Windows VM phase should run for this diff.

    `changed_paths` is a list of repo-relative paths; `diff_text` is the
    unified diff of the same change. Either being `None` (the diff could not
    be computed, e.g. no network access to fetch the base branch) fails safe
    and returns True.
    """
    if changed_paths is None or diff_text is None:
        return True
    if any(_path_is_sensitive(path) for path in changed_paths):
        return True
    return _diff_lines_are_sensitive(diff_text)
