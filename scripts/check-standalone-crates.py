#!/usr/bin/env python3
"""Guard against windows-sys (and other target-cfg) feature gaps that
workspace feature unification hides.

Building the workspace together unions every member's Cargo feature
requests, so a crate missing a feature it needs standalone (e.g.
windows-sys's `Win32_Graphics_Gdi` for `WNDCLASSW`) only fails when that
crate is built *alone* — exactly what a consumer of the published crate, or
`cargo check -p <crate>`, does. `cargo clippy --workspace` and `cargo xwin
clippy --workspace` (scripts/prepush.py's "clippy-linux" and "xwin-clippy"
steps) do not catch this.

This runs `cargo check -p <member> --all-targets --all-features --locked`
for every workspace member, on the host (Linux) target and, unless
--linux-only is passed, cross-compiled for x86_64-pc-windows-msvc via
cargo-xwin. Vendored/provenance-pinned crates (crates/vosu-pp/rosu-*-api)
are included: they are still workspace members a bare `cargo check -p`
can hit.
"""

import argparse
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def cargo_metadata():
    output = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1", "--offline"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return json.loads(output)


def workspace_package_names(metadata):
    members = set(metadata["workspace_members"])
    return sorted(pkg["name"] for pkg in metadata["packages"] if pkg["id"] in members)


def run_check(cmd, env):
    import os

    full_env = dict(os.environ)
    full_env.update(env)
    completed = subprocess.run(cmd, cwd=ROOT, env=full_env, capture_output=True, text=True)
    return completed.returncode, completed.stdout + completed.stderr


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--linux-only",
        action="store_true",
        help="Skip the cargo-xwin pass (for machines without the Windows SDK cache).",
    )
    args = parser.parse_args()

    import os

    target_dir = os.environ.get("CARGO_TARGET_DIR", "/tmp/vosu-target-prepush")
    base_env = {"CARGO_INCREMENTAL": "0", "CARGO_TARGET_DIR": target_dir}
    linux_env = {**base_env, "C_INCLUDE_PATH": "/usr/include/x86_64-linux-gnu"}
    xwin_env = {**base_env, "C_INCLUDE_PATH": ""}

    metadata = cargo_metadata()
    names = workspace_package_names(metadata)

    failures = []
    for name in names:
        rc, log = run_check(
            ["cargo", "check", "--package", name, "--all-targets", "--all-features", "--locked"],
            linux_env,
        )
        if rc != 0:
            failures.append((name, "linux", log))
        if not args.linux_only:
            rc, log = run_check(
                [
                    "cargo", "xwin", "check", "--package", name,
                    "--target", "x86_64-pc-windows-msvc",
                    "--all-targets", "--all-features", "--locked",
                ],
                xwin_env,
            )
            if rc != 0:
                failures.append((name, "windows", log))

    if failures:
        for name, platform, log in failures:
            print(f"=== {name} ({platform}) ===", file=sys.stderr)
            print(log[-4000:], file=sys.stderr)
        crate_list = ", ".join(sorted({f"{name} ({platform})" for name, platform, _ in failures}))
        print(
            f"\nerror: {len(failures)} standalone crate check(s) failed: {crate_list}\n"
            "Each workspace member must declare every Cargo feature it uses itself "
            "(e.g. windows-sys features); do not rely on another workspace member's "
            "request to unify it in.",
            file=sys.stderr,
        )
        return 1

    print(f"standalone check passed for {len(names)} workspace member(s) on Linux"
          + ("" if args.linux_only else " and Windows (xwin)"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
