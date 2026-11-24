#!/usr/bin/env python3
"""Enforce the recorded `crate::` dependency graph between each workspace
crate's own top-level `src/` modules.

Every edge in a crate's scripts/module-boundaries/<crate>.txt must still be
exercised by non-test code (a vanished edge is stale and must be removed),
and no new edge may appear without a deliberate rules-file update alongside
docs/architecture/source-map.md. The rules file is also rejected if it is
cyclic, so a cycle broken during M0 cannot silently return.
"""

import argparse
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
RULES_DIR = ROOT / "scripts/module-boundaries"

# Crates whose internal module graph is checked. `vosu-pp` and its vendored
# `rosu-*-api` members, `vosu-test-harness`, and `vosu-release-smoke` keep
# their pre-M0 unchanged role (docs/v2/implementation-plan.md section 2) and
# are out of scope.
CRATE_ROOTS = {
    "vosu": ROOT,
    "vosu-core": ROOT / "crates/vosu-core",
    "vosu-net": ROOT / "crates/vosu-net",
    "vosu-osu-calc": ROOT / "crates/vosu-osu-calc",
    "vosu-osu-model": ROOT / "crates/vosu-osu-model",
    "vosu-osu-reader": ROOT / "crates/vosu-osu-reader",
    "vosu-osu-state": ROOT / "crates/vosu-osu-state",
    "vosu-plugin-api": ROOT / "crates/vosu-plugin-api",
    "vosu-proptest": ROOT / "crates/vosu-proptest",
    "vosu-sys": ROOT / "crates/vosu-sys",
    "vosu-surface": ROOT / "crates/vosu-surface",
}

MOD_DECL = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)\s*;", re.MULTILINE)
CFG_TEST_EXTERNAL_MOD = re.compile(r"#\[cfg\(test\)\]\s*mod\s+([A-Za-z_]\w*)\s*;")
CFG_TEST_INLINE_MOD = re.compile(r"#\[cfg\(test\)\]\s*(?:#\[[^\]]*\]\s*)*mod\s+\w+\s*\{")
BLOCK_COMMENT = re.compile(r"/\*.*?\*/", re.S)
LINE_COMMENT = re.compile(r"//[^\n]*")
RAW_STRING = re.compile(r'r(#*)"(?:.*?)"\1', re.S)
STRING_LITERAL = re.compile(r'"(?:\\.|[^"\\\n])*"')
CHAR_LITERAL = re.compile(r"'(?:\\.|[^'\\\n])'")
CRATE_PATH = re.compile(r"crate::([A-Za-z_]\w*)")
CRATE_GROUP = re.compile(r"crate::\{")
PUB_USE_GROUP = re.compile(r"pub use ([A-Za-z_][\w:]*)::\{([^}]*)\}", re.S)
PUB_USE_SINGLE = re.compile(r"pub use ([A-Za-z_][\w:]*)::([A-Za-z_]\w*)\s*;")


def top_level_modules(lib_text):
    """Top-level modules of a crate: the `mod x;` declarations in its lib.rs."""
    return sorted(set(MOD_DECL.findall(lib_text)))


def owning_module(relative_path, modules):
    parts = Path(relative_path).parts
    name = parts[0] if len(parts) > 1 else Path(parts[0]).stem
    return name if name in modules else None


def is_test_path(relative_path):
    parts = Path(relative_path).parts
    if "tests" in parts[:-1]:
        return True
    return bool(re.match(r"^tests.*\.rs$", parts[-1]))


def cfg_test_external_files(src_files):
    """Files reachable only through `#[cfg(test)] mod name;` (test-only, separate file)."""
    excluded = set()
    for relative_path, text in src_files.items():
        child_dir = Path(relative_path).parent / Path(relative_path).stem
        for name in CFG_TEST_EXTERNAL_MOD.findall(text):
            excluded.add((child_dir / f"{name}.rs").as_posix())
    return excluded


def mask(text):
    """Blank out comments and literals, preserving line/column offsets."""
    chars = list(text)

    def blank(match):
        for index in range(match.start(), match.end()):
            if chars[index] != "\n":
                chars[index] = " "

    for pattern in (RAW_STRING, BLOCK_COMMENT, LINE_COMMENT, STRING_LITERAL, CHAR_LITERAL):
        for match in pattern.finditer(text):
            blank(match)
    return "".join(chars)


