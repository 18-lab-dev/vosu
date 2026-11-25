[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ConfigPath
)
# Runs one filtered `cargo test` natively in the VM for scripts/vm-test.py,
# the ad-hoc "run native tests before handoff" lever. Invoked detached
# through schtasks (not interactive SSH), so completion is signaled by
# writing done.json rather than by the caller's connection staying open.
#
# Parameters come from $ConfigPath, a JSON file next to this script in the
# staging directory, not from schtasks /TR: schtasks caps /TR at 261
# characters, far too short for a full cargo argv (test filters can be
# long or contain spaces), so scripts/vm-test.py writes them to
# invocation.json instead and schedules only this short, fixed command.
#
# Shares env setup (toolchain, Cargo, debuginfo) with scripts/native-ci.ps1
# via windows-ci-env.ps1, so results match the prepush gate's native phase.
# Concurrency: each invocation gets a unique WorkRoot and CARGO_TARGET_DIR
# (see Set-VosuCargoWorkEnv); CARGO_HOME is shared and read-mostly, which
# Cargo's own package-cache locking makes safe to share across concurrent
# processes. Jobs (default 8 of the VM's 16 vCPUs) caps CPU use so this can
# run alongside a prepush native phase without a pause protocol.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# Fallback WorkRoot for error reporting before the config is even readable:
# scripts/vm-test.py always places invocation.json directly in WorkRoot.
$WorkRoot = Split-Path -Parent $ConfigPath
$donePath = Join-Path $WorkRoot 'done.json'

function Write-Done([int]$ExitCode, [bool]$TimedOut, [string]$ErrorMessage) {
    $result = [ordered]@{
        exitCode = $ExitCode
        timedOut = $TimedOut
        error    = $ErrorMessage
    }
    $result | ConvertTo-Json -Depth 3 | Set-Content $donePath -Encoding UTF8
}

try {
    $config = Get-Content $ConfigPath -Raw | ConvertFrom-Json
    $WorkRoot = $config.WorkRoot
    $donePath = Join-Path $WorkRoot 'done.json'
    $BundlePath = $config.BundlePath
    $PatchPath = $config.PatchPath
    $ExpectedCommit = $config.ExpectedCommit
    $cargoArgs = @($config.CargoArgs)
    $Jobs = [int]$config.Jobs
    $TimeoutSeconds = [int]$config.TimeoutSeconds

    $logs = Join-Path $WorkRoot 'logs'
    New-Item -ItemType Directory -Force -Path $logs | Out-Null

    $source = Join-Path $WorkRoot 'source'
    $git = 'C:\Program Files\Git\cmd\git.exe'
    # core.autocrlf=false on the clone, not just the source repo: a staged
    # bundle+patch must land byte-identical to the agent's working tree, and
    # CRLF rewriting on checkout would make the patch (built with the same
    # setting on Linux) fail to apply or silently reintroduce line-ending
    # churn the agent did not make.
    & $git -c core.autocrlf=false clone --quiet --no-checkout $BundlePath $source
    if ($LASTEXITCODE -ne 0) { throw 'git clone from bundle failed.' }
    & $git -C $source config core.autocrlf false
    & $git -C $source checkout --quiet --detach $ExpectedCommit
    if ($LASTEXITCODE -ne 0) { throw 'git checkout of expected commit failed.' }
    $actual = (& $git -C $source rev-parse HEAD | Out-String).Trim()
    if ($actual -ne $ExpectedCommit) { throw "Checked-out commit $actual does not match expected $ExpectedCommit." }

    if ($PatchPath -and (Test-Path $PatchPath) -and ((Get-Item $PatchPath).Length -gt 0)) {
        & $git -C $source apply --whitespace=nowarn $PatchPath
        if ($LASTEXITCODE -ne 0) { throw 'applying the uncommitted-changes patch failed.' }
    }

    . (Join-Path $PSScriptRoot 'windows-ci-env.ps1')
    $cargo = Import-VosuNativeToolchain
    Set-VosuCargoWorkEnv -WorkRoot $WorkRoot -Jobs $Jobs | Out-Null

    $steps = New-Object 'System.Collections.Generic.List[object]'
    if ($config.PluginHostFixtures) {
        # The root plugin_host_native test reads fixture libraries built in
        # this guest (never Linux artifacts) and never runs Cargo itself.
        $fixturesExit = Invoke-VosuPluginHostFixtures -CargoExe $cargo -WorkingDirectory $source `
            -LogDir $logs -Steps $steps -TimeoutSeconds $TimeoutSeconds
        if ($fixturesExit -ne 0) {
            $tail = (Get-Content (Join-Path $logs 'plugin-host-fixtures.stderr.log') -Tail 40) -join "`n"
            throw "plugin-host fixture preparation failed (exit $fixturesExit):`n$tail"
        }
    }
    $exitCode = Invoke-VosuCargoStep -Name 'cargo-test' -CargoExe $cargo -WorkingDirectory $source `
        -Arguments $cargoArgs -LogDir $logs -Steps $steps -TimeoutSeconds $TimeoutSeconds

    $timedOut = $steps[$steps.Count - 1].timedOut
    if ($timedOut) {
        # A killed process's exit code is an OS artifact, not a test result;
        # report the conventional shell timeout code instead.
        $exitCode = 124
    }

    $summary = [ordered]@{
        commit      = $actual
        workRoot    = $WorkRoot
        cargoArgs   = $cargoArgs
        passed      = ($exitCode -eq 0)
        timedOut    = $timedOut
        finishedUtc = (Get-Date).ToUniversalTime().ToString('o')
        steps       = $steps
    }
    $summary | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $WorkRoot 'summary.json') -Encoding UTF8

    Write-Done -ExitCode $exitCode -TimedOut $timedOut -ErrorMessage $null
    exit $exitCode
} catch {
    Write-Done -ExitCode 1 -TimedOut $false -ErrorMessage $_.ToString()
    [Console]::Error.WriteLine($_.ToString())
    exit 1
}
