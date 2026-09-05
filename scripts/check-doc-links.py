"""Check local Markdown link paths; no network, renderer, or inference required."""

from pathlib import Path
import re
import sys
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parent.parent


def broken_links(files):
    for path in files:
        text = re.sub(r"```.*?```", lambda match: "\n" * match.group().count("\n"), path.read_text(encoding="utf-8"), flags=re.S)
        for match in re.finditer(r"!?\[[^\]\n]*\]\(([^\s)]+)(?:\s+[^)]*)?\)", text):
            target = match.group(1).strip("<>")
            url = urlsplit(target)
            if url.scheme or url.netloc or not url.path:
                continue
            destination = (path.parent / unquote(url.path)).resolve()
            if not destination.exists():
                line = text.count("\n", 0, match.start()) + 1
                yield f"{path.relative_to(ROOT)}:{line}: missing local link {target}"


def main():
    files = [ROOT / "README.md", ROOT / "AGENTS.md"]
    files.extend(sorted((ROOT / "openwiki").rglob("*.md")))
    errors = list(broken_links(files))
    for error in errors:
        print(error, file=sys.stderr)
    print(f"Checked local links in {len(files)} maintained Markdown files; {len(errors)} missing paths")
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
