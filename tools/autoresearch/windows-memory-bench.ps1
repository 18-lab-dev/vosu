#requires -Version 5.1
[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$ConfirmPreference = 'None'

$workspace = 'C:\Users\vosu-agent\vosu-autoresearch'
$rustc = 'C:\Rust\toolchain\bin\rustc.exe'
$vcvars = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
$outputDirectory = 'C:\vosu-autoresearch-bin'
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

    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
    $commandLine = 'call "' + $vcvars + '" >nul && "' + $rustc + '"' +
        ' --edition=2024 "' + $source + '"' +
        ' -C opt-level=3 -C lto=thin -C codegen-units=1 -C panic=abort' +
        ' -o "' + $executable + '"'

    & cmd.exe /d /s /c $commandLine
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
