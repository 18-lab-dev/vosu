#!/usr/bin/env python3
"""Check or refresh assets/SHA256SUMS for the embedded assets and release LICENSE.

Runs on Windows release runners too, where `sha256sum` is not guaranteed.
"""

import argparse
import hashlib
from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "assets/SHA256SUMS"


def rows():
    for line in MANIFEST.read_text(encoding="utf-8").splitlines():
        expected, separator, name = line.partition("  ")
        if not separator or len(expected) != 64 or Path(name).is_absolute() or ".." in Path(name).parts:
            raise ValueError(f"malformed manifest row: {line!r}")
        yield expected, name


def digest(name):
    return hashlib.sha256((ROOT / name).read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help="rewrite digests for the listed files")
    args = parser.parse_args()
    try:
        entries = list(rows())
        if args.write:
            MANIFEST.write_text("".join(f"{digest(name)}  {name}\n" for _, name in entries),
                                encoding="utf-8", newline="\n")
            print(f"Refreshed {len(entries)} digests")
            return 0
        stale = [name for expected, name in entries if digest(name) != expected]
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    if stale:
        print("checksum mismatch: " + ", ".join(stale)
              + "; review the change, then run scripts/check-asset-hashes.py --write", file=sys.stderr)
        return 1
    print(f"All {len(entries)} asset digests match")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
