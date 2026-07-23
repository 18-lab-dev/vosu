#!/usr/bin/env bash
set -euo pipefail

readonly SSH_DEST='vosu-agent@127.0.0.1'
readonly SSH_PORT='2223'
readonly REMOTE_SOURCE='C:\Users\vosu-agent\vosu-autoresearch'
readonly REMOTE_HELPER='C:\Users\vosu-agent\vosu-autoresearch\tools\autoresearch\windows-memory-bench.ps1'

readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SSH_CMD=(
  ssh
  -p "$SSH_PORT"
  -o BatchMode=yes
  -o ConnectTimeout=10
  -o ConnectionAttempts=1
  "$SSH_DEST"
)

# Confirm the pinned VM endpoint before changing its dedicated source workspace.
"${SSH_CMD[@]}" cmd.exe /D /C exit 0

# Replace only the autoresearch source workspace. The tiny benchmark executable
# is rebuilt from the current production helper on every run.
"${SSH_CMD[@]}" powershell.exe -NoLogo -NoProfile -NonInteractive \
  -ExecutionPolicy Bypass -Command - <<'POWERSHELL'
$ErrorActionPreference = 'Stop'
$source = 'C:\Users\vosu-agent\vosu-autoresearch'
Remove-Item -LiteralPath $source -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $source -Force -ErrorAction Stop | Out-Null
POWERSHELL

# Send a clean source snapshot. Do not copy build output, worktrees, benchmark
# session state, or compiler metadata; tools/autoresearch remains included.
tar --create --file=- \
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
  -C "$REPO_ROOT" . |
  "${SSH_CMD[@]}" tar.exe -xf - -C "$REMOTE_SOURCE"

# The helper owns the fixed Rust/MSVC setup, optimized rustc invocation, and
# exact METRIC output contract.
"${SSH_CMD[@]}" powershell.exe -NoLogo -NoProfile -NonInteractive \
  -ExecutionPolicy Bypass -File "$REMOTE_HELPER"