def strip_inline_test_blocks(text):
    out = []
    i = 0
    while True:
        match = CFG_TEST_INLINE_MOD.search(text, i)
        if not match:
            out.append(text[i:])
            break
        out.append(text[i:match.start()])
        depth = 1
        j = match.end()
        while j < len(text) and depth > 0:
            if text[j] == "{":
                depth += 1
            elif text[j] == "}":
                depth -= 1
            j += 1
        out.append("\n" * text.count("\n", match.start(), j))
        i = j
    return "".join(out)


def split_top_level(content):
    """Split a `use` group's inner text on top-level commas; yield (offset, text)."""
    depth = 0
    start = 0
    for index, char in enumerate(content):
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        elif char == "," and depth == 0:
            yield start, content[start:index]
            start = index + 1
    yield start, content[start:]


def group_members(text):
    """Yield (offset, module_name) for every member of a `crate::{...}` group."""
    for group in CRATE_GROUP.finditer(text):
        brace = group.end() - 1
        depth = 1
        j = brace + 1
        while j < len(text) and depth > 0:
            if text[j] == "{":
                depth += 1
            elif text[j] == "}":
                depth -= 1
            j += 1
        content_start = brace + 1
        content = text[content_start:j - 1]
        for offset, item in split_top_level(content):
            stripped = item.lstrip()
            leading = len(item) - len(stripped)
            name = re.match(r"[A-Za-z_]\w*", stripped)
            if name:
                yield content_start + offset + leading, name.group(0)


def reexport_symbol_modules(lib_text, modules):
    """Map a symbol this crate re-exports at its root to the top-level
    module that defines it, by reading lib.rs's own `pub use path::{...}`/
    `pub use path::name;` statements. `crate::symbol` reaches that module
    through the re-export without ever spelling `crate::module::symbol`,
    which `CRATE_PATH`'s bare name capture cannot trace back to a module
    on its own."""
    masked = mask(lib_text)
    symbol_module = {}
    for match in PUB_USE_GROUP.finditer(masked):
        owner = match.group(1).split("::", 1)[0]
        if owner not in modules:
            continue
        for _, item in split_top_level(match.group(2)):
            name = re.match(r"\s*[A-Za-z_]\w*", item)
            if name:
                symbol_module[name.group(0).strip()] = owner
    for match in PUB_USE_SINGLE.finditer(masked):
        owner = match.group(1).split("::", 1)[0]
        if owner in modules:
            symbol_module[match.group(2)] = owner
    return symbol_module


def file_edges(relative_path, text, modules, symbol_module):
    source_module = owning_module(relative_path, modules)
    if source_module is None:
        return
    cleaned = strip_inline_test_blocks(mask(text))
    seen = {}
    for offset, name in group_members(cleaned):
        target = name if name in modules else symbol_module.get(name)
        if target is not None and target != source_module and target not in seen:
            seen[target] = offset
    for match in CRATE_PATH.finditer(cleaned):
        name = match.group(1)
        target = name if name in modules else symbol_module.get(name)
        if target is not None and target != source_module and target not in seen:
            seen[target] = match.start()
    for target, offset in seen.items():
        line = cleaned.count("\n", 0, offset) + 1
        yield source_module, target, relative_path, line


def read_crate_files(crate_root):
    """Return (src_files, lib_text) for one crate's `src/` tree."""
    src = crate_root / "src"
    files = {}
    for path in sorted(src.rglob("*.rs")):
        relative = path.relative_to(src).as_posix()
        files[relative] = path.read_text(encoding="utf-8")
    return files, (src / "lib.rs").read_text(encoding="utf-8")


def build_graph(src_files, lib_text):
    """Return (edges, modules). edges: dict[(src, dst)] -> (file, line) first seen."""
    modules = top_level_modules(lib_text)
    symbol_module = reexport_symbol_modules(lib_text, modules)
    excluded = cfg_test_external_files(src_files)
    edges = {}
    for relative_path, text in sorted(src_files.items()):
        if is_test_path(relative_path) or relative_path in excluded:
            continue
        for source, target, path, line in file_edges(relative_path, text, modules, symbol_module):
            edges.setdefault((source, target), (path, line))
    return edges, modules


