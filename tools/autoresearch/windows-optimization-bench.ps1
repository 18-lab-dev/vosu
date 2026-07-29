#requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$ConfirmPreference = 'None'

$workspace = 'C:\Users\vosu-agent\vosu-autoresearch'
$cargo = 'C:\Rust\toolchain\bin\cargo.exe'
$rustc = 'C:\Rust\toolchain\bin\rustc.exe'
$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
$targetDirectory = 'C:\vosu-autoresearch-target'
$vendorDirectory = 'C:\vosu-autoresearch-vendor'
$runDirectory = 'C:\vosu-autoresearch-run'
$memoryOutputDirectory = 'C:\vosu-autoresearch-bin'
$target = 'x86_64-pc-windows-msvc'
$sampleCount = 31
$warmupSeconds = 5
$invariant = [Globalization.CultureInfo]::InvariantCulture
$daemon = $null
$failure = $null

Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class ProcessCycleCounter
{
    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern bool QueryProcessCycleTime(IntPtr processHandle, out ulong cycleTime);

    public static ulong Read(IntPtr processHandle)
    {
        ulong cycleTime;
        if (!QueryProcessCycleTime(processHandle, out cycleTime))
        {
            throw new Win32Exception(Marshal.GetLastWin32Error(), "QueryProcessCycleTime failed");
        }
        return cycleTime;
    }
}
'@

function Assert-Leaf([string]$Path, [string]$Description) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description does not exist: $Path"
    }
}

function Assert-Directory([string]$Path, [string]$Description) {
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "$Description does not exist: $Path"
    }
}

function Assert-PositiveFinite([double]$Value, [string]$Name) {
    if ($Value -le 0.0 -or [double]::IsNaN($Value) -or [double]::IsInfinity($Value)) {
        throw "$Name was not finite and positive: $Value"
    }
}

function Format-Metric([double]$Value) {
    return $Value.ToString('R', $invariant)
}

function Get-Median([double[]]$Values) {
    if ($Values.Count -ne $sampleCount) {
        throw "Expected $sampleCount samples, received $($Values.Count)"
    }
    $ordered = @($Values | Sort-Object)
    return [double]$ordered[[int]($ordered.Count / 2)]
}

function Test-DaemonReady {
    $request = $null
    $response = $null
    try {
        $request = [Net.HttpWebRequest]::Create('http://127.0.0.1:24050/json/v2')
        $request.Method = 'GET'
        $request.Proxy = $null
        $request.Timeout = 1000
        $request.ReadWriteTimeout = 1000
        $response = [Net.HttpWebResponse]$request.GetResponse()
        return ([int]$response.StatusCode -eq 200)
    }
    catch [Net.WebException] {
        if ($null -eq $_.Exception.Response) {
            return $false
        }
        $response = [Net.HttpWebResponse]$_.Exception.Response
        if ([int]$response.StatusCode -ne 500) {
            return $false
        }
        $reader = New-Object IO.StreamReader($response.GetResponseStream())
        try {
            $body = $reader.ReadToEnd()
        }
        finally {
            $reader.Dispose()
        }
        return ($body -eq '{"error":"osu is not ready/running"}')
    }
    finally {
        if ($null -ne $response) {
            $response.Dispose()
        }
    }
}

function Stop-Daemon {
    if ($null -eq $script:daemon) {
        return
    }
    $script:daemon.Refresh()
    if (-not $script:daemon.HasExited) {
        Stop-Process -Id $script:daemon.Id -Force -ErrorAction Stop
        if (-not $script:daemon.WaitForExit(10000)) {
            throw "vosu process $($script:daemon.Id) did not stop"
        }
    }
}

