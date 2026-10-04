#!/usr/bin/env bash
set -Eeuo pipefail

readonly REMOTE_SOURCE='V:\agent-workspaces\vosu-autoresearch'
readonly REMOTE_HELPER='V:\agent-workspaces\vosu-autoresearch\tools\autoresearch\windows-optimization-bench.ps1'
readonly REMOTE_VENDOR='V:\agent-workspaces\vosu-autoresearch-vendor'
readonly REMOTE_VENDOR_MARKER='V:\agent-workspaces\vosu-autoresearch-vendor\.cargo-lock.sha256'
readonly REMOTE_SOURCE_ARCHIVE="V:\\agent-workspaces\\vosu-source-$$.tar"
readonly REMOTE_VENDOR_ARCHIVE="V:\\agent-workspaces\\vosu-vendor-$$.tar"
readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"

source_archive=
vendor_archive=
source_archive_remote_owned=0
vendor_archive_remote_owned=0
vendor_temp=

remove_guest_archive() {
  local archive=$1
  windows-vm exec "\$archive = '$archive'; if (Test-Path -LiteralPath \$archive) { Remove-Item -LiteralPath \$archive -Force -ErrorAction Stop }" >/dev/null
}

cleanup() {
  local status=$?
  trap - EXIT ERR
  if ((source_archive_remote_owned)) && ! remove_guest_archive "$REMOTE_SOURCE_ARCHIVE"; then
    printf 'windows-optimization-bench: failed to remove guest source archive\n' >&2
    if ((status == 0)); then status=1; fi
  fi
  if ((vendor_archive_remote_owned)) && ! remove_guest_archive "$REMOTE_VENDOR_ARCHIVE"; then
    printf 'windows-optimization-bench: failed to remove guest vendor archive\n' >&2
    if ((status == 0)); then status=1; fi
  fi
  for archive in "$source_archive" "$vendor_archive"; do
    if [[ -n "$archive" ]] && ! rm -f -- "$archive"; then
      printf 'windows-optimization-bench: failed to remove local archive: %s\n' "$archive" >&2
      if ((status == 0)); then status=1; fi
    fi
  done
  if [[ -n "$vendor_temp" ]] && ! rm -rf -- "$vendor_temp"; then
    printf 'windows-optimization-bench: failed to remove vendor temp directory: %s\n' "$vendor_temp" >&2
    if ((status == 0)); then status=1; fi
  fi
  exit "$status"
}
trap cleanup EXIT



fail() {
  printf 'windows-optimization-bench: %s\n' "$*" >&2
  exit 1
}

remote_powershell() {
  windows-vm exec "$1"
}

report_error() {
  local status=$?
  local line=$1
  trap - ERR
  printf 'windows-optimization-bench: failed at line %s (exit %s)\n' "$line" "$status" >&2
  exit "$status"
}
trap 'report_error "$LINENO"' ERR

for required_command in cargo windows-vm sha256sum tar; do
  command -v "$required_command" >/dev/null 2>&1 ||
    fail "required host command not found: $required_command"
done
[[ -d "$REPO_ROOT" ]] || fail "component checkout not found: $REPO_ROOT"
[[ -f "$REPO_ROOT/tools/autoresearch/windows-optimization-bench.ps1" ]] ||
  fail 'missing tools/autoresearch/windows-optimization-bench.ps1'
[[ -f "$REPO_ROOT/Cargo.lock" ]] || fail 'missing Cargo.lock'


# Refuse to mutate the dedicated workspace unless this is the pinned VM endpoint.
windows-vm doctor </dev/null >/dev/null

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

  vendor_archive=$(mktemp "${TMPDIR:-/tmp}/vosu-autoresearch-vendor.XXXXXXXX.tar")
  tar --create --file="$vendor_archive" -C "$vendor_temp" .
  vendor_archive_remote_owned=1
  windows-vm put "$vendor_archive" "$REMOTE_VENDOR_ARCHIVE"
  remote_powershell "\$ErrorActionPreference = 'Stop'; tar.exe -xf '$REMOTE_VENDOR_ARCHIVE' -C '$REMOTE_VENDOR'; if (\$LASTEXITCODE -ne 0) { throw 'Cargo vendor archive extraction failed' }" </dev/null >/dev/null

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
remote_powershell "
\$source = '$REMOTE_SOURCE'
if (Test-Path -LiteralPath \$source) {
    Remove-Item -LiteralPath \$source -Recurse -Force -ErrorAction Stop
}
New-Item -ItemType Directory -Path \$source -Force -ErrorAction Stop | Out-Null" </dev/null >/dev/null

# Archive the live checkout locally, excluding VCS metadata, worktrees,
# build/session state, and generated compiler artifacts.
source_archive=$(mktemp "${TMPDIR:-/tmp}/vosu-autoresearch-source.XXXXXXXX.tar")
tar --create --file="$source_archive" \
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
  -C "$REPO_ROOT" .
source_archive_remote_owned=1
windows-vm put "$source_archive" "$REMOTE_SOURCE_ARCHIVE"
remote_powershell "\$ErrorActionPreference = 'Stop'; tar.exe -xf '$REMOTE_SOURCE_ARCHIVE' -C '$REMOTE_SOURCE'; if (\$LASTEXITCODE -ne 0) { throw 'source archive extraction failed' }" </dev/null >/dev/null

# Preserve the helper's stdout byte-for-byte so its METRIC lines reach the root
# entrypoint unchanged. Archive cleanup runs after the helper completes.
windows-vm exec "& powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File '$REMOTE_HELPER'"
