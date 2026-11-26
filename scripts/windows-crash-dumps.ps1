[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateSet('Enable', 'Disable')][string]$Action,
    [string]$DumpFolder,
    [int]$DumpCount = 10,
    [int]$DumpType = 1
)
# Enables or disables Windows Error Reporting's machine-wide LocalDumps key
# around a native test run, so an access violation or other unhandled crash
# in a `cargo nextest`/`cargo test` test binary leaves a minidump on disk
# instead of only an exit code. The global key is used rather than a
# per-executable one because test binaries get hashed names
# (`plugin_host_native-<hash>.exe`) that change every build.
#
# `-Action Enable` is idempotent (safe to call again without a prior
# Disable); `-Action Disable` removes the whole key, so nothing leaks into a
# shared machine's default crash behavior once the caller is done. Used by
# ci.yml's windows-native job (hosted runner, disposed after the job either
# way) and scripts/native-ci.ps1 (the persistent VM, where leaving this set
# would be a real, lasting change — always paired with Disable in a
# try/finally there).
#
# DumpType 1 is a minidump (stacks, handles, loaded-module list): enough to
# symbolize a crash with the matching .exe/.pdb, and small. DumpType 2 (full
# heap memory) is available for a case that needs it, but is not the
# default: it can be large per crash and DumpCount keeps only the last few.
$ErrorActionPreference = 'Stop'
$key = 'HKLM:\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps'

if ($Action -eq 'Enable') {
    if (-not $DumpFolder) { throw '-DumpFolder is required for -Action Enable.' }
    New-Item -ItemType Directory -Force -Path $DumpFolder | Out-Null
    New-Item -Path $key -Force | Out-Null
    New-ItemProperty -Path $key -Name 'DumpFolder' -PropertyType ExpandString -Value $DumpFolder -Force | Out-Null
    New-ItemProperty -Path $key -Name 'DumpType' -PropertyType DWord -Value $DumpType -Force | Out-Null
    New-ItemProperty -Path $key -Name 'DumpCount' -PropertyType DWord -Value $DumpCount -Force | Out-Null
    Write-Output "WER LocalDumps enabled: $DumpFolder (DumpType=$DumpType, DumpCount=$DumpCount)"
} else {
    Remove-Item -Path $key -Recurse -Force -ErrorAction SilentlyContinue
    Write-Output 'WER LocalDumps key removed.'
}
