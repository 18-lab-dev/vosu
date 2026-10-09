#!/usr/bin/env python3
"""Report bounded textual matches; never infer authorship or licensing from them."""

import argparse
import difflib
import hashlib
import json
from pathlib import Path
import re


DASHBOARD = ("homepage.js", "homepage-settings.js", "homepage.html", "report.html",
             "homepage.css", "homepage-controls.css")
WRAPPER = (("model.rs", "model.rs"), ("data.rs", "data.rs"),
           ("performance.rs", "engine.rs"), ("gradual.rs", "gradual.rs"),
           ("scoring.rs", "scoring.rs"))
REFERENCE = "tools/oracle/reference/lazer-calculator/lib/native/Internal"
OSU_TOOLS = tuple(f"PerformanceCalculator/Simulate/{mode}SimulateCommand.cs"
                  for mode in ("Osu", "Taiko", "Catch", "Mania")) + (
    "PerformanceCalculatorGUI/RulesetHelper.cs",)


def lines(path):
    return [re.sub(r"\s+", "", line) for line in path.read_text(encoding="utf-8").splitlines()]


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def dashboard(root, upstream):
    directory = upstream / "packages/server/assets"
    files = sorted(p for p in directory.rglob("*") if p.is_file()
                   and p.suffix.lower() in {".html", ".js", ".css", ".scss"}
                   and "vue" not in p.name.lower())
    if not files:
        raise ValueError(f"no tosu dashboard inputs in {directory}")
    index = {}
    for path in files:
        text = lines(path)
        for start in range(len(text) - 4):
            block = tuple(text[start:start + 5])
            if len("".join(block)) > 100:
                index.setdefault(block, []).append({
                    "path": path.relative_to(upstream).as_posix(), "line": start + 1,
                })
    matches = []
    for name in DASHBOARD:
        path = root / "assets/server" / name
        text = lines(path)
        for start in range(len(text) - 4):
            block = tuple(text[start:start + 5])
            if block in index:
                matches.append({"path": path.relative_to(root).as_posix(),
                                "line": start + 1, "upstream": index[block]})
    return {"local_files": {name: digest(root / "assets/server" / name) for name in DASHBOARD},
            "upstream_files": {p.relative_to(upstream).as_posix(): digest(p) for p in files},
            "matches": matches}


def wrapper(root, former):
    result = []
    for current, previous in WRAPPER:
        path = root / "crates/vosu-pp/src" / current
        old = former / "crates/vosu-lazer-pp/src" / previous
        a, b = lines(path), lines(old)
        blocks = difflib.SequenceMatcher(None, a, b, autojunk=False).get_matching_blocks()
        matches = [{"current_lines": [m.a + 1, m.a + m.size],
                    "former_lines": [m.b + 1, m.b + m.size]}
                   for m in blocks if m.size >= 5 and len("".join(a[m.a:m.a + m.size])) > 100]
        result.append({"current": path.relative_to(root).as_posix(), "current_sha256": digest(path),
                       "former": old.relative_to(former).as_posix(), "former_sha256": digest(old),
                       "matches": matches})
    return result


def reference_upstream(root, tools):
    """List lazer-calculator simulator/accuracy lines absent from pinned MIT osu-tools."""
    known = set()
    for name in OSU_TOOLS:
        known.update(lines(tools / name))
    result = []
    for name in ("ScoreSimulator.cs", "AccuracyCalculator.cs"):
        path = root / REFERENCE / name
        residual = [{"line": number, "text": text}
                    for number, text in enumerate(lines(path), 1) if text and text not in known]
        result.append({"reference": path.relative_to(root).as_posix(), "reference_sha256": digest(path),
                       "lines_absent_from_osu_tools": residual})
    return {"osu_tools_files": {name: digest(tools / name) for name in OSU_TOOLS},
            "reference_files": result}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--tosu", type=Path, action="append", default=[])
    parser.add_argument("--former-wrapper", type=Path)
    parser.add_argument("--osu-tools", type=Path,
                        help="ppy/osu-tools checkout at d82cf2a986218f80c18dea5cc1399133d4a8586d")
    args = parser.parse_args()
    if not args.tosu and args.former_wrapper is None and args.osu_tools is None:
        parser.error("provide a pinned --tosu or --osu-tools checkout, or a --former-wrapper tree")
    result = {"method": "Whitespace-normalized lines, at least five lines and over 100 characters; dashboard five-line windows and wrapper SequenceMatcher with autojunk disabled. The osu-tools comparison lists every nonblank reference line absent from the pinned osu-tools files.",
              "limitations": "Textual similarity only; no cross-language translation detector or conclusion about source origin, copyright, or applicable license."}
    try:
        result["dashboard"] = [dashboard(args.root, path) for path in args.tosu]
        if args.former_wrapper is not None:
            result["former_wrapper"] = wrapper(args.root, args.former_wrapper)
        if args.osu_tools is not None:
            result["reference_upstream"] = reference_upstream(args.root, args.osu_tools)
    except (OSError, ValueError) as error:
        parser.exit(1, f"error: {error}\n")
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
