"""Classify a GitHub event without skipping the required CI status on docs-only PRs."""

import json
import os
from pathlib import Path
import subprocess


UI_PREFIXES = ("assets/server/", "src/server/", "src/tools/")


def is_ui_path(path):
    if path.startswith(UI_PREFIXES):
        return True
    if path.startswith("src/runtime/") and Path(path).name.startswith("render"):
        return True
    if path.startswith("tests/"):
        name = Path(path).name
        if "smoke" in name or "browser" in name:
            return True
    return False


def classify(paths):
    code = False
    native = False
    ui = False
    for path in paths:
        if path.endswith(".md") or path.startswith("docs/") or path in {".gitignore", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE"}:
            continue
        code = True
        # Pure schema/calculator changes retain the Windows cross-check. Everything
        # else defaults to native verification; new shared/platform files fail safe.
        # `src/calculation/` moved out of the root package in M0: the JS-compatible
        # root helpers, the calculation engines, and the osu! value types it
        # covered now live in `crates/vosu-osu-model/` and `crates/vosu-osu-calc/`.
        if not path.startswith(
            (
                "src/api/",
                "src/model/",
                "crates/vosu-osu-model/",
                "crates/vosu-osu-calc/",
                "crates/vosu-pp/",
                "benches/",
                "tools/oracle/",
            )
        ):
            native = True
        if is_ui_path(path):
            ui = True
    return code, native, ui


def main():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    if os.environ["GITHUB_EVENT_NAME"] == "workflow_dispatch":
        code, native, ui = True, True, True
    else:
        if "pull_request" in event:
            base = event["pull_request"]["base"]["sha"]
            head = event["pull_request"]["head"]["sha"]
        else:
            base, head = event.get("before", ""), event["after"]
        if not base or set(base) == {"0"}:
            code, native, ui = True, True, True
        else:
            # Include both sides of moves so relocating native code cannot bypass
            # native verification just because the destination is schema-only.
            result = subprocess.run(["git", "diff", "--no-renames", "--name-only", "-z", base, head], capture_output=True, check=False)
            if result.returncode:
                code, native, ui = True, True, True
            else:
                code, native, ui = classify(result.stdout.decode("utf-8", errors="replace").split("\0")[:-1])
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"code={str(code).lower()}\nnative={str(native).lower()}\nui={str(ui).lower()}\n")
    print(f"Rust validation: {code}; native Windows verification: {native}; dashboard UI verification: {ui}")


if __name__ == "__main__":
    main()
