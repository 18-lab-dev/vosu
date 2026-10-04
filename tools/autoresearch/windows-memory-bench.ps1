#requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$ConfirmPreference = 'None'

$workspace = 'V:\agent-workspaces\vosu-autoresearch'
$rustc = 'C:\Program Files\Rust\1.96.1\bin\rustc.exe'
$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
$outputDirectory = 'V:\agent-workspaces\vosu-autoresearch-bin'
$source = Join-Path -Path $workspace -ChildPath 'tools\autoresearch\windows-memory-bench.rs'
$executable = Join-Path -Path $outputDirectory -ChildPath 'windows-memory-bench.exe'

try {
    if (-not (Test-Path -LiteralPath $workspace -PathType Container)) {
        throw "Remote benchmark workspace does not exist: $workspace"
    }
    if (-not (Test-Path -LiteralPath $rustc -PathType Leaf)) {
        throw "Pinned rustc does not exist: $rustc"
    }
    if (-not (Test-Path -LiteralPath $vcvars -PathType Leaf)) {
        throw "MSVC environment script does not exist: $vcvars"
    }
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "Benchmark source does not exist: $source"
    }

    # OpenSSH service environments can preserve quotes inside PATH entries.
    # VsDevCmd parses PATH in batch conditionals and fails on those stray quotes.
    $env:Path = $env:Path.Replace('"', '')
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
    $buildScript = Join-Path -Path $outputDirectory -ChildPath 'build-windows-memory-bench.cmd'
    $buildLines = @(
        '@echo off',
        ('call "' + $vcvars + '" >nul'),
        'if errorlevel 1 exit /b 1',
        ('"' + $rustc + '" --edition=2024 "' + $source + '"' +
            ' -C opt-level=3 -C lto=thin -C codegen-units=1 -C panic=abort' +
            ' -o "' + $executable + '"')
    )
    [IO.File]::WriteAllLines($buildScript, $buildLines, [Text.Encoding]::ASCII)

    & cmd.exe /d /c $buildScript
    $compileExitCode = $LASTEXITCODE
    if ($compileExitCode -ne 0) {
        exit $compileExitCode
    }

    & $executable
    $runExitCode = $LASTEXITCODE
    if ($runExitCode -ne 0) {
        exit $runExitCode
    }
}
catch {
    [Console]::Error.WriteLine(('windows-memory-bench: ' + $_.Exception.Message))
    exit 1
}
