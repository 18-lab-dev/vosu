#!/usr/bin/env bash
set -Eeuo pipefail

readonly SSH_DEST='vosu-agent@127.0.0.1'
readonly SSH_PORT='2223'
readonly REMOTE_SOURCE='C:\Users\vosu-agent\vosu-autoresearch'
readonly REMOTE_HELPER='C:\Users\vosu-agent\vosu-autoresearch\tools\autoresearch\windows-optimization-bench.ps1'
readonly REMOTE_VENDOR='C:\vosu-autoresearch-vendor'
readonly REMOTE_VENDOR_MARKER='C:\vosu-autoresearch-vendor\.cargo-lock.sha256'
readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
readonly SSH_CMD=(
  ssh
  -T
  -p "$SSH_PORT"
  -o BatchMode=yes
  -o StrictHostKeyChecking=yes
  -o ConnectTimeout=10
  -o ConnectionAttempts=1
  "$SSH_DEST"
)

vendor_temp=

cleanup() {
  local status=$?
  trap - EXIT
  if [[ -n "$vendor_temp" ]] && ! rm -rf -- "$vendor_temp"; then
    printf 'windows-optimization-bench: failed to remove vendor temp directory: %s\n' \
      "$vendor_temp" >&2
    ((status == 0)) && status=1
  fi
  exit "$status"
}
trap cleanup EXIT

fail() {
  printf 'windows-optimization-bench: %s\n' "$*" >&2
  exit 1
}

remote_powershell() {
  local encoded
  encoded=$(
    printf '%s' "$1" |
      python3 -c 'import base64, sys; print(base64.b64encode(sys.stdin.read().encode("utf-16le")).decode("ascii"))'
  )
  "${SSH_CMD[@]}" \
    "powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $encoded"
}

report_error() {
  local status=$?
  local line=$1
  trap - ERR
  printf 'windows-optimization-bench: failed at line %s (exit %s)\n' "$line" "$status" >&2
  exit "$status"
}
trap 'report_error "$LINENO"' ERR

for required_command in cargo python3 sha256sum ssh tar; do
  command -v "$required_command" >/dev/null 2>&1 ||
    fail "required host command not found: $required_command"
done

[[ -d "$REPO_ROOT" ]] || fail "component checkout not found: $REPO_ROOT"
[[ -f "$REPO_ROOT/tools/autoresearch/windows-optimization-bench.ps1" ]] ||
  fail 'missing tools/autoresearch/windows-optimization-bench.ps1'
[[ -f "$REPO_ROOT/Cargo.lock" ]] || fail 'missing Cargo.lock'


# Refuse to mutate the dedicated workspace unless this is the pinned VM endpoint.
"${SSH_CMD[@]}" cmd.exe /D /C exit 0 </dev/null >/dev/null

cargo_lock_sha256=$(sha256sum "$REPO_ROOT/Cargo.lock")
cargo_lock_sha256=${cargo_lock_sha256%%[[:space:]]*}
[[ "$cargo_lock_sha256" =~ ^[[:xdigit:]]{64}$ ]] ||
  fail 'sha256sum returned an invalid Cargo.lock digest'
cargo_lock_sha256=${cargo_lock_sha256,,}

# The guest dependency cache is persistent and valid only for the exact lockfile.
# Read it before replacing the source workspace so unchanged runs do no vendoring
# or dependency transfer.
marker_read_script="\$ErrorActionPreference = 'Stop'
\$marker = '$REMOTE_VENDOR_MARKER'
if (Test-Path -LiteralPath \$marker -PathType Leaf) {
    [Console]::Out.Write([System.IO.File]::ReadAllText(\$marker))
}"
guest_vendor_marker=$(remote_powershell "$marker_read_script" </dev/null)

if [[ "$guest_vendor_marker" == "$cargo_lock_sha256" ]]; then
  printf 'windows-optimization-bench: reusing guest Cargo vendor cache for %s\n' \
    "$cargo_lock_sha256" >&2
else
  printf 'windows-optimization-bench: provisioning guest Cargo vendor cache for %s\n' \
    "$cargo_lock_sha256" >&2
  vendor_temp=$(mktemp -d "${TMPDIR:-/tmp}/vosu-autoresearch-vendor.XXXXXXXX")

  # cargo prints a source-replacement config snippet on stdout; suppress it so
  # benchmark stdout remains exclusively owned by the guest helper.
  (
    cd -- "$REPO_ROOT"
    cargo vendor --quiet --locked --offline --versioned-dirs "$vendor_temp"
  ) >/dev/null

  vendor_prepare_script="\$ErrorActionPreference = 'Stop'
