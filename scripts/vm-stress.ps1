[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ConfigPath
)
# Runs one filtered test suite natively in the Windows VM N times in a row
# off a single build, for scripts/vm-stress.py (the "hunt a native flake"
# lever). Invoked detached through schtasks (not interactive SSH), so
# completion is signaled by writing done.json rather than by the caller's
# connection staying open.
#
# Parameters come from $ConfigPath, a JSON file next to this script in the
# staging directory (see scripts/vm-test.ps1's own doc comment for why: the
# schtasks /TR 261-character cap).
#
# Shares env setup (toolchain, Cargo, debuginfo) with scripts/native-ci.ps1
# via windows-ci-env.ps1, so results match the prepush gate's native phase.
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$WorkRoot = Split-Path -Parent $ConfigPath
$donePath = Join-Path $WorkRoot 'done.json'

function Write-Done([int]$ExitCode, [string]$ErrorMessage) {
    $result = [ordered]@{
        exitCode = $ExitCode
        error    = $ErrorMessage
    }
    $result | ConvertTo-Json -Depth 3 | Set-Content $donePath -Encoding UTF8
}

function Write-Progress-Json([int]$Completed, [int]$Total, [int]$FailingSoFar) {
    $progressPath = Join-Path $WorkRoot 'progress.json'
    $result = [ordered]@{
        completed    = $Completed
        total        = $Total
        failingSoFar = $FailingSoFar
    }
    $result | ConvertTo-Json -Depth 3 | Set-Content $progressPath -Encoding UTF8
}

