# Shared native Windows CI environment setup. Dot-sourced by
# scripts/native-ci.ps1 (the prepush gate's native phase) and
# scripts/vm-test.ps1 (the ad-hoc native test lever), so both configure the
# pinned toolchain, Cargo, and debuginfo settings identically. Keep env
# behavior here in sync with ci.yml's windows-native job; a change here
# affects both callers.

function Import-VosuNativeToolchain {
    <#
    .SYNOPSIS
    Imports the pinned MSVC build environment into the current process and
    puts the pinned Rust toolchain first on PATH. Mutates $env:*; callers run
    once per process before building. Returns the cargo.exe path.
    #>
    $vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
    $devEnvironment = & cmd.exe /d /c ('call "' + $vcvars + '" >nul && set')
    if ($LASTEXITCODE -ne 0) { throw 'vcvars64 import failed.' }
    foreach ($line in $devEnvironment) {
        if ($line -match '^([^=][^=]*)=(.*)$') {
            Set-Item -Path ('Env:\' + $matches[1]) -Value $matches[2]
        }
    }
    $rust = 'C:\Program Files\Rust\1.96.1\bin'
    # cargo-nextest.exe is pinned and installed once into this VM's CARGO_HOME
    # (see CONTRIBUTING.md "Install the native build dependencies"), not
    # reinstalled per run; cargo resolves it as a subcommand from PATH.
    $env:PATH = $rust + ';' + (Join-Path 'V:\agent-desktop\.cargo' 'bin') + ';' + $env:PATH
    Remove-Item Env:\C_INCLUDE_PATH -ErrorAction SilentlyContinue
    return (Join-Path $rust 'cargo.exe')
}

function Set-VosuCargoWorkEnv {
    <#
    .SYNOPSIS
    Points Cargo at a per-invocation target dir and TEMP/TMP under
    $WorkRoot, while leaving $CargoHome (the registry index/download/package
    cache) shared across concurrent invocations.
    .NOTES
    Sharing CARGO_HOME across concurrent cargo processes is safe as long as
    each has its own CARGO_TARGET_DIR (set here): Cargo takes its own
    advisory locks around registry index updates and package-cache
    extraction, so concurrent processes serialize only that shared,
    read-mostly download step and never race on build output, which stays
    per-invocation. See AGENTS.md "Verification by change" (native lever)
    and scripts/test_vm_test.py for the argument/command-construction tests.
    .PARAMETER Jobs
    Caps CARGO_BUILD_JOBS so one invocation cannot claim every vCPU; 0 (the
    default for scripts/native-ci.ps1's full gate) leaves Cargo's default
    (all logical processors).
    #>
    param(
        [Parameter(Mandatory = $true)][string]$WorkRoot,
        [string]$CargoHome = 'V:\agent-desktop\.cargo',
        [int]$Jobs = 0
    )
    $cargoTarget = Join-Path $WorkRoot 'target'
    New-Item -ItemType Directory -Force -Path $cargoTarget | Out-Null
    $env:CARGO_TARGET_DIR = $cargoTarget
    $env:CARGO_HOME = $CargoHome
    $env:CARGO_INCREMENTAL = '0'
    # Matches ci.yml's windows-native job: full PDB debuginfo scales badly with
    # workspace crate count (M0 added ~8 crates and regressed the hosted job by
    # 186s); line tables keep panic backtraces readable. See
    # docs/testing/benchmarks.md for the measurement.
    $env:CARGO_PROFILE_DEV_DEBUG = 'line-tables-only'
    if ($Jobs -gt 0) {
        $env:CARGO_BUILD_JOBS = [string]$Jobs
    } else {
        Remove-Item Env:\CARGO_BUILD_JOBS -ErrorAction SilentlyContinue
    }
    $tmp = Join-Path $WorkRoot 'tmp'
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    $env:TEMP = $tmp
    $env:TMP = $tmp
    return $cargoTarget
}

function Invoke-VosuCargoStep {
    <#
    .SYNOPSIS
    Runs one cargo invocation, capturing stdout/stderr to
    "$LogDir/$Name.std{out,err}.log" and appending a
    {step, exitCode, wallSeconds, timedOut} entry to $Steps.
    .PARAMETER TimeoutSeconds
    When > 0 and the process has not exited by the deadline, kills the whole
    process tree (taskkill /T /F), not just the direct child: cargo spawns
    rustc/linker children that Process.Kill() alone would leave running.
    #>
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$CargoExe,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string]$LogDir,
        [Parameter(Mandatory = $true)]$Steps,
        [int]$TimeoutSeconds = 0
    )
    $prefix = Join-Path $LogDir $Name
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $CargoExe
    $info.Arguments = $Arguments -join ' '
    $info.WorkingDirectory = $WorkingDirectory
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $info
    if (-not $process.Start()) { throw "Could not start step $Name" }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $timedOut = $false
    if ($TimeoutSeconds -gt 0 -and -not $process.WaitForExit($TimeoutSeconds * 1000)) {
        $timedOut = $true
        & taskkill.exe /PID $process.Id /T /F | Out-Null
        $process.WaitForExit()
    } else {
        $process.WaitForExit()
    }
    [IO.File]::WriteAllText($prefix + '.stdout.log', $stdout.GetAwaiter().GetResult())
    [IO.File]::WriteAllText($prefix + '.stderr.log', $stderr.GetAwaiter().GetResult())
    $watch.Stop()
    $entry = [ordered]@{
        step        = $Name
        exitCode    = $process.ExitCode
        wallSeconds = $watch.Elapsed.TotalSeconds
        timedOut    = $timedOut
        stdout      = $prefix + '.stdout.log'
        stderr      = $prefix + '.stderr.log'
    }
    [void]$Steps.Add([pscustomobject]$entry)
    $process.Dispose()
    $suffix = if ($timedOut) { ' (TIMED OUT, process tree killed)' } else { '' }
    Write-Host ('{0}: exit {1}, {2:N2}s{3}' -f $Name, $entry.exitCode, $entry.wallSeconds, $suffix)
    return $entry.exitCode
}

