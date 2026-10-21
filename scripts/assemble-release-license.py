#!/usr/bin/env python3
"""Check or refresh the retained calculator and font notices in the release LICENSE.

The updater requires a two-entry ZIP, so these notices travel inside LICENSE.
This is not a complete inventory of all linked dependency license obligations.
"""

import argparse
import hashlib
from pathlib import Path
import sys


MARKER = b"\n===== Retained calculator notices =====\n\n"
SOURCE = "crates/vosu-pp/THIRD_PARTY_LICENSES.txt"
# The dashboard fonts are embedded in the executable, so the release must
# carry their SIL Open Font License text.
FONT_MARKER = b"\n===== Retained font notices =====\n\n"
FONT_SOURCE = "assets/server/fonts-OFL.txt"
# Digests of the pinned MIT texts with CRLF and surrounding LF framing normalized.
# Original source-byte digests and revision links remain in SOURCE.
RETAINED_TEXT_SHA256 = {
    "MaxOhn/rosu-pp": "2faa2179bb64650cfead17ce8487e24f2a018d9471ad9f43e1b04353d89a585d",
    "ppy/osu": "5ee23b58e0e3a34c0d62113cde6eb709b7f72efe6df1fff02679d10d62647dc1",
    "ppy/osu-framework": "54244abaef14491253a187a8b5d147168a356297c49e8cf8156138aa39c17031",
    "MaxOhn/rosu-map": "0354bcd52e08e0464a41cf202b6bb7dc8a779bec206ae7b218544ec2ed360079",
    "ppy/osu-tools": "5608e432587c9cd040c109c7e56c7bfe84891f925bdb1f967fc3a9d626c0e784",
    "dotnet/runtime": "ae48df11a335dc1a615f4f938b69cba73bcf4485c4f97af49b38efb0f216353b",
}


def expected_license(root):
    current = (root / "LICENSE").read_bytes()
    if current.count(MARKER) > 1:
        raise ValueError("duplicate calculator appendix markers in LICENSE")
    if current.count(FONT_MARKER) > 1:
        raise ValueError("duplicate font appendix markers in LICENSE")
    first_party = current.split(MARKER)[0].rstrip(b"\n") + b"\n"
    source = (root / SOURCE).read_bytes()
    for name, expected in RETAINED_TEXT_SHA256.items():
        marker = f"MIT license text ({name})\n".encode()
        if source.count(marker) != 1:
            raise ValueError(f"missing or duplicate retained license section: {name}")
        section = source.split(marker, 1)[1].partition(b"\n\n")[2]
        text = section.partition(b"\nMIT license text (")[0]
        normalized = text.replace(b"\r\n", b"\n").strip(b"\n")
        if hashlib.sha256(normalized).hexdigest() != expected:
            raise ValueError(f"retained MIT terms differ from the pinned source: {name}")
    introduction = (
        f"Source: {SOURCE}\n"
        "The following material retains its own license terms.\n"
        "This appendix does not certify complete distribution compliance.\n\n"
    ).encode()
    font_source = (root / FONT_SOURCE).read_bytes()
    if b"SIL OPEN FONT LICENSE Version 1.1" not in font_source:
        raise ValueError(f"{FONT_SOURCE} must contain the SIL Open Font License 1.1 text")
    font_introduction = f"Source: {FONT_SOURCE}\n\n".encode()
    return (first_party + MARKER + introduction + source.rstrip(b"\n") + b"\n"
            + FONT_MARKER + font_introduction + font_source)


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
            print("Refreshed LICENSE appendix")
        elif path.read_bytes() != expected:
            raise ValueError("LICENSE appendix is stale; run this script with --write")
        else:
            print("LICENSE contains the exact retained calculator and font notices")
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
