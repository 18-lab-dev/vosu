#!/usr/bin/env python3
"""Stage first-party tools beside a release executable (Python 3.11+)."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import shutil
import tempfile
import tomllib


def stage_tools(workspace: Path, target_dir: Path, profile: str, target: str | None, out: Path) -> Path:
    binaries = target_dir / target / profile if target else target_dir / profile
    suffix = '.exe' if (target and 'windows' in target) or (not target and os.name == 'nt') else ''
    tools = out / 'tools'
    out.mkdir(parents=True, exist_ok=True)
    # Validate and populate before replacing the previous stage: missing binaries
    # must not silently turn a release into a partial tool bundle.
    with tempfile.TemporaryDirectory(prefix='.tools-', dir=out) as temporary:
        staged = Path(temporary) / 'tools'
        staged.mkdir()
        for manifest in sorted((workspace / 'crates').glob('vosu-tool-*/tool.toml')):
            tool_id = manifest.parent.name.removeprefix('vosu-tool-')
            data = tomllib.loads(manifest.read_text(encoding='utf-8'))
            command = f'bin/vosu-{tool_id}'
            if not re.fullmatch(r'[a-z0-9][a-z0-9-]{0,31}', tool_id) or data.get('id') != tool_id:
                raise ValueError(f'{manifest}: id must equal crate suffix {tool_id!r}')
            if data.get('service', {}).get('command') != [command]:
                raise ValueError(f'{manifest}: command must be [{command!r}]')
            binary = binaries / f'vosu-{tool_id}{suffix}'
            if not binary.is_file() or binary.is_symlink():
                raise ValueError(f'missing regular tool binary: {binary}')
            destination = staged / tool_id
            (destination / 'bin').mkdir(parents=True)
            for name in ('tool.toml', 'settings.json', 'web'):
                source = manifest.parent / name
                if not source.exists():
                    continue
                if source.is_symlink() or (source.is_dir() and any(p.is_symlink() for p in source.rglob('*'))):
                    raise ValueError(f'tool assets must not be symlinks: {source}')
                if source.is_dir():
                    shutil.copytree(source, destination / name)
                else:
                    shutil.copy2(source, destination / name)
            shutil.copy2(binary, destination / 'bin' / binary.name)
            (destination / 'bin' / binary.name).chmod(0o755)
        if tools.is_symlink():
            raise ValueError(f'staged tools root must not be a symlink: {tools}')
        if tools.exists():
            shutil.rmtree(tools)
        staged.rename(tools)
    return tools


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--workspace', type=Path, default=Path('.'))
    parser.add_argument('--target-dir', type=Path, default=Path('target'))
    parser.add_argument('--profile', default='release')
    parser.add_argument('--target', help='Cargo target triple; omit for a native build')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    print(stage_tools(args.workspace, args.target_dir, args.profile, args.target, args.out))


if __name__ == '__main__':
    main()