function Invoke-VosuPluginHostFixtures {
    <#
    .SYNOPSIS
    Runs scripts\plugin-host-fixtures.py (the same helper ci.yml and the Linux
    gate run) as one logged step named 'plugin-host-fixtures', then imports
    the two fixture library variables it wrote into this process, so the
    cargo test that follows finds them. Returns the helper's exit code.
    .PARAMETER Target
    Must match the test build's own --target, so the libraries come from the
    same target directory layout.
    .PARAMETER ScratchRoot
    A directory Defender's real-time scanning excludes (the VM's
    V:\agent-desktop\tmp). Each plugin-host test stages its own fresh copy of
    a fixture library; in an unexcluded directory the on-access scan of every
    copy (measured 1.6-3 s for 194 MB, 0.2 s for 0.8 MB, against about 50 ms
    excluded) runs inside the 5-second startup deadline under parallel load.
    A unique subdirectory is exported as VOSU_PLUGIN_HOST_SCRATCH_DIR;
    remove it with Remove-VosuPluginHostScratch.
    .PARAMETER ReleaseDaemon
    Also build and stage the release daemon (no test-harness) the
    plugin_host_native approval module runs, exported as
    VOSU_PLUGIN_APPROVAL_DAEMON (plugin-host-fixtures.py --release-daemon).
    #>
    param(
        [Parameter(Mandatory = $true)][string]$CargoExe,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory,
        [Parameter(Mandatory = $true)][string]$LogDir,
        [Parameter(Mandatory = $true)]$Steps,
        [string]$Target,
        [string]$ScratchRoot,
        [int]$TimeoutSeconds = 0,
        [switch]$ReleaseDaemon
    )
    $python = (Get-Command python.exe -ErrorAction Stop).Source
    $envFile = Join-Path $LogDir 'plugin-host-fixtures.env'
    Remove-Item $envFile -ErrorAction SilentlyContinue
    $arguments = @('scripts\plugin-host-fixtures.py')
    if ($Target) { $arguments += @('--target', $Target) }
    if ($ReleaseDaemon) { $arguments += @('--release-daemon') }
    $arguments += @('--env-file', $envFile)
    $env:CARGO = $CargoExe
    $exitCode = Invoke-VosuCargoStep -Name 'plugin-host-fixtures' -CargoExe $python `
        -WorkingDirectory $WorkingDirectory -Arguments $arguments -LogDir $LogDir -Steps $Steps `
        -TimeoutSeconds $TimeoutSeconds
    if ($exitCode -eq 0) {
        if ($ScratchRoot) {
            $scratch = Join-Path $ScratchRoot ('plugin-host-' + [guid]::NewGuid().ToString('N'))
            New-Item -ItemType Directory -Force -Path $scratch | Out-Null
            $env:VOSU_PLUGIN_HOST_SCRATCH_DIR = $scratch
        }
        foreach ($line in Get-Content $envFile) {
            $name, $value = $line -split '=', 2
            Set-Item -Path ('Env:\' + $name) -Value $value
        }
    }
    return $exitCode
}

function Remove-VosuPluginHostScratch {
    <#
    .SYNOPSIS
    Best-effort removal of the scratch directory Invoke-VosuPluginHostFixtures
    created, if any.
    #>
    if ($env:VOSU_PLUGIN_HOST_SCRATCH_DIR) {
        Remove-Item -Recurse -Force $env:VOSU_PLUGIN_HOST_SCRATCH_DIR -ErrorAction SilentlyContinue
    }
}