try {
    Assert-Directory $workspace 'Remote source workspace'
    Assert-Leaf (Join-Path $workspace 'Cargo.toml') 'Workspace manifest'
    Assert-Leaf (Join-Path $workspace 'Cargo.lock') 'Workspace lockfile'
    Assert-Leaf (Join-Path $workspace 'rust-toolchain.toml') 'Pinned toolchain manifest'
    Assert-Leaf $cargo 'Pinned Cargo'
    Assert-Leaf $rustc 'Pinned rustc'
    Assert-Leaf $vcvars 'Pinned MSVC environment script'

    $toolchainText = Get-Content -LiteralPath (Join-Path $workspace 'rust-toolchain.toml') -Raw
    $channelMatch = [regex]::Match($toolchainText, '(?m)^\s*channel\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"\s*$')
    if (-not $channelMatch.Success) {
        throw 'rust-toolchain.toml does not contain a pinned numeric channel'
    }
    $pinnedRustVersion = $channelMatch.Groups[1].Value

    $rustVersion = (& $rustc --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $rustVersion -notmatch ('^rustc ' + [regex]::Escape($pinnedRustVersion) + '(?:\s|$)')) {
        throw "Pinned rustc version mismatch: expected $pinnedRustVersion, got '$rustVersion'"
    }
    $cargoVersion = (& $cargo --version | Out-String).Trim()
    $pinnedRustParts = $pinnedRustVersion.Split('.')
    $expectedCargoPrefix = '^cargo ' + [regex]::Escape($pinnedRustParts[0] + '.' + $pinnedRustParts[1] + '.')
    if ($LASTEXITCODE -ne 0 -or $cargoVersion -notmatch $expectedCargoPrefix) {
        throw "Pinned Cargo version mismatch for Rust $pinnedRustVersion`: '$cargoVersion'"
    }

    $clCommand = 'call "' + $vcvars + '" >nul && where cl.exe'
    $clPaths = @(& cmd.exe /d /s /c $clCommand)
    if ($LASTEXITCODE -ne 0 -or $clPaths.Count -eq 0) {
        throw 'Pinned MSVC environment did not provide cl.exe'
    }
    $expectedClRoot = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\'
    if (@($clPaths | Where-Object { $_.Trim().StartsWith($expectedClRoot, [StringComparison]::OrdinalIgnoreCase) }).Count -eq 0) {
        throw "MSVC cl.exe was not supplied by the pinned BuildTools installation: $($clPaths -join ', ')"
    }

    $conflicts = @(Get-Process -ErrorAction Stop | Where-Object {
        $_.ProcessName -like 'osu*' -or $_.ProcessName -ieq 'vosu'
    })
    if ($conflicts.Count -ne 0) {
        $descriptions = @($conflicts | ForEach-Object { "$($_.ProcessName) (PID $($_.Id))" })
        throw "Benchmark requires no running osu* or vosu process; found: $($descriptions -join ', ')"
    }

    Assert-Directory $vendorDirectory 'Persistent Cargo vendor directory'
    $vendorMarker = Join-Path $vendorDirectory '.cargo-lock.sha256'
    Assert-Leaf $vendorMarker 'Persistent Cargo vendor lock marker'

    $workspaceLockHash = (Get-FileHash -LiteralPath (Join-Path $workspace 'Cargo.lock') -Algorithm SHA256).Hash.ToLowerInvariant()
    $vendorLockHash = [IO.File]::ReadAllText($vendorMarker).Trim()
    if ($vendorLockHash -notmatch '^[0-9a-f]{64}$') {
        throw "Persistent Cargo vendor lock marker is not a lowercase SHA-256: $vendorMarker"
    }
    if (-not [string]::Equals($workspaceLockHash, $vendorLockHash, [StringComparison]::Ordinal)) {
        throw "Persistent Cargo vendor lock marker mismatch: expected $workspaceLockHash, got $vendorLockHash"
    }

    $cargoConfigDirectory = Join-Path $workspace '.cargo'
    New-Item -ItemType Directory -Path $cargoConfigDirectory -Force | Out-Null
    $cargoConfig = @'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "C:/vosu-autoresearch-vendor"
'@
    [IO.File]::WriteAllText(
        (Join-Path $cargoConfigDirectory 'config.toml'),
        $cargoConfig,
        (New-Object Text.UTF8Encoding($false))
    )

    New-Item -ItemType Directory -Path $targetDirectory -Force | Out-Null
    $env:CARGO_TARGET_DIR = $targetDirectory
    $env:RUSTC = $rustc
    $buildCommand = 'call "' + $vcvars + '" >nul && cd /d "' + $workspace + '" && "' + $cargo + '" build --release --locked --offline --no-default-features --target ' + $target
    & cmd.exe /d /s /c $buildCommand
    if ($LASTEXITCODE -ne 0) {
        throw "Production vosu build failed with exit code $LASTEXITCODE"
    }

    $builtExecutable = Join-Path $targetDirectory "$target\release\vosu.exe"
    Assert-Leaf $builtExecutable 'Built production executable'

    if (Test-Path -LiteralPath $runDirectory) {
        Remove-Item -LiteralPath $runDirectory -Recurse -Force
    }
    New-Item -ItemType Directory -Path $runDirectory -Force | Out-Null
    $runExecutable = Join-Path $runDirectory 'vosu.exe'
    Copy-Item -LiteralPath $builtExecutable -Destination $runExecutable
    $config = @'
ENABLE_AUTOUPDATE = false
OPEN_DASHBOARD_ON_STARTUP = false
DEBUG_LOG = false
SERVER_IP = "127.0.0.1"
SERVER_PORT = 24050
'@
    [IO.File]::WriteAllText((Join-Path $runDirectory 'vosu.toml'), $config, (New-Object Text.UTF8Encoding($false)))

    $daemonStdout = Join-Path $runDirectory 'daemon.stdout.log'
    $daemonStderr = Join-Path $runDirectory 'daemon.stderr.log'
    $daemon = Start-Process -FilePath $runExecutable -ArgumentList '--update=false' -WorkingDirectory $runDirectory -WindowStyle Hidden -RedirectStandardOutput $daemonStdout -RedirectStandardError $daemonStderr -PassThru

    $readinessTimer = [Diagnostics.Stopwatch]::StartNew()
    $ready = $false
    while ($readinessTimer.Elapsed.TotalSeconds -lt 60.0) {
        $daemon.Refresh()
        if ($daemon.HasExited) {
            $startupError = if (Test-Path -LiteralPath $daemonStderr) { (Get-Content -LiteralPath $daemonStderr -Raw).Trim() } else { '' }
            throw "vosu exited before readiness with code $($daemon.ExitCode): $startupError"
        }
        if (Test-DaemonReady) {
            $ready = $true
            break
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not $ready) {
        $startupError = if (Test-Path -LiteralPath $daemonStderr) { (Get-Content -LiteralPath $daemonStderr -Raw).Trim() } else { '' }
        throw "vosu did not make /json/v2 ready within 60 seconds: $startupError"
    }

    for ($second = 0; $second -lt $warmupSeconds; $second++) {
        Start-Sleep -Seconds 1
        $daemon.Refresh()
        if ($daemon.HasExited) {
            throw "vosu exited during warmup with code $($daemon.ExitCode)"
        }
    }

    $cycleSamples = New-Object 'double[]' $sampleCount
    $privateSamples = New-Object 'double[]' $sampleCount
    $daemon.Refresh()
    $aggregateStartTicks = $daemon.TotalProcessorTime.Ticks
    $aggregateTimer = [Diagnostics.Stopwatch]::StartNew()
    for ($index = 0; $index -lt $sampleCount; $index++) {
        $daemon.Refresh()
        if ($daemon.HasExited) {
            throw "vosu exited before sample $($index + 1)"
        }
        $beforeCycles = [ProcessCycleCounter]::Read($daemon.Handle)
        $timer = [Diagnostics.Stopwatch]::StartNew()
        Start-Sleep -Seconds 1
        $timer.Stop()
        $daemon.Refresh()
        if ($daemon.HasExited) {
            throw "vosu exited during sample $($index + 1)"
        }
        $cycleDelta = [ProcessCycleCounter]::Read($daemon.Handle) - $beforeCycles
        if ($cycleDelta -le 0 -or $timer.Elapsed.TotalSeconds -le 0.0) {
            throw "Invalid process-cycle observation in sample $($index + 1)"
        }
        $cycleSamples[$index] = [double]$cycleDelta / $timer.Elapsed.TotalSeconds
        $privateSamples[$index] = [double]$daemon.PrivateMemorySize64
    }
    $aggregateTimer.Stop()
    $processorTicks = $daemon.TotalProcessorTime.Ticks - $aggregateStartTicks
    if ($processorTicks -le 0 -or $aggregateTimer.Elapsed.TotalSeconds -le 0.0) {
        throw 'Invalid aggregate processor-time observation'
    }

    $idleCycles = Get-Median $cycleSamples
    $idleCpu = ([double]$processorTicks * 100.0) / $aggregateTimer.Elapsed.TotalSeconds
    $idlePrivateBytes = Get-Median $privateSamples
    Assert-PositiveFinite $idleCycles 'windows_idle_cpu_cycles_per_s'
    Assert-PositiveFinite $idleCpu 'windows_idle_cpu_ns_per_s'
    Assert-PositiveFinite $idlePrivateBytes 'windows_idle_private_bytes'

    Stop-Daemon

    $memorySource = Join-Path $workspace 'tools\autoresearch\windows-memory-bench.rs'
    Assert-Leaf $memorySource 'Windows memory benchmark source'
    New-Item -ItemType Directory -Path $memoryOutputDirectory -Force | Out-Null
    $memoryExecutable = Join-Path $memoryOutputDirectory 'windows-memory-bench.exe'
    $memoryCompileCommand = 'call "' + $vcvars + '" >nul && "' + $rustc + '" --edition=2024 "' + $memorySource + '" -C opt-level=3 -C lto=thin -C codegen-units=1 -C panic=abort -o "' + $memoryExecutable + '"'
    & cmd.exe /d /s /c $memoryCompileCommand
    if ($LASTEXITCODE -ne 0) {
        throw "Windows memory benchmark compilation failed with exit code $LASTEXITCODE"
    }

    $memoryStdout = Join-Path $memoryOutputDirectory 'windows-memory-bench.stdout.log'
    $memoryStderr = Join-Path $memoryOutputDirectory 'windows-memory-bench.stderr.log'
    Remove-Item -LiteralPath $memoryStdout, $memoryStderr -Force -ErrorAction SilentlyContinue
    $memoryProcess = Start-Process -FilePath $memoryExecutable -WorkingDirectory $workspace -RedirectStandardOutput $memoryStdout -RedirectStandardError $memoryStderr -Wait -PassThru
    if ($memoryProcess.ExitCode -ne 0) {
        $memoryError = if (Test-Path -LiteralPath $memoryStderr) { (Get-Content -LiteralPath $memoryStderr -Raw).Trim() } else { '' }
        throw "Windows memory benchmark failed with exit code $($memoryProcess.ExitCode): $memoryError"
    }

    $memoryNames = @(
        'windows_read_many_gapped_ns',
        'windows_read_exact_ns',
        'windows_read_many_single_ns',
        'windows_read_many_contiguous_ns'
    )
    $memoryValues = @{}
    foreach ($line in @(Get-Content -LiteralPath $memoryStdout)) {
        if ($line -notmatch '^METRIC\s+([^=]+)=(.+)$') {
            continue
        }
        $name = $matches[1]
        $rawValue = $matches[2]
        if ($memoryNames -notcontains $name) {
            throw "Windows memory benchmark emitted unexpected metric '$name'"
        }
        if ($memoryValues.ContainsKey($name)) {
            throw "Windows memory benchmark emitted duplicate metric '$name'"
        }
        $value = 0.0
        if (-not [double]::TryParse($rawValue, [Globalization.NumberStyles]::Float, $invariant, [ref]$value)) {
            throw "Windows memory benchmark emitted invalid value for ${name}: '$rawValue'"
        }
        Assert-PositiveFinite $value $name
        $memoryValues[$name] = $value
    }
    foreach ($name in $memoryNames) {
        if (-not $memoryValues.ContainsKey($name)) {
            throw "Windows memory benchmark did not emit $name"
        }
    }

    $logSum = 0.0
    foreach ($name in $memoryNames) {
        $logSum += [Math]::Log([double]$memoryValues[$name])
    }
    $memoryGeomean = [Math]::Exp($logSum / $memoryNames.Count)
    Assert-PositiveFinite $memoryGeomean 'windows_memory_geomean_ns'

    [Console]::Out.WriteLine('METRIC windows_idle_cpu_cycles_per_s=' + (Format-Metric $idleCycles))
    [Console]::Out.WriteLine('METRIC windows_idle_cpu_ns_per_s=' + (Format-Metric $idleCpu))
    [Console]::Out.WriteLine('METRIC windows_idle_private_bytes=' + (Format-Metric $idlePrivateBytes))
    [Console]::Out.WriteLine('METRIC windows_memory_geomean_ns=' + (Format-Metric $memoryGeomean))
    foreach ($name in $memoryNames) {
        [Console]::Out.WriteLine('METRIC ' + $name + '=' + (Format-Metric ([double]$memoryValues[$name])))
    }
}
catch {
    $failure = $_
}
finally {
    try {
        Stop-Daemon
    }
    catch {
        if ($null -eq $failure) {
            $failure = $_
        }
        else {
            [Console]::Error.WriteLine(('windows-optimization-bench cleanup: ' + $_.Exception.Message))
        }
    }
}

if ($null -ne $failure) {
    [Console]::Error.WriteLine(('windows-optimization-bench: ' + $failure.Exception.Message))
    exit 1
}
