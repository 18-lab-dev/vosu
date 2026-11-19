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
#   cargo test --workspace --lib --bins --tests --examples --all-features --locked --target x86_64-pc-windows-msvc
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

$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
$devEnvironment = & cmd.exe /d /c ('call "' + $vcvars + '" >nul && set')
if ($LASTEXITCODE -ne 0) { throw 'vcvars64 import failed.' }
foreach ($line in $devEnvironment) {
    if ($line -match '^([^=][^=]*)=(.*)$') {
        Set-Item -Path ('Env:\' + $matches[1]) -Value $matches[2]
    }
}

$rust = 'C:\Program Files\Rust\1.96.1\bin'
$env:PATH = $rust + ';' + $env:PATH
Remove-Item Env:\C_INCLUDE_PATH -ErrorAction SilentlyContinue
$cargoTarget = Join-Path $WorkRoot 'target'
New-Item -ItemType Directory -Force -Path $cargoTarget | Out-Null
$env:CARGO_TARGET_DIR = $cargoTarget
$env:CARGO_HOME = 'V:\agent-desktop\.cargo'
$env:CARGO_INCREMENTAL = '0'
$tmp = Join-Path $WorkRoot 'tmp'
New-Item -ItemType Directory -Force -Path $tmp | Out-Null
$env:TEMP = $tmp
$env:TMP = $tmp

$steps = New-Object 'System.Collections.Generic.List[object]'
$cargo = Join-Path $rust 'cargo.exe'

function Invoke-Step([string]$Name, [string[]]$Arguments) {
    $prefix = Join-Path $logs $Name
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $cargo
    $info.Arguments = $Arguments -join ' '
    $info.WorkingDirectory = $source
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $info
    if (-not $process.Start()) { throw "Could not start step $Name" }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    [IO.File]::WriteAllText($prefix + '.stdout.log', $stdout.GetAwaiter().GetResult())
    [IO.File]::WriteAllText($prefix + '.stderr.log', $stderr.GetAwaiter().GetResult())
    $watch.Stop()
    $entry = [ordered]@{
        step = $Name
        exitCode = $process.ExitCode
        wallSeconds = $watch.Elapsed.TotalSeconds
        stdout = $prefix + '.stdout.log'
        stderr = $prefix + '.stderr.log'
    }
    [void]$steps.Add([pscustomobject]$entry)
    $process.Dispose()
    Write-Host ('{0}: exit {1}, {2:N2}s' -f $Name, $entry.exitCode, $entry.wallSeconds)
    return $entry.exitCode
}

$overallExit = 0

$overallExit = $overallExit -bor (Invoke-Step 'cargo-clippy' @(
        'clippy', '--workspace', '--all-targets', '--all-features', '--locked',
        '--target', 'x86_64-pc-windows-msvc', '--', '-D', 'warnings'))

$overallExit = $overallExit -bor (Invoke-Step 'cargo-test-workspace' @(
        'test', '--workspace', '--lib', '--bins', '--tests', '--examples',
        '--all-features', '--locked', '--target', 'x86_64-pc-windows-msvc'))

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
