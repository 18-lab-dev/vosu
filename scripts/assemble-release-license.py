#!/usr/bin/env python3
"""Check or refresh the retained calculator notices in the release LICENSE.

The updater requires a two-entry ZIP, so these notices travel inside LICENSE.
This is not a complete inventory of all linked dependency license obligations.
"""

import argparse
from pathlib import Path
import sys


MARKER = b"\n===== Retained calculator notices =====\n\n"
SOURCE = "crates/vosu-pp/THIRD_PARTY_LICENSES.txt"


def expected_license(root):
    current = (root / "LICENSE").read_bytes()
    if current.count(MARKER) > 1:
        raise ValueError("duplicate calculator appendix markers in LICENSE")
    first_party = current.split(MARKER)[0].rstrip(b"\n") + b"\n"
    source = (root / SOURCE).read_bytes()
    for name in ("MaxOhn/rosu-pp", "ppy/osu", "ppy/osu-framework", "MaxOhn/rosu-map"):
        if source.count(f"MIT license text ({name})".encode()) != 1:
            raise ValueError(f"missing or duplicate retained license section: {name}")
    introduction = (
        f"Source: {SOURCE}\n"
        "The following material retains its own license terms.\n"
        "Other dependency notice coverage remains under review in issue #6.\n\n"
    ).encode()
    return first_party + MARKER + introduction + source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    try:
        expected = expected_license(args.root)
        path = args.root / "LICENSE"
        if args.write:
            path.write_bytes(expected)
            print("Refreshed LICENSE appendix; run check-provenance.py --refresh-license-hash")
        elif path.read_bytes() != expected:
            raise ValueError("LICENSE calculator appendix is stale; run this script with --write")
        else:
            print("LICENSE contains the exact retained calculator notices")
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
