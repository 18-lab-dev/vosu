param(
    [Parameter(Mandatory = $true)]
    [string]$Binary,

    [Parameter(Mandatory = $true)]
    [string]$Tag,

    # Common name of the certificate SignPath Foundation issues for open-source
    # projects; any other valid signer means the wrong certificate was used.
    [string]$ExpectedSigner = "SignPath Foundation",

    # Releases are unsigned until SignPath signing is enabled; the workflow
    # passes this only for signed runs, which must then pass every check below.
    [switch]$RequireSignature
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ($Tag -notmatch '^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$') {
    throw "Release tag must be v<major>.<minor>.<patch>: $Tag"
}

$binaryPath = (Resolve-Path -LiteralPath $Binary).Path
$expectedVersion = $Tag.Substring(1) + ".0"
$expectedVersionParts = [System.Version]::Parse($expectedVersion)

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class VosuWindowsResources
{
    [DllImport("version.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern uint GetFileVersionInfoSizeW(string fileName, out uint handle);

    [DllImport("version.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool GetFileVersionInfoW(
        string fileName, uint handle, uint length, [Out] byte[] data);

    [DllImport("version.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool VerQueryValueW(
        byte[] block, string subBlock, out IntPtr buffer, out uint length);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    public static extern uint ExtractIconExW(
        string fileName, int iconIndex, IntPtr[] largeIcons, IntPtr[] smallIcons, uint iconCount);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool DestroyIcon(IntPtr icon);
}
'@

$versionInfo = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($binaryPath)
# Issue #6: match the approved copyright and empty company field in build.rs.
$expectedFields = @{
    FileVersion = $expectedVersion
    ProductVersion = $expectedVersion
    ProductName = "vosu"
    CompanyName = ""
    LegalCopyright = 'Copyright (c) 2026 18-LAB'
    FileDescription = "vosu Rust daemon"
}
foreach ($field in $expectedFields.Keys) {
    $actual = $versionInfo.$field
    if ($actual -ne $expectedFields[$field]) {
        throw "$field mismatch: expected '$($expectedFields[$field])', got '$actual'"
    }
}

$expectedNumericParts = @{
    FileMajorPart = $expectedVersionParts.Major
    FileMinorPart = $expectedVersionParts.Minor
    FileBuildPart = $expectedVersionParts.Build
    FilePrivatePart = $expectedVersionParts.Revision
    ProductMajorPart = $expectedVersionParts.Major
    ProductMinorPart = $expectedVersionParts.Minor
    ProductBuildPart = $expectedVersionParts.Build
    ProductPrivatePart = $expectedVersionParts.Revision
}
foreach ($field in $expectedNumericParts.Keys) {
    $actual = $versionInfo.$field
    if ($actual -ne $expectedNumericParts[$field]) {
        throw "$field mismatch: expected $($expectedNumericParts[$field]), got $actual"
    }
}

[uint32]$ignored = 0
$resourceSize = [VosuWindowsResources]::GetFileVersionInfoSizeW($binaryPath, [ref]$ignored)
if ($resourceSize -eq 0) {
    throw "Executable has no version resource"
}
$resource = [byte[]]::new($resourceSize)
if (-not [VosuWindowsResources]::GetFileVersionInfoW(
    $binaryPath, 0, $resourceSize, $resource)) {
    throw "Unable to read executable version resource"
}
[IntPtr]$translations = [IntPtr]::Zero
[uint32]$translationBytes = 0
if (-not [VosuWindowsResources]::VerQueryValueW(
    $resource, "\VarFileInfo\Translation", [ref]$translations, [ref]$translationBytes)) {
    throw "Version resource has no translation table"
}

$hasRequiredTranslation = $false
for ($offset = 0; $offset + 3 -lt $translationBytes; $offset += 4) {
    $language = [uint16][Runtime.InteropServices.Marshal]::ReadInt16($translations, $offset)
    $codepage = [uint16][Runtime.InteropServices.Marshal]::ReadInt16($translations, $offset + 2)
    if ($language -eq 1033 -and $codepage -eq 1200) {
        $hasRequiredTranslation = $true
        break
    }
}
if (-not $hasRequiredTranslation) {
    throw "Version resource lacks language 1033/codepage 1200"
}

$largeIcon = [IntPtr[]]::new(1)
$smallIcon = [IntPtr[]]::new(1)
$iconCount = [VosuWindowsResources]::ExtractIconExW(
    $binaryPath, 0, $largeIcon, $smallIcon, 1)
try {
    if ($iconCount -lt 1 -or
        ($largeIcon[0] -eq [IntPtr]::Zero -and $smallIcon[0] -eq [IntPtr]::Zero)) {
        throw "Executable has no extractable icon"
    }
}
finally {
    foreach ($icon in @($largeIcon[0], $smallIcon[0])) {
        if ($icon -ne [IntPtr]::Zero) {
            [void][VosuWindowsResources]::DestroyIcon($icon)
        }
    }
}

$signature = Get-AuthenticodeSignature -LiteralPath $binaryPath
if ($RequireSignature) {
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
        throw "Signed executable failed Authenticode validation: $($signature.Status)"
    }
    $signer = $signature.SignerCertificate.GetNameInfo(
        [System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
    if ($signer -ne $ExpectedSigner) {
        throw "Executable is signed by '$signer', expected '$ExpectedSigner'"
    }
    # Without a timestamp the signature stops validating when the certificate expires.
    if ($null -eq $signature.TimeStamperCertificate) {
        throw "Executable signature has no timestamp countersignature"
    }
}

Write-Output "Verified Windows resources for $binaryPath (signature: $($signature.Status))"