def load_rules(path):
    if not path.exists():
        return set()
    rules = set()
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        source, separator, target = line.partition("->")
        if not separator:
            raise ValueError(f"{path.name}:{lineno}: expected 'source -> target'")
        rules.add((source.strip(), target.strip()))
    return rules


def write_rules(path, edges):
    body = "".join(f"{source} -> {target}\n" for source, target in sorted(edges))
    path.write_text(body, encoding="utf-8", newline="\n")


def find_cycle(rules):
    """Return an offending cycle (list of module names, first repeated at the
    end) in the rules file's graph, or None if it is acyclic."""
    graph = {}
    for source, target in rules:
        graph.setdefault(source, set()).add(target)
    unvisited, in_progress, done = 0, 1, 2
    state = {}
    stack = []

    def visit(node):
        state[node] = in_progress
        stack.append(node)
        for neighbour in sorted(graph.get(node, ())):
            status = state.get(neighbour, unvisited)
            if status == unvisited:
                cycle = visit(neighbour)
                if cycle is not None:
                    return cycle
            elif status == in_progress:
                start = stack.index(neighbour)
                return stack[start:] + [neighbour]
        state[node] = done
        stack.pop()
        return None

    for node in sorted(graph):
        if state.get(node, unvisited) == unvisited:
            cycle = visit(node)
            if cycle is not None:
                return cycle
    return None


def _display(path):
    try:
        return path.relative_to(ROOT).as_posix()
    except ValueError:
        return str(path)


def check(rules_path, src_files, lib_text):
    edges, _modules = build_graph(src_files, lib_text)
    rules = load_rules(rules_path)
    rules_display = _display(rules_path)
    errors = []
    for (source, target), (path, line) in sorted(edges.items()):
        if (source, target) not in rules:
            errors.append(
                f"{path}:{line}: new module edge {source} -> {target} is not in "
                f"{rules_display}; respect ownership in "
                "docs/architecture/source-map.md, or add the edge to the rules file "
                "deliberately in the same change as the architecture doc update"
            )
    cycle = find_cycle(rules)
    if cycle is not None:
        errors.append(
            f"{rules_path.name}: cyclic module rule "
            + " -> ".join(cycle)
            + "; break the cycle (it makes a future crate split impossible) before "
            "adding or keeping these edges"
        )
    for source, target in sorted(rules):
        if (source, target) not in edges:
            errors.append(
                f"{rules_display}: {source} -> {target} is no longer used by "
                "any non-test source file; remove the stale rule"
            )
    return errors, edges


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--update", action="store_true", help="rewrite the rules file to match the current graph"
    )
    parser.add_argument("--crate", choices=sorted(CRATE_ROOTS), help="only check this crate")
    args = parser.parse_args()
    crates = {args.crate: CRATE_ROOTS[args.crate]} if args.crate else CRATE_ROOTS

    if args.update:
        RULES_DIR.mkdir(parents=True, exist_ok=True)
        for name, crate_root in sorted(crates.items()):
            src_files, lib_text = read_crate_files(crate_root)
            edges, _modules = build_graph(src_files, lib_text)
            write_rules(RULES_DIR / f"{name}.txt", edges.keys())
            print(f"{name}: recorded {len(edges)} module edges")
        return 0

    total_errors = []
    total_edges = 0
    for name, crate_root in sorted(crates.items()):
        src_files, lib_text = read_crate_files(crate_root)
        errors, edges = check(RULES_DIR / f"{name}.txt", src_files, lib_text)
        total_edges += len(edges)
        total_errors.extend(f"{name}: {error}" for error in errors)
    for error in total_errors:
        print(error, file=sys.stderr)
    print(
        f"Checked {total_edges} module edges across {len(crates)} crates; "
        f"{len(total_errors)} errors"
    )
    return bool(total_errors)


if __name__ == "__main__":
    sys.exit(main())
