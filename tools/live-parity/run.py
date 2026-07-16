#!/usr/bin/env python3
"""CLI composition entrypoint for deterministic upstream-source-versus-vosu live parity."""
from __future__ import annotations

import argparse
from functools import wraps
import json
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))

import parity_process
import parity_artifacts
import parity_config
import parity_probes
import parity_build
import parity_daemon
import parity_capture
import parity_verify
import parity_compare
import parity_diff

_MODULES = (
    parity_process,
    parity_artifacts,
    parity_config,
    parity_probes,
    parity_build,
    parity_daemon,
    parity_capture,
    parity_verify,
    parity_compare,
    parity_diff,
)
for _module in _MODULES:
    globals().update({
        name: value for name, value in vars(_module).items()
        if not name.startswith("__")
    })


def _facade(function, module):
    """Keep direct-script monkey-patching behavior from the former monolith."""
    @wraps(function)
    def call(*args, **kwargs):
        current = globals()
        for name in tuple(vars(module)):
            if name != function.__name__ and name in current:
                setattr(module, name, current[name])
        return function(*args, **kwargs)
    return call


for _module in _MODULES:
    for _name, _value in tuple(vars(_module).items()):
        if callable(_value) and getattr(_value, "__module__", None) == _module.__name__:
            globals()[_name] = _facade(_value, _module)


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description=(f"Capture and compare pinned upstream tosu "
                     f"{PINNED_SOURCE_VERSION} and vosu {PINNED_VOSU_VERSION} "
                     "live schemas"),
        epilog=("Prerequisites: either the pinned upstream tosu source repository with "
                "canonical Node/pnpm or an exact hash-pinned native upstream tosu reference, "
                "an exact supported osu client executable with expected digest and version. "
                "The gate creates fresh controlled build roots, launches or attaches to and "
                "attests the client, and requires distinct loopback ports. On Windows use "
                "stable/lazer; on Linux use lazer. Manually traverse menu -> active play -> "
                "paused -> result, then close the client. Artifact closures, hashes, process "
                "identities, and socket ownership are attested."),
    )
    sub = p.add_subparsers(dest="action", required=True)
    sub.add_parser("config-schema", help="print the strict run-config JSON Schema")
    cap = sub.add_parser("capture", help="build and launch both daemons and client, then capture")
    cap.add_argument("--config", required=True, help="JSON run configuration")
    cap.add_argument("--out", required=True, help="raw capture JSON")
    cap.add_argument("--timeout", type=float, default=900.0)
    cap.add_argument("--interval-ms", type=int, default=25)
    cmp = sub.add_parser("compare", help="compare a completed raw capture")
    cmp.add_argument("--capture", required=True)
    cmp.add_argument("--report", required=True)
    diff = sub.add_parser("diff", help="export canonical synchronized payloads and unified diffs")
    diff.add_argument("--capture", required=True)
    diff.add_argument("--out-dir", required=True)
    check = sub.add_parser("check-config", help="validate live prerequisites without launching")
    check.add_argument("--config", required=True)
    return p


def main() -> int:
    args = parser().parse_args()
    try:
        if args.action == "config-schema":
            print(json.dumps(config_schema(), indent=2, sort_keys=True))
            return 0
        if args.action == "check-config":
            cfg = load_config(args.config)
            print(json.dumps({"valid": True, "matrix": f"{cfg['os'].lower()}/{cfg['client'].lower()}",
                              "artifacts": cfg["_artifact_identity"]}, sort_keys=True))
            return 0
        if args.action == "capture":
            return capture(load_config(args.config), args.out, args.timeout, args.interval_ms)
        if args.action == "diff":
            return export_diffs(args.capture, args.out_dir)
        return compare(args.capture, args.report)
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"vosu-live-parity: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
