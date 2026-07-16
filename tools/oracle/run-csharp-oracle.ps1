param(
    [Parameter(Mandatory = $true)][string]$OsuSource,
    [Parameter(Mandatory = $true)][string]$Dotnet,
    [Parameter(Mandatory = $true)][string]$Output
)

$ErrorActionPreference = 'Stop'
$gitMetadata = Join-Path $OsuSource '.git'
$gitDirectory = if (Test-Path $gitMetadata -PathType Leaf) {
    $pointer = (Get-Content -Raw $gitMetadata).Trim()
    if (-not $pointer.StartsWith('gitdir: ')) { throw "malformed .git pointer: $gitMetadata" }
    [System.IO.Path]::GetFullPath((Join-Path $OsuSource $pointer.Substring(8)))
} else {
    $gitMetadata
}
$head = (Get-Content -Raw (Join-Path $gitDirectory 'HEAD')).Trim()
$revision = if ($head.StartsWith('ref: ')) {
    (Get-Content -Raw (Join-Path $gitDirectory $head.Substring(5))).Trim()
} else {
    $head
}
if ($revision -ne '97e94babde9d5ecbe574ecdd33509db27452cd0f') {
    throw "wrong osu! source revision: $revision"
}

$tests = @(
    @{ Project = 'osu.Game.Rulesets.Osu.Tests/osu.Game.Rulesets.Osu.Tests.csproj'; Filter = 'FullyQualifiedName~OsuScoreMultiplierTest' },
    @{ Project = 'osu.Game.Rulesets.Taiko.Tests/osu.Game.Rulesets.Taiko.Tests.csproj'; Filter = 'FullyQualifiedName~TaikoScoreMultiplierTest|FullyQualifiedName~TaikoBeatmapConversionTest' },
    @{ Project = 'osu.Game.Rulesets.Catch.Tests/osu.Game.Rulesets.Catch.Tests.csproj'; Filter = 'FullyQualifiedName~CatchScoreMultiplierTest|FullyQualifiedName~CatchBeatmapConversionTest' },
    @{ Project = 'osu.Game.Rulesets.Mania.Tests/osu.Game.Rulesets.Mania.Tests.csproj'; Filter = 'FullyQualifiedName~ManiaScoreMultiplierTest|FullyQualifiedName~ManiaBeatmapConversionTest' }
)

$results = @()
foreach ($test in $tests) {
    $project = Join-Path $OsuSource $test.Project
    $text = & $Dotnet test $project --configuration Release --filter $test.Filter --logger 'console;verbosity=normal' 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) {
        throw "C# oracle test failed: $($test.Project)`n$text"
    }
    if ($text -notmatch 'Test Run Successful\.') {
        throw "C# oracle test did not report exact success marker: $($test.Project)`n$text"
    }
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($text)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha256.ComputeHash($bytes)
    } finally {
        $sha256.Dispose()
    }
    $hash = [BitConverter]::ToString($digest).Replace('-', '').ToLowerInvariant()
    $results += [ordered]@{ project = $test.Project; filter = $test.Filter; stdoutSha256 = $hash }
}

$evidence = [ordered]@{
    schema = 1
    osuRevision = $revision
    dotnetVersion = (& $Dotnet --version).Trim()
    tests = $results
}
$directory = Split-Path -Parent $Output
New-Item -ItemType Directory -Force -Path $directory | Out-Null
$json = $evidence | ConvertTo-Json -Depth 5
[System.IO.File]::WriteAllText($Output, $json, [System.Text.UTF8Encoding]::new($false))
if (-not (Test-Path -LiteralPath $Output) -or (Get-Item -LiteralPath $Output).Length -eq 0) {
    throw "Failed to persist C# oracle evidence at $Output"
}
Write-Host "Wrote C# oracle evidence: $Output"