\$vendor = '$REMOTE_VENDOR'
if (Test-Path -LiteralPath \$vendor) {
    Remove-Item -LiteralPath \$vendor -Recurse -Force -ErrorAction Stop
}
New-Item -ItemType Directory -Path \$vendor -Force -ErrorAction Stop | Out-Null
if (-not (Test-Path -LiteralPath \$vendor -PathType Container)) {
    throw 'Cargo vendor directory creation failed'
}"
  remote_powershell "$vendor_prepare_script" </dev/null >/dev/null

  tar --create --file=- -C "$vendor_temp" . |
    "${SSH_CMD[@]}" tar.exe -xf - -C "$REMOTE_VENDOR"

  # Publish the marker only after the complete archive arrived. Write without a
  # BOM or newline, replace it atomically, and verify its exact bytes.
  marker_script="\$ErrorActionPreference = 'Stop'
\$marker = '$REMOTE_VENDOR_MARKER'
\$temporaryMarker = Join-Path (Split-Path -Parent \$marker) ('.cargo-lock.sha256.tmp-' + [Guid]::NewGuid().ToString('N'))
try {
    [System.IO.File]::WriteAllText(\$temporaryMarker, '$cargo_lock_sha256', (New-Object System.Text.UTF8Encoding(\$false)))
    Move-Item -LiteralPath \$temporaryMarker -Destination \$marker -Force -ErrorAction Stop
    if ([System.IO.File]::ReadAllText(\$marker) -cne '$cargo_lock_sha256') {
        throw 'Cargo vendor marker verification failed'
    }
} finally {
    if (Test-Path -LiteralPath \$temporaryMarker) {
        Remove-Item -LiteralPath \$temporaryMarker -Force -ErrorAction SilentlyContinue
    }
}"
  remote_powershell "$marker_script" </dev/null >/dev/null
fi

# Replace only the contracted source workspace. Persistent build and isolated run
# directories are owned by the guest helper and must survive this transfer step.
"${SSH_CMD[@]}" powershell.exe -NoLogo -NoProfile -NonInteractive \
  -ExecutionPolicy Bypass -Command - <<'POWERSHELL' >/dev/null
$ErrorActionPreference = 'Stop'
$source = 'C:\Users\vosu-agent\vosu-autoresearch'
if (Test-Path -LiteralPath $source) {
    Remove-Item -LiteralPath $source -Recurse -Force -ErrorAction Stop
}
New-Item -ItemType Directory -Path $source -Force -ErrorAction Stop | Out-Null
POWERSHELL

# Stream the live checkout, including uncommitted source changes, without VCS
# metadata, worktrees, build/session state, or generated compiler artifacts.
tar --create --file=- \
  --exclude='.git' \
  --exclude='.worktrees' \
  --exclude='worktrees' \
  --exclude='target' \
  --exclude='.autoresearch' \
  --exclude='autoresearch.md' \
  --exclude='autoresearch.checks.sh' \
  --exclude='autoresearch.program.md' \
  --exclude='autoresearch.ideas.md' \
  --exclude='autoresearch.jsonl' \
  --exclude='autoresearch.config.json' \
  --exclude='__pycache__' \
  --exclude='*.pyc' \
  --exclude='*.pyo' \
  --exclude='*.rmeta' \
  --exclude='*.rlib' \
  --exclude='*.o' \
  --exclude='*.obj' \
  --exclude='*.pdb' \
  --exclude='*.ilk' \
  -C "$REPO_ROOT" . |
  "${SSH_CMD[@]}" tar.exe -xf - -C "$REMOTE_SOURCE"

# An EXIT trap cannot run after a successful exec, so release the temporary
# vendor tree now while retaining the trap for every earlier failure path.
if [[ -n "$vendor_temp" ]]; then
  rm -rf -- "$vendor_temp"
  vendor_temp=
fi

# Preserve the helper's stdout byte-for-byte so its METRIC lines reach the root
# entrypoint unchanged. SSH and PowerShell failures propagate through exec.
exec "${SSH_CMD[@]}" powershell.exe -NoLogo -NoProfile -NonInteractive \
  -ExecutionPolicy Bypass -File "$REMOTE_HELPER"
