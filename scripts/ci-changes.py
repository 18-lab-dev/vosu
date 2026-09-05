"""Classify a GitHub event without skipping the required CI status on docs-only PRs."""

import json
import os
from pathlib import Path
import subprocess


def classify(paths):
    code = False
    native = False
    for path in paths:
        if path.endswith(".md") or path.startswith(("docs/", "openwiki/", ".agents/")) or path in {".gitignore", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE"}:
            continue
        code = True
        # Pure schema/calculator changes retain the Windows cross-check. Everything
        # else defaults to native verification; new shared/platform files fail safe.
        if not path.startswith(("src/api/", "src/model/", "src/calculation/", "crates/vosu-pp/", "benches/", "tools/oracle/")):
            native = True
    return code, native


def main():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    if os.environ["GITHUB_EVENT_NAME"] == "workflow_dispatch":
        code, native = True, True
    else:
        if "pull_request" in event:
            base = event["pull_request"]["base"]["sha"]
            head = event["pull_request"]["head"]["sha"]
        else:
            base, head = event.get("before", ""), event["after"]
        if not base or set(base) == {"0"}:
            code, native = True, True
        else:
            # Include both sides of moves so relocating native code cannot bypass
            # native verification just because the destination is schema-only.
            result = subprocess.run(["git", "diff", "--no-renames", "--name-only", "-z", base, head], capture_output=True, check=False)
            if result.returncode:
                code, native = True, True
            else:
                code, native = classify(result.stdout.decode("utf-8", errors="replace").split("\0")[:-1])
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"code={str(code).lower()}\nnative={str(native).lower()}\n")
    print(f"Rust validation: {code}; native Windows verification: {native}")


if __name__ == "__main__":
    main()
