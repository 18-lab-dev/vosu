#!/usr/bin/env bash
set -euo pipefail

readonly REMOTE_ARCHIVE="V:\\agent-workspaces\\vosu-memory-source-$$.tar"
local_archive=
remote_archive_owned=0

cleanup() {
  local status=$?
  trap - EXIT
  if ((remote_archive_owned)); then
    if ! windows-vm exec "\$archive = '$REMOTE_ARCHIVE'; if (Test-Path -LiteralPath \$archive) { Remove-Item -LiteralPath \$archive -Force -ErrorAction Stop }" >/dev/null; then
      printf 'windows-memory-bench: failed to remove guest archive: %s\n' "$REMOTE_ARCHIVE" >&2
      if ((status == 0)); then status=1; fi
    fi
  fi
  if [[ -n "$local_archive" ]] && ! rm -f -- "$local_archive"; then
    printf 'windows-memory-bench: failed to remove local archive: %s\n' "$local_archive" >&2
    if ((status == 0)); then status=1; fi
  fi
  exit "$status"
}
trap cleanup EXIT
readonly REMOTE_SOURCE='V:\agent-workspaces\vosu-autoresearch'
readonly REMOTE_HELPER='V:\agent-workspaces\vosu-autoresearch\tools\autoresearch\windows-memory-bench.ps1'

readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"

# Confirm the pinned VM endpoint before changing its dedicated source workspace.
windows-vm doctor >/dev/null

# Replace only the autoresearch source workspace. The tiny benchmark executable
# is rebuilt from the current production helper on every run.
windows-vm exec "
\$source = '$REMOTE_SOURCE'
Remove-Item -LiteralPath \$source -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path \$source -Force | Out-Null"

# Send a filtered local archive through the canonical guest transfer command.
local_archive=$(mktemp "${TMPDIR:-/tmp}/vosu-memory-source.XXXXXXXX.tar")
tar --create --file="$local_archive" \
  --exclude='.git' \
  --exclude='.worktrees' \
  --exclude='target' \
  --exclude='.autoresearch' \
  --exclude='autoresearch.md' \
  --exclude='autoresearch.checks.sh' \
  --exclude='autoresearch.program.md' \
  --exclude='autoresearch.ideas.md' \
  --exclude='autoresearch.jsonl' \
  --exclude='autoresearch.config.json' \
  --exclude='*.rmeta' \
  --exclude='scripts/__pycache__' \
  -C "$REPO_ROOT" .
remote_archive_owned=1
windows-vm put "$local_archive" "$REMOTE_ARCHIVE"
windows-vm exec "\$ErrorActionPreference = 'Stop'; tar.exe -xf '$REMOTE_ARCHIVE' -C '$REMOTE_SOURCE'; if (\$LASTEXITCODE -ne 0) { throw 'source archive extraction failed' }"

# The helper owns the fixed Rust/MSVC setup, optimized rustc invocation, and
# exact METRIC output contract.
windows-vm exec "& powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File '$REMOTE_HELPER'"