try {
    $config = Get-Content $ConfigPath -Raw | ConvertFrom-Json
    $WorkRoot = $config.WorkRoot
    $donePath = Join-Path $WorkRoot 'done.json'
    $BundlePath = $config.BundlePath
    $PatchPath = $config.PatchPath
    $ExpectedCommit = $config.ExpectedCommit
    $cargoArgsBuild = @($config.CargoArgsBuild)
    $testArgs = @($config.TestArgs)
    $Iterations = [int]$config.Iterations
    $Jobs = [int]$config.Jobs
    $BuildTimeoutSeconds = [int]$config.BuildTimeoutSeconds
    $IterTimeoutSeconds = [int]$config.IterTimeoutSeconds

    $logs = Join-Path $WorkRoot 'logs'
    New-Item -ItemType Directory -Force -Path $logs | Out-Null
    Write-Progress-Json -Completed 0 -Total $Iterations -FailingSoFar 0

    $source = Join-Path $WorkRoot 'source'
    $git = 'C:\Program Files\Git\cmd\git.exe'
    # core.autocrlf=false: see scripts/vm-test.ps1's own comment -- a staged
    # bundle+patch must land byte-identical to the agent's working tree.
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

    # Discover the executable paths from this one build. Calling Cargo again
    # is not reuse: build scripts may invalidate their artifacts on every run.
    $buildExit = Invoke-VosuCargoStep -Name 'build' -CargoExe $cargo -WorkingDirectory $source `
        -Arguments ($cargoArgsBuild + @('--message-format=json')) -LogDir $logs -Steps $steps -TimeoutSeconds $BuildTimeoutSeconds
    if ($buildExit -ne 0) {
        $summary = [ordered]@{
            commit   = $actual
            workRoot = $WorkRoot
            steps    = $steps
        }
        $summary | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $WorkRoot 'summary.json') -Encoding UTF8
        Write-Done -ExitCode $buildExit -ErrorMessage 'build (cargo test --no-run) failed; see logs/build.std{out,err}.log'
        exit $buildExit
    }
    $binaries = @(Get-Content (Join-Path $logs 'build.stdout.log') | ForEach-Object {
        $artifact = $_ | ConvertFrom-Json
        if ($artifact.reason -eq 'compiler-artifact' -and $artifact.profile.test -and $artifact.executable) {
            $artifact
        }
    })
    if ($binaries.Count -eq 0) { throw 'cargo test --no-run produced no test executables' }
    $env:PATH = (Join-Path $env:CARGO_TARGET_DIR 'debug\deps') + ';' + (Join-Path $env:CARGO_TARGET_DIR 'debug') + ';' + $env:PATH

    $failurePattern = [regex]'(?m)^test (\S+) \.\.\. FAILED\r?$'
    $perTest = [ordered]@{}
    $crashedIterations = New-Object 'System.Collections.Generic.List[object]'
    $failingIterations = 0

    for ($i = 1; $i -le $Iterations; $i++) {
        $name = "iter-$i"
        $exitCode = 0
        $timedOut = $false
        $stdoutParts = New-Object 'System.Collections.Generic.List[string]'
        $stderrParts = New-Object 'System.Collections.Generic.List[string]'
        $binaryIndex = 0
        foreach ($binary in $binaries) {
            $binaryIndex++
            $binaryName = "$name-bin-$binaryIndex"
            $rc = Invoke-VosuCargoStep -Name $binaryName -CargoExe $binary.executable `
                -WorkingDirectory (Split-Path -Parent $binary.manifest_path) `
                -Arguments $testArgs -LogDir $logs -Steps $steps -TimeoutSeconds $IterTimeoutSeconds
            if ($rc -ne 0) { $exitCode = $rc }
            $timedOut = $timedOut -or $steps[$steps.Count - 1].timedOut
            $stdoutParts.Add([IO.File]::ReadAllText((Join-Path $logs "$binaryName.stdout.log")))
            $stderrParts.Add([IO.File]::ReadAllText((Join-Path $logs "$binaryName.stderr.log")))
        }
        [IO.File]::WriteAllText((Join-Path $logs "$name.stdout.log"), ($stdoutParts -join "`n"))
        [IO.File]::WriteAllText((Join-Path $logs "$name.stderr.log"), ($stderrParts -join "`n"))

        $stdout = Get-Content (Join-Path $logs "$name.stdout.log") -Raw -ErrorAction SilentlyContinue
        if (-not $stdout) { $stdout = '' }
        $failMatches = $failurePattern.Matches($stdout)
        $iterationFailed = $false
        foreach ($m in $failMatches) {
            $iterationFailed = $true
            $testName = $m.Groups[1].Value
            if (-not $perTest.Contains($testName)) {
                $perTest[$testName] = New-Object 'System.Collections.Generic.List[int]'
            }
            [void]$perTest[$testName].Add($i)
        }
        if ($exitCode -ne 0 -and -not $iterationFailed) {
            # Nonzero exit with no parsed "... FAILED" line: the harness
            # itself crashed (panic-abort, access violation, timeout kill)
            # rather than reporting an ordinary test failure.
            [void]$crashedIterations.Add([ordered]@{
                iteration = $i
                exitCode  = $exitCode
                timedOut  = $timedOut
                stdoutLog = (Join-Path $logs "$name.stdout.log")
                stderrLog = (Join-Path $logs "$name.stderr.log")
            })
            $iterationFailed = $true
        }
        if ($iterationFailed) { $failingIterations++ }
        Write-Progress-Json -Completed $i -Total $Iterations -FailingSoFar $failingIterations
    }

    $perTestOut = [ordered]@{}
    foreach ($key in $perTest.Keys) {
        $perTestOut[$key] = [ordered]@{
            failureCount = $perTest[$key].Count
            iterations   = $perTest[$key].ToArray()
        }
    }

    $results = [ordered]@{
        commit             = $actual
        iterations         = $Iterations
        failingIterations  = $failingIterations
        perTest            = $perTestOut
        crashedIterations  = $crashedIterations.ToArray()
    }
    $results | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $WorkRoot 'results.json') -Encoding UTF8

    $summary = [ordered]@{
        commit      = $actual
        workRoot    = $WorkRoot
        testArgs    = $testArgs
        iterations  = $Iterations
        finishedUtc = (Get-Date).ToUniversalTime().ToString('o')
        steps       = $steps
    }
    $summary | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $WorkRoot 'summary.json') -Encoding UTF8

    $finalExit = if ($failingIterations -gt 0) { 1 } else { 0 }
    Write-Done -ExitCode $finalExit -ErrorMessage $null
    exit $finalExit
} catch {
    $detail = "$($_.ToString()) -- $($_.InvocationInfo.PositionMessage)"
    Write-Done -ExitCode 1 -ErrorMessage $detail
    [Console]::Error.WriteLine($detail)
    exit 1
}
