[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$workspace = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
Set-Location $workspace
$buildTarget = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { 'target' }

$packageId = cargo pkgid -p gocode
$version = $packageId -replace '.*[#@]', ''
$archiveName = "gocode-$version-windows-x86_64.zip"
$staging = Join-Path ([System.IO.Path]::GetTempPath()) ("gocode-release-" + [guid]::NewGuid())

try {
    cargo build --release --locked -p gocode -p gocode-updater
    $payload = Join-Path $staging "gocode-$version-windows-x86_64"
    New-Item -ItemType Directory -Force -Path $payload | Out-Null
    New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
    $releaseDirectory = Join-Path $buildTarget 'release'
    Copy-Item (Join-Path $releaseDirectory 'gocode.exe') $payload/gocode.exe
    Copy-Item (Join-Path $releaseDirectory 'gocode-updater.exe') $payload/gocode-updater.exe
    Copy-Item LICENSE $payload/LICENSE
    Copy-Item docs/INSTALL.md $payload/INSTALL.md
    Copy-Item scripts/install-windows.ps1 $payload/install-windows.ps1
    Compress-Archive -Path (Join-Path $payload '*') -DestinationPath (Join-Path $OutputDirectory $archiveName) -Force
}
finally {
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $staging
}
