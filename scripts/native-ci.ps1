[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$BundlePath,
    [Parameter(Mandatory = $true)][string]$ExpectedCommit,
    [string]$WorkRoot,
    [string]$Repeat
)
# Runs the ci.yml `windows-native` job (plus the workspace doctest step, which
# the hosted job does not run but AGENTS.md requires locally) against an exact
# commit staged from one git bundle. Invoked by scripts/prepush.py inside the
# vosu-windows-pro VM. Never run against a dirty or unexpected checkout: the
# commit is verified before anything builds.
#
# Covered CI commands (keep in sync with ci.yml's windows-native job and with
# scripts/prepush.py / scripts/test_gate_tooling.py, which checks this list):
#   cargo clippy --workspace --all-targets --all-features --locked --target x86_64-pc-windows-msvc -- -D warnings
#   python scripts/plugin-host-fixtures.py --target x86_64-pc-windows-msvc --env-file <env-file>
#   cargo nextest run --workspace --lib --bins --tests --examples --all-features --locked --target x86_64-pc-windows-msvc
#   cargo test --workspace --doc --all-features --locked --target x86_64-pc-windows-msvc
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

if (-not $WorkRoot) {
    $WorkRoot = "V:\bt\vosu-$($ExpectedCommit.Substring(0, 12))"
}
if (Test-Path $WorkRoot) {
    throw "Work root already exists, refusing to reuse: $WorkRoot"
}
New-Item -ItemType Directory -Force -Path $WorkRoot | Out-Null
$source = Join-Path $WorkRoot 'source'
$logs = Join-Path $WorkRoot 'logs'
New-Item -ItemType Directory -Force -Path $logs | Out-Null

$git = 'C:\Program Files\Git\cmd\git.exe'
& $git clone --quiet --no-checkout $BundlePath $source
if ($LASTEXITCODE -ne 0) { throw 'git clone from bundle failed.' }
& $git -C $source checkout --quiet --detach $ExpectedCommit
if ($LASTEXITCODE -ne 0) { throw 'git checkout of expected commit failed.' }
$actual = (& $git -C $source rev-parse HEAD | Out-String).Trim()
if ($actual -ne $ExpectedCommit) { throw "Checked-out commit $actual does not match expected $ExpectedCommit." }

. (Join-Path $PSScriptRoot 'windows-ci-env.ps1')
$cargo = Import-VosuNativeToolchain
$cargoTarget = Set-VosuCargoWorkEnv -WorkRoot $WorkRoot

$steps = New-Object 'System.Collections.Generic.List[object]'

function Invoke-Step([string]$Name, [string[]]$Arguments) {
    return Invoke-VosuCargoStep -Name $Name -CargoExe $cargo -WorkingDirectory $source `
        -Arguments $Arguments -LogDir $logs -Steps $steps
}

$overallExit = 0

$overallExit = $overallExit -bor (Invoke-Step 'cargo-clippy' @(
        'clippy', '--workspace', '--all-targets', '--all-features', '--locked',
        '--target', 'x86_64-pc-windows-msvc', '--', '-D', 'warnings'))

# The root plugin-host native test reads the fixture libraries this step
# builds in this guest; it never runs Cargo itself. Without them the test
# step does not run, so prepush.py finds no 'cargo-test-workspace' result.
$fixturesExit = Invoke-VosuPluginHostFixtures -CargoExe $cargo -WorkingDirectory $source `
    -LogDir $logs -Steps $steps -Target 'x86_64-pc-windows-msvc' -ScratchRoot 'V:\agent-desktop\tmp'
$overallExit = $overallExit -bor $fixturesExit
$werDumpFolder = Join-Path $WorkRoot 'wer-dumps'
if ($fixturesExit -eq 0) {
    & (Join-Path $PSScriptRoot 'windows-crash-dumps.ps1') -Action Enable -DumpFolder $werDumpFolder
    try {
        $overallExit = $overallExit -bor (Invoke-Step 'cargo-test-workspace' @(
                'nextest', 'run', '--workspace', '--lib', '--bins', '--tests', '--examples',
                '--all-features', '--locked', '--target', 'x86_64-pc-windows-msvc'))
    } finally {
        & (Join-Path $PSScriptRoot 'windows-crash-dumps.ps1') -Action Disable
    }
    $dumps = Get-ChildItem $werDumpFolder -Filter '*.dmp' -ErrorAction SilentlyContinue
    if ($dumps) {
        Write-Output "Native test crash dumps captured: $werDumpFolder ($($dumps.Count) file(s))"
    }
}

$overallExit = $overallExit -bor (Invoke-Step 'cargo-test-doc' @(
        'test', '--workspace', '--doc', '--all-features', '--locked',
        '--target', 'x86_64-pc-windows-msvc'))

if ($Repeat) {
    $parts = $Repeat.Split(':', 2)
    if ($parts.Count -ne 2) { throw "-Repeat must be '<test-filter>:<n>'." }
    $filter = $parts[0]
    $count = [int]$parts[1]
    for ($i = 1; $i -le $count; $i++) {
        $overallExit = $overallExit -bor (Invoke-Step ("repeat-$filter-$i") @(
                'test', '--workspace', '--all-features', '--locked',
                '--target', 'x86_64-pc-windows-msvc', $filter))
    }
}
Remove-VosuPluginHostScratch

$summary = [ordered]@{
    commit = $actual
    workRoot = $WorkRoot
    cargoTargetDir = $cargoTarget
    finishedUtc = (Get-Date).ToUniversalTime().ToString('o')
    passed = ($overallExit -eq 0)
    steps = $steps
}
$summaryPath = Join-Path $WorkRoot 'summary.json'
$summary | ConvertTo-Json -Depth 6 | Set-Content $summaryPath -Encoding UTF8
Write-Output "Summary: $summaryPath"

if ($overallExit -ne 0) { exit 1 }
exit 0
